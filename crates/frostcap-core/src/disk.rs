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
