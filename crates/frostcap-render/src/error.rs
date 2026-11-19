//! Error type.

use std::path::{Path, PathBuf};

/// Result alias.
pub type Result<T, E = RenderError> = std::result::Result<T, E>;

/// Export / render failure.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// Filesystem error.
    #[error("I/O error on {path}: {source}")]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// project.json could not be interpreted.
    #[error("project: {0}")]
    Project(String),
    /// Invalid export settings.
    #[error("invalid export settings: {0}")]
    Settings(String),
    /// GPU (wgpu) failure.
    #[error("GPU: {0}")]
    Gpu(String),
    /// Media Foundation decode / encode failure.
    #[error("media: {0}")]
    Media(String),
    /// Audio file / DSP failure.
    #[error("audio: {0}")]
    Audio(String),
    /// The export was cancelled.
    #[error("export cancelled")]
    Cancelled,
}

impl RenderError {
    /// Wrap an I/O error with its path.
    pub fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

#[cfg(windows)]
impl From<windows::core::Error> for RenderError {
    fn from(e: windows::core::Error) -> Self {
        Self::Media(format!("{} (0x{:08X})", e.message(), e.code().0 as u32))
    }
}
