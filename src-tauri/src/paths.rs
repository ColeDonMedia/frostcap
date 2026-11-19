//! Path policy for commands that touch the filesystem on behalf of the webview.
//!
//! A path from the UI is accepted only if, after canonicalization, it is
//! * a `*.frostcap` folder containing `project.json` (projects), or an `.mp4` /
//!   `.gif` file name (exports), and
//! * inside the projects root (`Videos\Frostcap`), or exactly a path the user
//!   picked in a native dialog this session, or (projects) one in the recent list.
//!
//! Exports may never target a file inside a project folder.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use frostcap_core::Error;
use frostcap_core::project::{PROJECT_EXT, PROJECT_FILE, project_dir_of};

/// Allowed export extensions.
pub const EXPORT_EXTS: [&str; 2] = ["mp4", "gif"];

fn canon(p: &Path) -> Result<PathBuf, Error> {
    dunce_like(
        p.canonicalize()
            .map_err(|e| Error::InvalidPath(format!("{}: {e}", p.display())))?,
    )
}

/// Strip the `\\?\` verbatim prefix for comparisons and display.
fn dunce_like(p: PathBuf) -> Result<PathBuf, Error> {
    let s = p.to_string_lossy();
    Ok(match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p,
    })
}

fn has_traversal(p: &Path) -> bool {
    p.components().any(|c| matches!(c, Component::ParentDir))
}

/// Session path policy.
#[derive(Debug)]
pub struct PathPolicy {
    root: PathBuf,
    picked: Mutex<HashSet<PathBuf>>,
    recent: Mutex<HashSet<PathBuf>>,
}

impl PathPolicy {
    /// Policy rooted at `root` (created if missing).
    pub fn new(root: &Path) -> Result<Self, Error> {
        std::fs::create_dir_all(root).map_err(|e| Error::io(root, e))?;
        Ok(Self {
            root: canon(root)?,
            picked: Mutex::default(),
            recent: Mutex::default(),
        })
    }

    /// Projects root (canonical).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Remember a path the user picked in a native dialog.
    pub fn allow_picked(&self, p: &Path) {
        let p = canon(p).unwrap_or_else(|_| p.to_path_buf());
        if let Ok(mut s) = self.picked.lock() {
            s.insert(p);
        }
    }

    /// Remember a save target the user picked (file may not exist yet).
    pub fn allow_picked_save(&self, p: &Path) {
        if let (Some(parent), Some(name)) = (p.parent(), p.file_name())
            && let Ok(parent) = canon(parent)
            && let Ok(mut s) = self.picked.lock()
        {
            s.insert(parent.join(name));
        }
    }

    /// Seed with folders from the recent list (already validated earlier).
    pub fn allow_recent(&self, dirs: impl IntoIterator<Item = PathBuf>) {
        if let Ok(mut s) = self.recent.lock() {
            s.extend(dirs.into_iter().filter_map(|d| canon(&d).ok()));
        }
    }

    fn is_listed(&self, p: &Path) -> bool {
        let picked = self.picked.lock().is_ok_and(|s| s.contains(p));
        picked || self.recent.lock().is_ok_and(|s| s.contains(p))
    }

    /// Existing path inside the projects root, or one the user picked / exported to.
    pub fn is_known(&self, p: &Path) -> bool {
        if has_traversal(p) {
            return false;
        }
        canon(p).is_ok_and(|c| c.starts_with(&self.root) || self.is_listed(&c))
    }

    /// Validate a project folder (or its `project.json`) and return the canonical folder.
    pub fn project_dir(&self, p: &Path) -> Result<PathBuf, Error> {
        if has_traversal(p) {
            return Err(Error::InvalidPath("'..' is not allowed".into()));
        }
        let dir = canon(&project_dir_of(p))?;
        if dir
            .extension()
            .is_none_or(|e| !e.eq_ignore_ascii_case(PROJECT_EXT))
            || !dir.is_dir()
        {
            return Err(Error::InvalidPath(format!(
                "{} is not a .{PROJECT_EXT} folder",
                dir.display()
            )));
        }
        if !dir.join(PROJECT_FILE).is_file() {
            return Err(Error::InvalidPath(format!(
                "{} has no {PROJECT_FILE}",
                dir.display()
            )));
        }
        if dir.starts_with(&self.root) || self.is_listed(&dir) {
            Ok(dir)
        } else {
            Err(Error::InvalidPath(format!(
                "{} is outside the projects folder; open it with the file picker first",
                dir.display()
            )))
        }
    }

