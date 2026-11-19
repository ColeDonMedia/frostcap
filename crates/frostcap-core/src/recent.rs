//! Recent-projects list.
//!
//! Stored as a small JSON array of folder paths (most recent first) in a file the
//! host chooses (the Tauri app uses `<app data dir>/recent.json`). Listing also
//! scans an optional projects root for `*.frostcap` folders, reads only the small
//! `meta.json` of each (falling back to `project.json`), and flags interrupted
//! recordings (`status: "recording"` without a live session) as `interrupted` so the
//! UI can offer [`crate::recovery::recover_project`].

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::project::{PROJECT_EXT, PROJECT_FILE, Project, ProjectStatus, Size, read_meta};

/// Maximum number of entries kept in the store.
pub const MAX_RECENT: usize = 50;
/// Maximum folders scanned in the projects root.
const MAX_SCAN: usize = 500;

/// Summary of a project for the start screen.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecentProject {
    /// Project folder.
    pub path: PathBuf,
    /// Project name.
    pub name: String,
    /// Creation time (Unix ms).
    pub created_at_ms: u64,
    /// Duration (ms).
    pub duration_ms: u64,
    /// Canvas size.
    pub canvas: Size,
    /// Lifecycle state.
    pub status: ProjectStatus,
    /// Interrupted recording (crash) that can be recovered with `recover_project`.
    pub interrupted: bool,
}

fn read_store(store: &Path) -> Vec<PathBuf> {
    fs::read(store)
        .ok()
        .filter(|b| b.len() < 1 << 20)
        .and_then(|b| serde_json::from_slice::<Vec<PathBuf>>(&b).ok())
        .unwrap_or_default()
}

/// Record `project_dir` as most recently used.
pub fn add_recent(store: &Path, project_dir: &Path) -> Result<()> {
    // Read-modify-write: serialize writers in this process.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut list = read_store(store);
    list.retain(|p| p != project_dir);
    list.insert(0, project_dir.to_path_buf());
    list.truncate(MAX_RECENT);
    if let Some(parent) = store.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    crate::project::write_atomic(store, &serde_json::to_vec(&list)?)
}

/// Whether `path` is listed in the store (used to authorize re-opening).
pub fn is_recent(store: &Path, path: &Path) -> bool {
    read_store(store).iter().any(|p| p == path)
}

fn summarize(dir: &Path, live: Option<&Path>) -> Option<RecentProject> {
    let meta = read_meta(dir).or_else(|| Project::load(dir).ok().map(|p| p.meta()))?;
    let interrupted = meta.status == ProjectStatus::Recording && live != Some(dir);
    Some(RecentProject {
        path: dir.to_path_buf(),
        name: meta.name,
        created_at_ms: meta.created_at_ms,
        duration_ms: meta.duration_ms,
        canvas: meta.canvas,
        status: meta.status,
        interrupted,
    })
}

/// List recent projects: store entries first (in order), then any other
/// `*.frostcap` folders under `scan_root` (newest first). Unreadable entries are
/// skipped. `live` is the folder of the currently running recording, if any.
pub fn list_recent(
    store: &Path,
    scan_root: Option<&Path>,
    live: Option<&Path>,
) -> Vec<RecentProject> {
    let stored = read_store(store);
    let mut out: Vec<RecentProject> = stored.iter().filter_map(|d| summarize(d, live)).collect();
    let mut scanned = Vec::new();
    if let Some(root) = scan_root
        && let Ok(rd) = fs::read_dir(root)
    {
        for entry in rd.flatten().take(MAX_SCAN) {
            let p = entry.path();
            if p.is_dir()
                && p.extension().is_some_and(|e| e == PROJECT_EXT)
                && p.join(PROJECT_FILE).is_file()
                && !stored.contains(&p)
                && let Some(r) = summarize(&p, live)
            {
                scanned.push(r);
            }
        }
    }
    scanned.sort_by_key(|r| std::cmp::Reverse(r.created_at_ms));
    out.extend(scanned);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_roundtrip_dedupes_and_orders() {
        let root = crate::test_tmp_dir().join(format!("recent-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = root.join("recent.json");
        add_recent(&store, Path::new("C:/a.frostcap")).unwrap();
        add_recent(&store, Path::new("C:/b.frostcap")).unwrap();
        add_recent(&store, Path::new("C:/a.frostcap")).unwrap();
        assert_eq!(
            read_store(&store),
            vec![
                PathBuf::from("C:/a.frostcap"),
                PathBuf::from("C:/b.frostcap")
            ]
        );
        assert!(is_recent(&store, Path::new("C:/b.frostcap")));
        // Entries without a readable project are skipped.
        assert!(list_recent(&store, Some(&root), None).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn concurrent_adds_are_not_lost() {
        let root = crate::test_tmp_dir().join(format!("recent-conc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = root.join("recent.json");
        let handles: Vec<_> = (0..16)
            .map(|i| {
                let store = store.clone();
                std::thread::spawn(move || {
                    add_recent(&store, Path::new(&format!("C:/p{i}.frostcap"))).unwrap()
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(read_store(&store).len(), 16);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn interrupted_recordings_are_flagged_unless_live() {
        let root = crate::test_tmp_dir().join(format!("recent-int-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("crashed.frostcap");
        let mut p = crate::project::tests_support::minimal();
        p.status = ProjectStatus::Recording;
        p.save(&dir).unwrap();
        let store = root.join("recent.json");
        let l = list_recent(&store, Some(&root), None);
        assert_eq!(l.len(), 1);
        assert!(l[0].interrupted);
        let l = list_recent(&store, Some(&root), Some(&dir));
        assert!(
            !l[0].interrupted,
            "the live session's folder is not interrupted"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
