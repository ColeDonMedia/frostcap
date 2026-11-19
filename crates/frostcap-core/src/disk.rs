//! Free-space checks.

use std::path::Path;

use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::core::HSTRING;

use crate::error::{Error, Result};

/// Refuse to start below this much free space.
pub const MIN_FREE_TO_START: u64 = 1024 * 1024 * 1024;
/// Auto-stop a running recording below this much free space.
pub const MIN_FREE_WHILE_RECORDING: u64 = 300 * 1024 * 1024;

/// Bytes available to the caller on the volume holding `path`.
pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut avail = 0u64;
    // SAFETY: valid string and out param.
    unsafe {
        GetDiskFreeSpaceExW(
            &HSTRING::from(path.as_os_str()),
            Some(&mut avail),
            None,
            None,
        )
    }
    .ok()?;
    Some(avail)
}

/// Error with code `disk_full` if `path`'s volume has less than `min` bytes free.
pub fn ensure_free(path: &Path, min: u64) -> Result<()> {
    match free_bytes(path) {
        Some(f) if f < min => Err(Error::DiskFull(format!(
            "only {} MB free on the recording drive (need {} MB)",
            f / (1024 * 1024),
            min / (1024 * 1024)
        ))),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_free_space() {
        let here = std::env::current_dir().unwrap();
        assert!(free_bytes(&here).unwrap() > 0);
        assert!(ensure_free(&here, 1).is_ok());
        assert!(ensure_free(&here, u64::MAX).is_err());
    }
}

/// The real path of an open file (symlinks, junctions and 8.3 names resolved), asked
/// from the handle itself so it cannot differ from what was opened. The `\?` prefix
/// is removed.
pub fn final_path(file: &std::fs::File) -> std::io::Result<std::path::PathBuf> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GETFINALPATHNAMEBYHANDLE_FLAGS, GetFinalPathNameByHandleW,
    };
    let h = HANDLE(file.as_raw_handle());
    let mut buf = vec![0u16; 1024];
    loop {
        // SAFETY: valid handle and buffer; returns the needed length when too small.
        let n = unsafe { GetFinalPathNameByHandleW(h, &mut buf, GETFINALPATHNAMEBYHANDLE_FLAGS(0)) }
            as usize;
        if n == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if n < buf.len() {
            let s = String::from_utf16_lossy(&buf[..n]);
            let s = match s.strip_prefix(r"\\?\") {
                Some(rest) if !rest.starts_with("UNC\\") => rest.to_owned(),
                _ => s,
            };
            return Ok(s.into());
        }
        if n > 32_768 {
            return Err(std::io::Error::other("path too long"));
        }
        buf.resize(n + 1, 0);
    }
}

#[cfg(test)]
mod final_path_tests {
    use super::*;

    #[test]
    fn final_path_of_a_handle_is_the_real_path_without_the_verbatim_prefix() {
        let dir = crate::test_tmp_dir().join(format!("finalpath-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("Pic.PNG");
        std::fs::write(&p, b"x").unwrap();
        let f = std::fs::File::open(&p).unwrap();
        let got = final_path(&f).unwrap();
        let want = std::fs::canonicalize(&p).unwrap();
        let want = want.to_string_lossy();
        let want = want.strip_prefix(r"\\?\").unwrap_or(&want);
        assert!(!got.to_string_lossy().starts_with(r"\\?\"), "{got:?}");
        assert!(
            got.to_string_lossy().eq_ignore_ascii_case(want),
            "{got:?} vs {want}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
