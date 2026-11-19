//! Native file dialogs (IFileOpenDialog / IFileSaveDialog) and "reveal in
//! Explorer", without any Tauri plugin. Each dialog runs on its own STA thread.

use std::path::{Path, PathBuf};
use std::thread;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::Common::{COMDLG_FILTERSPEC, ITEMIDLIST};
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog,
    FileSaveDialog, IFileDialog, IFileOpenDialog, IFileSaveDialog, ILFree,
    SHOpenFolderAndSelectItems, SHParseDisplayName, SIGDN_FILESYSPATH,
};
use windows::core::{HSTRING, PCWSTR};

use crate::error::{Error, Result};
use crate::project::{PROJECT_EXT, PROJECT_FILE};

/// HRESULT_FROM_WIN32(ERROR_CANCELLED).
const CANCELLED: i32 = 0x8007_04C7_u32 as i32;

fn on_sta<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    thread::Builder::new()
        .name("frostcap-dialog".into())
        .spawn(move || {
            // SAFETY: balanced below.
            let ok =
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
                    .is_ok();
            let r = f();
            if ok {
                // SAFETY: paired with CoInitializeEx.
                unsafe { CoUninitialize() };
            }
            r
        })
        .map_err(|e| Error::Invalid(e.to_string()))?
        .join()
        .map_err(|_| Error::Invalid("dialog thread panicked".into()))?
}

fn show(dialog: &IFileDialog, owner: Option<isize>) -> Result<Option<PathBuf>> {
    let owner = owner.map(|h| HWND(h as *mut _));
    // SAFETY: modal dialog on this STA thread; result string freed with CoTaskMemFree.
    unsafe {
        if let Err(e) = dialog.Show(owner) {
            if e.code().0 == CANCELLED {
                return Ok(None);
            }
            return Err(e.into());
        }
        let item = dialog.GetResult()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let s = p.to_string();
        CoTaskMemFree(Some(p.0 as *const _));
        Ok(Some(PathBuf::from(
            s.map_err(|e| Error::Invalid(e.to_string()))?,
        )))
    }
}

/// Pick a folder. `None` if cancelled.
pub fn pick_folder(owner: Option<isize>, title: &str) -> Result<Option<PathBuf>> {
    let title = HSTRING::from(title);
    on_sta(move || {
        // SAFETY: standard dialog setup.
        unsafe {
            let d: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            d.SetOptions(
                d.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST,
            )?;
            d.SetTitle(&title)?;
            show(&d.into(), owner)
        }
    })
}

/// Pick a `*.frostcap` project folder (validated to contain `project.json`).
pub fn pick_project(owner: Option<isize>) -> Result<Option<PathBuf>> {
    let Some(dir) = pick_folder(owner, "Open Frostcap project (.frostcap folder)")? else {
        return Ok(None);
    };
    let dir = if dir.file_name().is_some_and(|n| n == PROJECT_FILE) {
        dir.parent().map(Path::to_path_buf).unwrap_or(dir)
    } else {
        dir
    };
    if dir.extension().is_none_or(|e| e != PROJECT_EXT) || !dir.join(PROJECT_FILE).is_file() {
        return Err(Error::InvalidPath(format!(
            "{} is not a .{PROJECT_EXT} project folder",
            dir.display()
        )));
    }
    Ok(Some(dir))
}

/// Pick a save path with `default_name` and a single extension filter (`ext`
/// without dot). `None` if cancelled. The extension is enforced.
pub fn pick_save_path(
    owner: Option<isize>,
    default_name: &str,
    ext: &str,
) -> Result<Option<PathBuf>> {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    if ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric()) || ext.len() > 8 {
        return Err(Error::InvalidPath(format!("bad extension {ext:?}")));
    }
    let name = HSTRING::from(default_name);
    on_sta(move || {
        let label = HSTRING::from(format!("{} file (*.{ext})", ext.to_ascii_uppercase()));
        let spec = HSTRING::from(format!("*.{ext}"));
        let def = HSTRING::from(ext.as_str());
        // SAFETY: standard dialog setup; filter strings outlive the call.
        let picked = unsafe {
            let d: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
            d.SetOptions(
                d.GetOptions()? | FOS_OVERWRITEPROMPT | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST,
            )?;
            let filters = [COMDLG_FILTERSPEC {
                pszName: PCWSTR(label.as_ptr()),
                pszSpec: PCWSTR(spec.as_ptr()),
            }];
            d.SetFileTypes(&filters)?;
            d.SetDefaultExtension(&def)?;
            d.SetFileName(&name)?;
            show(&d.into(), owner)?
        };
        // Return exactly what the user confirmed (the dialog applied the default
        // extension and its overwrite prompt covered that name). A different
        // extension typed by the user is rejected, never silently rewritten.
        match picked {
            Some(p) if !p.extension().is_some_and(|e| e.eq_ignore_ascii_case(&ext)) => Err(
                Error::InvalidPath(format!("{} is not a .{ext} file", p.display())),
            ),
            other => Ok(other),
        }
    })
}

/// Open Explorer with `path` selected (or the folder itself if it is a dir).
pub fn reveal_in_folder(path: &Path) -> Result<()> {
    let path = path.canonicalize().map_err(|e| Error::io(path, e))?;
    let s = path
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_owned();
    on_sta(move || {
        // SAFETY: PIDL from SHParseDisplayName freed with ILFree.
        unsafe {
            let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
            SHParseDisplayName(&HSTRING::from(s), None, &mut pidl, 0, None)?;
            let r = SHOpenFolderAndSelectItems(pidl, None, 0);
            ILFree(Some(pidl));
            r?;
        }
        Ok(())
    })
}