    /// Validate an export target for `project_dir` and return the canonical path.
    pub fn export_target(&self, out: &Path, project_dir: &Path) -> Result<PathBuf, Error> {
        if has_traversal(out) {
            return Err(Error::InvalidPath("'..' is not allowed".into()));
        }
        let ext = out
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if !EXPORT_EXTS.contains(&ext.as_str()) {
            return Err(Error::InvalidPath(format!(
                "export must be .mp4 or .gif, got {:?}",
                out.display()
            )));
        }
        let (Some(parent), Some(name)) = (out.parent(), out.file_name()) else {
            return Err(Error::InvalidPath(
                "export path needs a folder and a file name".into(),
            ));
        };
        check_export_name(name)?;
        let target = canon(parent)?.join(name);
        if let Ok(pd) = canon(project_dir)
            && target.starts_with(&pd)
        {
            return Err(Error::InvalidPath(
                "cannot export into the project folder".into(),
            ));
        }
        if target.ancestors().any(|a| {
            a.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case(PROJECT_EXT))
        }) {
            return Err(Error::InvalidPath(
                "cannot export into a project folder".into(),
            ));
        }
        if target.starts_with(&self.root) || self.is_listed(&target) {
            Ok(target)
        } else {
            Err(Error::InvalidPath(
                "choose the export location with the save dialog".into(),
            ))
        }
    }
}

/// Largest self-test report we write.
pub const MAX_REPORT_BYTES: usize = 1 << 20;
/// Name prefix of the exports the UI self-test creates.
pub const SELFTEST_EXPORT_PREFIX: &str = "frostcap-selftest-";

/// Whether the process was started with the exact argument `--selftest`.
pub fn selftest_requested(args: impl IntoIterator<Item = std::ffi::OsString>) -> bool {
    args.into_iter().skip(1).any(|a| a == "--selftest")
}

/// Reserved Windows device names (with or without an extension).
fn is_reserved_device(stem: &str) -> bool {
    let u = stem.trim_end().to_ascii_uppercase();
    matches!(
        u.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((u.starts_with("COM") || u.starts_with("LPT"))
        && u.len() == 4
        && matches!(u.as_bytes()[3], b'1'..=b'9'))
}

/// An export file name must be a plain name: no `:` (NTFS alternate streams), no
/// wildcard or control characters, no trailing dot or space, no device names.
fn check_export_name(name: &std::ffi::OsStr) -> Result<(), Error> {
    let s = name.to_string_lossy();
    let stem = s.split('.').next().unwrap_or("");
    let bad = s.is_empty()
        || s.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
        || s.chars().any(char::is_control)
        || s.ends_with(['.', ' '])
        || is_reserved_device(stem);
    if bad {
        return Err(Error::InvalidPath(format!(
            "{s:?} is not a valid export file name"
        )));
    }
    Ok(())
}

/// Serialize a self-test report, refusing more than [`MAX_REPORT_BYTES`].
pub fn report_bytes(report: &str) -> Result<Vec<u8>, Error> {
    // Length first: an oversized payload is never parsed.
    if report.len() > MAX_REPORT_BYTES {
        return Err(Error::Invalid("self-test report is too large".into()));
    }
    serde_json::from_str::<serde_json::Value>(report)
        .map_err(|e| Error::Invalid(format!("self-test report is not JSON: {e}")))?;
    Ok(report.as_bytes().to_vec())
}

/// What a self-test cleanup may delete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deletable {
    /// A project folder created by this self-test run.
    Project,
    /// An exported `frostcap-selftest-*.mp4`.
    Export,
}

