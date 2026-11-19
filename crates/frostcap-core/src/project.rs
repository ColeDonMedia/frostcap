//! The on-disk Frostcap project format.
//!
//! The schema, validation, loading and atomic saving live in the tiny
//! `frostcap-project` crate (serde only) so the export renderer shares the exact
//! same typed model; this module re-exports it so `frostcap_core::project::*`
//! keeps working. See that crate for the folder layout, time base and coordinate
//! space documentation.

pub use frostcap_project::Error as ProjectError;
pub use frostcap_project::{
    AudioStyle, AudioTrack, ButtonState, CaptionCue, CaptionPosition, CaptionStyle, ClickEvent,
    Corner, CursorSample, CursorStyle, Easing, EditList, FocusChange, KeyEvent, META_FILE,
    MIC_AUDIO_FILE, MediaFiles, Modifiers, MouseButton, NormPoint, PROJECT_EXT, PROJECT_FILE,
    PROJECT_VERSION, PauseGap, Project, ProjectMeta, ProjectStatus, Rect, SCREEN_FILE,
    SYSTEM_AUDIO_FILE, SegmentOrigin, Size, SourceDescriptor, SourceKind, Style, Tracks, TrimRange,
    VideoTrack, WEBCAM_FILE, WebcamShape, WebcamStyle, WindowFrame, ZoomSegment, limits,
    project_dir_of, read_meta,
};
pub use frostcap_project::{BackgroundImage, background_path};
pub use frostcap_project::{import_background, import_background_file};

/// Write `bytes` to `file` via a sibling temp file + rename (+ fsync).
pub fn write_atomic(file: &std::path::Path, bytes: &[u8]) -> crate::Result<()> {
    Ok(frostcap_project::write_atomic(file, bytes)?)
}

/// Example projects for tests and golden fixtures (not used by the app).
#[doc(hidden)]
pub mod samples {
    use super::*;

    pub use frostcap_project::samples::sample;

    /// A realistic project used for the golden fixture (`fixtures/project.sample.json`).
    pub fn fixture() -> Project {
        let mut p = sample();
        p.name = "Recording 2026-11-17 14.03.31".into();
        p.media.mic_audio = Some(AudioTrack {
            file: MIC_AUDIO_FILE.into(),
            offset_ms: 0,
            sample_rate: 48_000,
            channels: 2,
            duration_ms: 3000,
        });
        p.media.webcam = Some(WEBCAM_FILE.into());
        p.media.webcam_video = Some(VideoTrack {
            file: WEBCAM_FILE.into(),
            name: "Integrated Camera".into(),
            width: 1280,
            height: 720,
            offset_ms: 0,
            duration_ms: 3000,
        });
        p.tracks.keys.push(KeyEvent {
            t_ms: 1200,
            vk: 0x53,
            label: "S".into(),
            modifiers: Modifiers {
                ctrl: true,
                ..Default::default()
            },
        });
        p.tracks.focus.push(FocusChange {
            t_ms: 0,
            window_id: 1,
            title: String::new(),
            rect: Rect {
                x: 100,
                y: 80,
                width: 900,
                height: 600,
            },
        });
        p.pauses.push(PauseGap {
            at_ms: 1500,
            duration_ms: 2000,
        });
        let s = &mut p.edits.style;
        s.show_keys = true;
        s.frame = WindowFrame::Windows;
        s.cursor = CursorStyle {
            visible: true,
            smoothing: 0.5,
            motion_blur: 0.25,
        };
        s.webcam = WebcamStyle {
            enabled: true,
            shape: WebcamShape::Rounded,
            size: 0.25,
            position: Corner::TopRight,
            ..WebcamStyle::default()
        };
        s.audio = AudioStyle {
            noise_gate: true,
            noise_gate_threshold: -45.0,
            high_pass: true,
            high_pass_hz: 100.0,
            normalize: true,
            target_lufs: -16.0,
            mic_gain: 1.0,
            system_gain: 0.5,
        };
        s.captions = CaptionStyle {
            enabled: true,
            font_size: 44.0,
            position: CaptionPosition::Bottom,
            background: true,
        };
        p.edits.captions = vec![
            CaptionCue {
                start_ms: 200,
                end_ms: 1400,
                text: "Open the settings".into(),
            },
            CaptionCue {
                start_ms: 1600,
                end_ms: 2800,
                text: "Save with Ctrl + S".into(),
            },
        ];
        p.edits.splits = vec![1500];
        p.edits.ui = serde_json::json!({
            "v": 1,
            "zoom": {"preset": "balanced", "scale": 1.8, "easing": "easeInOutCubic", "transitionMs": 650}
        });
        p.edits.zoom_segments =
            crate::autozoom::generate_for_project(&p, crate::autozoom::ZoomPreset::Balanced);
        p
    }
}

/// Shared test helpers.
#[cfg(test)]
pub(crate) mod tests_support {
    /// A small valid project.
    pub fn minimal() -> super::Project {
        super::samples::sample()
    }
}

#[cfg(test)]
mod tests {
    use super::samples::fixture;
    use super::*;
    use crate::Error;

    #[test]
    fn fixture_validates_and_roundtrips() {
        let p = fixture();
        p.validate().unwrap();
        let back: Project = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn project_errors_keep_their_core_codes() {
        let dir = crate::test_tmp_dir().join(format!("core-codes-{}.frostcap", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(PROJECT_FILE), b"{\"version\":1,").unwrap();
        let e: Error = Project::load(&dir).unwrap_err().into();
        assert!(matches!(e, Error::InvalidProject(_)));
        let mut p = samples::sample();
        p.version = 99;
        let e: Error = p.validate().unwrap_err().into();
        assert!(matches!(e, Error::UnsupportedVersion(99)));
        let e: Error = Project::load(&dir.join("missing")).unwrap_err().into();
        assert!(matches!(e, Error::Io { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
