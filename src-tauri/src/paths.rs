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
        base
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