impl PathPolicy {
    /// Validate a self-test cleanup path: a project folder directly in the root that
    /// this run created (`created`, canonical), or `frostcap-selftest-*.mp4` directly
    /// in the root. Anything else is refused.
    pub fn selftest_target(
        &self,
        p: &Path,
        created: &HashSet<PathBuf>,
    ) -> Result<(PathBuf, Deletable), Error> {
        if has_traversal(p) {
            return Err(Error::InvalidPath("'..' is not allowed".into()));
        }
        let c = canon(p)?;
        if c.parent() != Some(self.root.as_path()) {
            return Err(Error::InvalidPath(
                "not directly inside the projects folder".into(),
            ));
        }
        let name = c
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if c.is_dir() && created.contains(&c) && name.ends_with(&format!(".{PROJECT_EXT}")) {
            return Ok((c, Deletable::Project));
        }
        if c.is_file()
            && created.contains(&c)
            && name.starts_with(SELFTEST_EXPORT_PREFIX)
            && name.ends_with(".mp4")
        {
            return Ok((c, Deletable::Export));
        }
        Err(Error::InvalidPath(format!(
            "{} is not something the self-test created (known projects: {})",
            c.display(),
            created.len()
        )))
    }
}

/// Largest background image accepted by `import_background`.
pub const MAX_IMPORT_BYTES: u64 = 20 << 20;
/// Most outputs of one `export_batch`.
pub const MAX_BATCH_JOBS: usize = 8;

impl PathPolicy {
    /// Open the source of `import_background` exactly once and validate **the open
    /// handle**: it must be a regular file of at most [`MAX_IMPORT_BYTES`], and its real
    /// final path (asked from the handle, so a swap after the check cannot matter) must
    /// be a `.png`/`.jpg`/`.jpeg` the user picked in a dialog or inside the projects
    /// root. Returns the file and its lower-case extension.
    pub fn import_open(&self, p: &Path) -> Result<(std::fs::File, String), Error> {
        if has_traversal(p) {
            return Err(Error::InvalidPath("'..' is not allowed".into()));
        }
        let ext_of = |p: &Path| {
            p.extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default()
        };
        let allowed = |e: &str| ["png", "jpg", "jpeg"].contains(&e);
        if !allowed(&ext_of(p)) {
            return Err(Error::InvalidPath(
                "background must be a .png or .jpg file".into(),
            ));
        }
        let file = std::fs::File::open(p).map_err(|e| Error::io(p, e))?;
        let meta = file.metadata().map_err(|e| Error::io(p, e))?;
        if !meta.is_file() {
            return Err(Error::InvalidPath("not a file".into()));
        }
        if meta.len() > MAX_IMPORT_BYTES {
            return Err(Error::InvalidPath("image is larger than 20 MiB".into()));
        }
        let real = frostcap_core::disk::final_path(&file).map_err(|e| Error::io(p, e))?;
        let ext = ext_of(&real);
        if !allowed(&ext) {
            return Err(Error::InvalidPath(
                "background must be a .png or .jpg file".into(),
            ));
        }
        if !self.is_known(&real) {
            return Err(Error::InvalidPath(
                "choose the image with the file dialog first".into(),
            ));
        }
        Ok((file, ext))
    }

