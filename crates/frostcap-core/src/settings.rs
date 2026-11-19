//! Persisted capture settings (small JSON file in the app data folder).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Largest settings file we read.
const MAX_BYTES: u64 = 4096;

/// User-visible capture preferences (both default **on**).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct CaptureSettings {
    /// Show the Windows Graphics Capture border (yellow outline) while recording.
    pub capture_border: bool,
    /// Hide Frostcap's own windows from recordings (`WDA_EXCLUDEFROMCAPTURE`).
    pub exclude_own_windows: bool,
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self {
            capture_border: true,
            exclude_own_windows: true,
        }
    }
}

impl CaptureSettings {
    /// Read `path`; a missing, oversized or corrupt file yields the defaults.
    pub fn load(path: &Path) -> Self {
        let small = std::fs::metadata(path).is_ok_and(|m| m.len() <= MAX_BYTES);
        small
            .then(|| std::fs::read(path).ok())
            .flatten()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Atomically write to `path` (parent folder is created).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        let bytes = serde_json::to_vec(self).map_err(|e| Error::Invalid(e.to_string()))?;
        crate::project::write_atomic(path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        crate::test_tmp_dir().join(format!("settings-{name}-{}.json", std::process::id()))
    }

    #[test]
    fn defaults_keep_the_border_and_hide_our_own_windows() {
        let s = CaptureSettings::default();
        assert!(s.capture_border && s.exclude_own_windows);
    }

    #[test]
    fn missing_or_corrupt_files_fall_back_to_defaults() {
        assert_eq!(
            CaptureSettings::load(&tmp("missing")),
            CaptureSettings::default()
        );
        let p = tmp("corrupt");
        std::fs::write(&p, b"{not json").unwrap();
        assert_eq!(CaptureSettings::load(&p), CaptureSettings::default());
        std::fs::write(&p, vec![b' '; 100_000]).unwrap();
        assert_eq!(CaptureSettings::load(&p), CaptureSettings::default());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn roundtrip_uses_camel_case_and_tolerates_missing_fields() {
        let p = tmp("roundtrip");
        let s = CaptureSettings {
            capture_border: false,
            exclude_own_windows: true,
        };
        s.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("captureBorder") && text.contains("excludeOwnWindows"));
        assert_eq!(CaptureSettings::load(&p), s);
        std::fs::write(&p, br#"{"captureBorder":false}"#).unwrap();
        let l = CaptureSettings::load(&p);
        assert!(
            !l.capture_border && l.exclude_own_windows,
            "missing = default on"
        );
        let _ = std::fs::remove_file(&p);
    }
}
