//! # frostcap-core
//!
//! UI-independent core of Frostcap:
//!
//! * [`sources`] – monitor / window / region sources, thumbnails ([`thumb`])
//! * [`recorder`] – recording session (screen, webcam, audio, input) with pause,
//!   crash-safety sidecars and structured health reports
//! * [`screen`] – Windows Graphics Capture -> [`mf`] H.264 writer (GPU path)
//! * [`webcam`] – Media Foundation camera capture -> [`mf`]
//! * [`audio`] – WASAPI mic / loopback -> WAV, aligned to the [`timeline`]
//! * [`input`] – ~120 Hz input sampler (cursor, buttons, key-downs, focus)
//! * [`project`] – the `<name>.frostcap/` project format + validation
//! * [`recovery`] – rebuild interrupted recordings
//! * [`autozoom`] – pure auto-zoom segment generator
//! * `frostcap-export` (separate crate) – export pipeline
//! * [`recent`], [`dialogs`], [`region`], [`disk`] – app plumbing
//!
//! Nothing in this crate touches the network.

#![warn(missing_docs)]

pub mod autozoom;
pub mod canvas;
pub mod error;
pub mod hdr;
pub mod mp4;
pub mod project;
pub mod recent;
pub mod settings;
pub mod timeline;
pub mod wav;

#[cfg(windows)]
pub mod affinity;
#[cfg(windows)]
pub mod audio;
#[cfg(windows)]
pub mod clock;
#[cfg(windows)]
pub mod com;
#[cfg(windows)]
pub mod dialogs;
#[cfg(windows)]
pub mod disk;
#[cfg(windows)]
pub mod displaycfg;
#[cfg(windows)]
pub mod hdr_gpu;
#[cfg(windows)]
pub mod hotkeys;
#[cfg(windows)]
pub mod input;
#[cfg(windows)]
pub mod mf;
#[cfg(windows)]
pub mod recorder;
#[cfg(windows)]
pub mod recovery;
#[cfg(windows)]
pub mod region;
#[cfg(windows)]
pub mod screen;
#[cfg(windows)]
pub mod sources;
#[cfg(windows)]
pub mod thumb;
#[cfg(windows)]
pub mod webcam;

pub use error::{Error, ErrorCode, ErrorPayload, Result};

/// Scratch directory for unit tests, inside the cargo target dir (never the system temp).
#[cfg(test)]
pub(crate) fn test_tmp_dir() -> std::path::PathBuf {
    let base = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    let dir = base.join("test-tmp");
    std::fs::create_dir_all(&dir).expect("create test tmp dir");
    dir
}