    /// Validate every output of a batch with [`Self::export_target`]; also rejects
    /// an empty batch, more than [`MAX_BATCH_JOBS`] jobs and duplicate targets.
    pub fn batch_targets(
        &self,
        outs: &[PathBuf],
        project_dir: &Path,
    ) -> Result<Vec<PathBuf>, Error> {
        if outs.is_empty() || outs.len() > MAX_BATCH_JOBS {
            return Err(Error::Invalid(format!(
                "a batch needs 1..={MAX_BATCH_JOBS} jobs"
            )));
        }
        let mut seen = HashSet::new();
        let mut ok = Vec::new();
        for o in outs {
            let t = self.export_target(o, project_dir)?;
            if !seen.insert(t.to_string_lossy().to_ascii_lowercase()) {
                return Err(Error::Invalid("two jobs write the same file".into()));
            }
            ok.push(t);
        }
        Ok(ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let base = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../target"))
            .join("test-tmp")
            .join(format!("paths-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        // CARGO_MANIFEST_DIR/../target contains `..` (CI has no CARGO_TARGET_DIR) and the
        // policy rightly rejects lexical `..`, so hand out the canonical form.
        canon(&base).unwrap()
    }

    fn make_project(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(PROJECT_FILE), b"{}").unwrap();
    }

    #[test]
    fn projects_inside_root_are_accepted() {
        let base = tmp("inside");
        let root = base.join("Frostcap");
        let pol = PathPolicy::new(&root).unwrap();
        let p = root.join("Rec 1.frostcap");
        make_project(&p);
        assert_eq!(pol.project_dir(&p).unwrap(), canon(&p).unwrap());
        assert_eq!(
            pol.project_dir(&p.join(PROJECT_FILE)).unwrap(),
            canon(&p).unwrap()
        );
    }

    #[test]
    fn verbatim_and_mixed_case_forms_are_accepted() {
        let base = tmp("forms");
        let root = base.join("Frostcap");
        let pol = PathPolicy::new(&root).unwrap();
        let p = root.join("Rec.frostcap");
        make_project(&p);
        let verbatim = PathBuf::from(format!(r"\\?\{}", p.display()));
        assert_eq!(pol.project_dir(&verbatim).unwrap(), p);
        let upper = PathBuf::from(p.to_string_lossy().to_uppercase());
        assert_eq!(pol.project_dir(&upper).unwrap(), p);
    }

    #[test]
    fn rejects_outside_traversal_and_non_projects() {
        let base = tmp("reject");
        let root = base.join("Frostcap");
        let pol = PathPolicy::new(&root).unwrap();
        let outside = base.join("elsewhere.frostcap");
        make_project(&outside);
        assert!(
            pol.project_dir(&outside).is_err(),
            "outside root and not picked"
        );
        pol.allow_picked(&outside);
        assert!(pol.project_dir(&outside).is_ok(), "picked in a dialog");
        let plain = root.join("notaproject");
        fs::create_dir_all(&plain).unwrap();
        assert!(pol.project_dir(&plain).is_err());
        let empty = root.join("empty.frostcap");
        fs::create_dir_all(&empty).unwrap();
        assert!(pol.project_dir(&empty).is_err(), "no project.json");
        let sneaky = root.join("..").join("elsewhere.frostcap");
        assert!(matches!(
            pol.project_dir(&sneaky),
            Err(Error::InvalidPath(_))
        ));
        assert!(pol.project_dir(Path::new(r"C:\Windows")).is_err());
    }

    #[test]
    fn export_targets() {
        let base = tmp("export");
        let root = base.join("Frostcap");
        let pol = PathPolicy::new(&root).unwrap();
        let proj = root.join("a.frostcap");
        make_project(&proj);
        assert!(pol.export_target(&root.join("out.mp4"), &proj).is_ok());
        assert!(pol.export_target(&root.join("out.GIF"), &proj).is_ok());
        assert!(pol.export_target(&root.join("out.exe"), &proj).is_err());
        assert!(
            pol.export_target(&proj.join("screen.mp4"), &proj).is_err(),
            "inside project"
        );
        let other = base.join("Desktop");
        fs::create_dir_all(&other).unwrap();
        let picked = other.join("clip.mp4");
        assert!(pol.export_target(&picked, &proj).is_err(), "not picked yet");
        pol.allow_picked_save(&picked);
        assert!(pol.export_target(&picked, &proj).is_ok());
        assert!(
            pol.export_target(&other.join("other.mp4"), &proj).is_err(),
            "only the exact picked file"
        );
        assert!(
            pol.export_target(&root.join("..").join("x.mp4"), &proj)
                .is_err()
        );
        fs::write(&picked, b"x").unwrap();
        assert!(pol.is_known(&picked));
        assert!(pol.is_known(&proj));
        let stranger = other.join("secret.txt");
        fs::write(&stranger, b"x").unwrap();
        assert!(!pol.is_known(&stranger));
    }
}

#[cfg(test)]
mod selftest_tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target")
            .join("test-tmp")
            .join(format!("selftest-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        canon(&base).unwrap()
    }

    #[test]
    fn flag_is_only_enabled_by_an_exact_argument() {
        let args = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        assert!(selftest_requested(args(&["frostcap.exe", "--selftest"])));
        assert!(!selftest_requested(args(&["frostcap.exe"])));
        assert!(!selftest_requested(args(&[
            "frostcap.exe",
            "--selftest=1",
            "selftest",
            "-selftest"
        ])));
    }

    #[cfg(windows)]
    #[test]
    fn non_unicode_arguments_do_not_panic() {
        use std::os::windows::ffi::OsStringExt;
        let bad = std::ffi::OsString::from_wide(&[0x61, 0xD800, 0x62]); // lone surrogate
        let args = vec![
            std::ffi::OsString::from("frostcap.exe"),
            bad.clone(),
            std::ffi::OsString::from("--selftest"),
        ];
        assert!(selftest_requested(args));
        assert!(!selftest_requested(vec![
            std::ffi::OsString::from("frostcap.exe"),
            bad
        ]));
    }

    #[test]
    fn only_own_projects_and_selftest_exports_in_the_root_are_deletable() {
        let base = tmp("del");
        let root = base.join("Frostcap");
        let pol = PathPolicy::new(&root).unwrap();
        let proj = root.join("Recording 2026-11-18 10.00.00.frostcap");
        fs::create_dir_all(&proj).unwrap();
        let other = root.join("Recording 2026-11-18 11.00.00.frostcap");
        fs::create_dir_all(&other).unwrap();
        let export = root.join("frostcap-selftest-abc.mp4");
        fs::write(&export, b"x").unwrap();
        let user_clip = root.join("my-clip.mp4");
        fs::write(&user_clip, b"x").unwrap();
        let created: HashSet<PathBuf> = [canon(&proj).unwrap(), canon(&export).unwrap()].into();
        let unrecorded = root.join("frostcap-selftest-other.mp4");
        fs::write(&unrecorded, b"x").unwrap();

        assert_eq!(
            pol.selftest_target(&proj, &created).unwrap().1,
            Deletable::Project
        );
        assert_eq!(
            pol.selftest_target(&export, &created).unwrap().1,
            Deletable::Export
        );
        assert!(
            pol.selftest_target(&other, &created).is_err(),
            "not created by this run"
        );
        assert!(
            pol.selftest_target(&user_clip, &created).is_err(),
            "not a self-test file"
        );
        assert!(
            pol.selftest_target(&root, &created).is_err(),
            "the root itself"
        );
        assert!(
            pol.selftest_target(Path::new(r"C:\Windows"), &created)
                .is_err()
        );
        let sneaky = root
            .join("..")
            .join("Frostcap")
            .join("frostcap-selftest-abc.mp4");
        assert!(
            pol.selftest_target(&sneaky, &created).is_err(),
            "no traversal"
        );
        let outside = base.join("frostcap-selftest-x.mp4");
        fs::write(&outside, b"x").unwrap();
        assert!(
            pol.selftest_target(&outside, &created).is_err(),
            "outside the root"
        );
    }

    #[test]
    fn export_names_cannot_smuggle_streams_or_device_names() {
        let base = tmp("names");
        let root = base.join("Frostcap");
        let pol = PathPolicy::new(&root).unwrap();
        let proj = root.join("a.frostcap");
        make_project_dir(&proj);
        for bad in [
            "Rec 1.frostcap:x.mp4",
            "a:b.mp4",
            "clip.mp4.",
            "clip .mp4 ",
            "CON.mp4",
            "nul.gif",
            "COM1.mp4",
            "lpt9.mp4",
            "a\u{1}b.mp4",
            "a*b.mp4",
            "a?b.mp4",
        ] {
            assert!(
                pol.export_target(&root.join(bad), &proj).is_err(),
                "{bad:?} must be refused"
            );
        }
        for ok in ["clip.mp4", "My clip (1).GIF", "COM10.mp4", "console.mp4"] {
            assert!(pol.export_target(&root.join(ok), &proj).is_ok(), "{ok:?}");
        }
        let outs = [root.join("a.mp4"), root.join("b:x.mp4")];
        assert!(
            pol.batch_targets(&outs, &proj).is_err(),
            "one bad job rejects the batch"
        );
    }

    fn make_project_dir(p: &Path) {
        fs::create_dir_all(p).unwrap();
        fs::write(p.join(PROJECT_FILE), b"{}").unwrap();
    }

    #[test]
    fn report_is_size_capped() {
        assert!(report_bytes(r#"{"ok":true}"#).is_ok());
        // Oversized text is refused by length alone, even if it is not JSON at all.
        let big = "a".repeat(MAX_REPORT_BYTES + 1);
        let e = report_bytes(&big).unwrap_err().to_string();
        assert!(e.contains("too large"), "{e}");
        assert!(report_bytes("{not json").is_err(), "must still be JSON");
    }
}

#[cfg(test)]
mod import_tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target")
            .join("test-tmp")
            .join(format!("import-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        canon(&base).unwrap()
    }

    #[test]
    fn only_picked_small_png_or_jpeg_files_can_be_imported() {
        let base = tmp("src");
        let pol = PathPolicy::new(&base.join("Frostcap")).unwrap();
        let ok = base.join("bg.PNG");
        fs::write(&ok, b"x").unwrap();
        assert!(pol.import_open(&ok).is_err(), "not picked in a dialog yet");
        pol.allow_picked(&ok);
        let (_f, ext) = pol.import_open(&ok).unwrap();
        assert_eq!(ext, "png", "extension of the opened file, lower case");

        let jpg = base.join("a.jpeg");
        fs::write(&jpg, b"x").unwrap();
        pol.allow_picked(&jpg);
        assert!(pol.import_open(&jpg).is_ok());

        let exe = base.join("evil.exe");
        fs::write(&exe, b"x").unwrap();
        pol.allow_picked(&exe);
        assert!(pol.import_open(&exe).is_err(), "extension");

        let big = base.join("big.png");
        let f = fs::File::create(&big).unwrap();
        f.set_len(MAX_IMPORT_BYTES + 1).unwrap();
        drop(f);
        pol.allow_picked(&big);
        assert!(pol.import_open(&big).is_err(), "over 20 MiB");

        let dir = base.join("folder.png");
        fs::create_dir_all(&dir).unwrap();
        pol.allow_picked(&dir);
        assert!(pol.import_open(&dir).is_err(), "a folder");
        assert!(pol.import_open(&base.join("..").join("bg.PNG")).is_err());
        // Files inside the projects root are known without a dialog.
        let inside = pol.root().join("x.jpg");
        fs::write(&inside, b"x").unwrap();
        assert!(pol.import_open(&inside).is_ok());
    }

    #[test]
    fn the_policy_is_checked_on_the_opened_files_real_path() {
        let base = tmp("junction");
        let root = base.join("Frostcap");
        let pol = PathPolicy::new(&root).unwrap();
        let outside = base.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.png"), b"x").unwrap();
        // A junction inside the root that points outside it.
        let link = root.join("link");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !made {
            eprintln!("junction creation unavailable; skipping");
            return;
        }
        let via_link = link.join("secret.png");
        assert!(
            pol.import_open(&via_link).is_err(),
            "looks like it is inside the root, really is outside"
        );
        // Once the user picked the real file it is fine through either name.
        pol.allow_picked(&outside.join("secret.png"));
        assert!(pol.import_open(&via_link).is_ok());
        let _ = fs::remove_dir(&link);
    }

    #[test]
    fn batch_targets_are_validated_together() {
        let base = tmp("batch");
        let root = base.join("Frostcap");
        let pol = PathPolicy::new(&root).unwrap();
        let proj = root.join("a.frostcap");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join(PROJECT_FILE), b"{}").unwrap();
        let a = root.join("a.mp4");
        let b = root.join("b.gif");
        let ok = pol.batch_targets(&[a.clone(), b.clone()], &proj).unwrap();
        assert_eq!(ok.len(), 2);
        assert!(pol.batch_targets(&[], &proj).is_err(), "empty");
        assert!(
            pol.batch_targets(&[a.clone(), a.clone()], &proj).is_err(),
            "duplicate"
        );
        assert!(
            pol.batch_targets(&[a.clone(), proj.join("x.mp4")], &proj)
                .is_err(),
            "one bad target rejects the batch"
        );
        let many: Vec<PathBuf> = (0..=MAX_BATCH_JOBS)
            .map(|i| root.join(format!("{i}.mp4")))
            .collect();
        assert!(pol.batch_targets(&many, &proj).is_err(), "too many");
    }
}
