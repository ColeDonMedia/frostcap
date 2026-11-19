//! Error type for frostcap-core, with stable machine-readable codes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Result alias used across the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// All errors produced by frostcap-core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Filesystem error on a specific path.
    #[error("I/O error on {path}: {source}")]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// JSON (de)serialization failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// project.json was written by a newer Frostcap (or is version 0).
    #[error("unsupported project version {0}")]
    UnsupportedVersion(u32),
    /// project.json is malformed, oversized or out of range.
    #[error("invalid project: {0}")]
    InvalidProject(String),
    /// A path was rejected (not a project folder, traversal, wrong extension...).
    #[error("invalid path: {0}")]
    InvalidPath(String),
    /// The requested capture source does not exist (anymore).
    #[error("capture source not found: {0}")]
    SourceNotFound(String),
    /// The disk is (nearly) full.
    #[error("not enough disk space: {0}")]
    DiskFull(String),
    /// No usable H.264 encoder.
    #[error("no H.264 encoder available: {0}")]
    NoH264(String),
    /// Access denied (privacy settings, file permissions, device in use).
    #[error("permission denied: {0}")]
    Permission(String),
    /// Screen capture / encoder failure.
    #[error("capture error: {0}")]
    Capture(String),
    /// Audio (WASAPI) failure.
    #[error("audio error: {0}")]
    Audio(String),
    /// Windows API failure.
    #[cfg(windows)]
    #[error("windows error: {0}")]
    Windows(#[from] windows::core::Error),
    /// Invalid argument / state.
    #[error("{0}")]
    Invalid(String),
    /// An error carried across a thread boundary with its code preserved.
    #[error("{}", .0.message)]
    Coded(ErrorPayload),
}

/// Stable error codes sent to the UI.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Disk full / low space.
    DiskFull,
    /// Capture source (window, monitor, camera) is gone.
    SourceGone,
    /// No H.264 encoder.
    NoH264,
    /// Path rejected.
    InvalidPath,
    /// Project rejected.
    InvalidProject,
    /// Access denied.
    Permission,
    /// Other I/O.
    Io,
    /// The user declined a confirmation.
    Cancelled,
    /// Anything else.
    Internal,
}

const ERROR_DISK_FULL: i32 = 112;
const ERROR_HANDLE_DISK_FULL: i32 = 39;
const ERROR_ACCESS_DENIED: i32 = 5;

fn io_code(e: &std::io::Error) -> ErrorCode {
    match e.raw_os_error() {
        Some(ERROR_DISK_FULL | ERROR_HANDLE_DISK_FULL) => return ErrorCode::DiskFull,
        Some(ERROR_ACCESS_DENIED) => return ErrorCode::Permission,
        _ => {}
    }
    match e.kind() {
        std::io::ErrorKind::StorageFull => ErrorCode::DiskFull,
        std::io::ErrorKind::PermissionDenied => ErrorCode::Permission,
        _ => ErrorCode::Io,
    }
}

#[cfg(windows)]
fn hresult_code(e: &windows::core::Error) -> ErrorCode {
    let hr = e.code().0 as u32;
    match hr {
        0x8007_0070 | 0x8007_0027 => ErrorCode::DiskFull,
        0x8007_0005 => ErrorCode::Permission,
        // MF_E_TOPO_CODEC_NOT_FOUND / MF_E_TRANSFORM_NOT_POSSIBLE_FOR_CURRENT_MEDIATYPE / MF_E_INVALIDMEDIATYPE
        0xC00D_5212 | 0xC00D_6D60 | 0xC00D_36B4 => ErrorCode::NoH264,
        0x8007_0578 | 0x8007_048F => ErrorCode::SourceGone, // invalid window handle / device not connected
        _ => ErrorCode::Internal,
    }
}

impl Error {
    /// Wrap an I/O error with its path.
    pub fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    /// Machine-readable code for the UI.
    pub fn code(&self) -> ErrorCode {
        match self {
            Error::Io { source, .. } => io_code(source),
            Error::Json(_) | Error::UnsupportedVersion(_) | Error::InvalidProject(_) => {
                ErrorCode::InvalidProject
            }
            Error::InvalidPath(_) => ErrorCode::InvalidPath,
            Error::SourceNotFound(_) => ErrorCode::SourceGone,
            Error::DiskFull(_) => ErrorCode::DiskFull,
            Error::NoH264(_) => ErrorCode::NoH264,
            Error::Permission(_) => ErrorCode::Permission,
            #[cfg(windows)]
            Error::Windows(e) => hresult_code(e),
            Error::Capture(_) | Error::Audio(_) | Error::Invalid(_) => ErrorCode::Internal,
            Error::Coded(p) => p.code,
        }
    }

    /// Structured form for the UI.
    pub fn to_payload(&self) -> ErrorPayload {
        ErrorPayload {
            code: self.code(),
            message: self.to_string(),
        }
    }
}

/// `{ code, message }` as sent to the UI.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ErrorPayload {
    /// Stable code.
    pub code: ErrorCode,
    /// Human readable message.
    pub message: String,
}

impl From<frostcap_project::Error> for Error {
    fn from(e: frostcap_project::Error) -> Self {
        match e {
            frostcap_project::Error::Io { path, source } => Self::Io { path, source },
            frostcap_project::Error::Json(e) => Self::Json(e),
            frostcap_project::Error::UnsupportedVersion(v) => Self::UnsupportedVersion(v),
            frostcap_project::Error::InvalidProject(m) => Self::InvalidProject(m),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_is_a_stable_snake_case_code() {
        assert_eq!(
            serde_json::to_string(&ErrorCode::Cancelled).unwrap(),
            "\"cancelled\""
        );
    }

    #[test]
    fn codes_survive_thread_hops_and_are_never_guessed() {
        let e = Error::io(Path::new("x"), std::io::Error::from_raw_os_error(112));
        let hop = Error::Coded(e.to_payload());
        assert_eq!(hop.code(), ErrorCode::DiskFull);
        assert_eq!(hop.to_string(), e.to_string());
        assert_eq!(
            Error::Capture("wrote to disk".into()).code(),
            ErrorCode::Internal
        );
        assert_eq!(
            Error::Audio("0x80070070".into()).code(),
            ErrorCode::Internal
        );
    }

    #[test]
    fn codes() {
        let e = Error::io(Path::new("x"), std::io::Error::from_raw_os_error(112));
        assert_eq!(e.code(), ErrorCode::DiskFull);
        let e = Error::io(Path::new("x"), std::io::Error::from_raw_os_error(5));
        assert_eq!(e.code(), ErrorCode::Permission);
        assert_eq!(
            Error::InvalidProject("x".into()).code(),
            ErrorCode::InvalidProject
        );
        assert_eq!(
            Error::SourceNotFound("x".into()).code(),
            ErrorCode::SourceGone
        );
        let json = serde_json::to_string(&Error::DiskFull("x".into()).to_payload()).unwrap();
        assert!(json.contains("\"code\":\"disk_full\""));
        #[cfg(windows)]
        {
            let e = Error::Windows(windows::core::Error::from_hresult(windows::core::HRESULT(
                0x8007_0070_u32 as i32,
            )));
            assert_eq!(e.code(), ErrorCode::DiskFull);
        }
    }
}
