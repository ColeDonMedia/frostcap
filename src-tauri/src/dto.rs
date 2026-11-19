//! Wire DTOs of the command surface (camelCase JSON). Rust is the source of
//! truth: `docs/CONTRACT.md` documents them and `fixtures/` holds golden JSON
//! generated from these types by the `golden_fixtures` test.

use std::path::PathBuf;

use frostcap_core::error::{Error, ErrorCode, ErrorPayload};
use frostcap_core::project::Size;
use frostcap_core::sources::CaptureSource;
use frostcap_export::ExportResult;
use serde::{Deserialize, Serialize};

/// Error returned by every command: `{ code, message }`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CommandError {
    /// Stable code (`disk_full`, `source_gone`, `no_h264`, `invalid_path`,
    /// `invalid_project`, `permission`, `io`, `internal`).
    pub code: ErrorCode,
    /// Human readable message.
    pub message: String,
}

impl From<Error> for CommandError {
    fn from(e: Error) -> Self {
        let ErrorPayload { code, message } = e.to_payload();
        Self { code, message }
    }
}

impl From<frostcap_core::project::ProjectError> for CommandError {
    fn from(e: frostcap_core::project::ProjectError) -> Self {
        Error::from(e).into()
    }
}

impl From<ErrorPayload> for CommandError {
    fn from(p: ErrorPayload) -> Self {
        Self {
            code: p.code,
            message: p.message,
        }
    }
}

impl CommandError {
    /// `internal` error with a message.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Internal,
            message: msg.into(),
        }
    }
}

/// Result type of every command.
pub type CmdResult<T> = Result<T, CommandError>;

/// `source` argument of `start_recording`: the tagged [`CaptureSource`] or a flat
/// id from `list_sources().sources[].id`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum SourceArg {
    /// `{ kind: "monitor", id }`, `{ kind: "window", hwnd, pid? }`, `{ kind: "region", monitorId, rect }`, `{ kind: "primaryMonitor" }`.
    Tagged(CaptureSource),
    /// `"display:\\.\DISPLAY1"`, `"window:65812"`, `"primary"`.
    Id(String),
}

impl SourceArg {
    /// Resolve to a tagged source.
    pub fn into_source(self) -> Result<CaptureSource, Error> {
        match self {
            SourceArg::Tagged(s) => Ok(s),
            SourceArg::Id(id) => CaptureSource::from_id(&id),
        }
    }
}

/// `start_recording` response.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StartResponse {
    /// New project folder.
    pub project_dir: PathBuf,
    /// Locked encoder size.
    pub canvas: Size,
    /// Time until capture was running (ms).
    pub start_ms: u64,
}

/// `stop_recording` response.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StopResponse {
    /// Project folder.
    pub project_dir: PathBuf,
    /// Duration (= screen.mp4 duration).
    pub duration_ms: u64,
    /// Non-fatal problems.
    pub warnings: Vec<String>,
}

/// `recording_state` response.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStateResponse {
    /// A session is running (possibly paused).
    pub recording: bool,
    /// Paused.
    pub paused: bool,
    /// Output-timeline position (pauses excluded).
    pub elapsed_ms: u64,
    /// Folder being written.
    pub project_dir: Option<PathBuf>,
}

/// `recording://tick` payload.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Tick {
    /// Output-timeline position (pauses excluded).
    pub elapsed_ms: u64,
    /// Paused.
    pub paused: bool,
}

/// `recording://warning` payload.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WarningEvent {
    /// Always `"warning"`.
    pub code: String,
    /// Message.
    pub message: String,
}

/// `recording://stopped` payload (auto-stop after a fatal error, window close...).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoppedEvent {
    /// The final result (absent if finalizing failed).
    pub result: Option<StopResponse>,
    /// Why it stopped (absent for a user stop).
    pub error: Option<CommandError>,
}

/// `export://progress` payload.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportProgress {
    /// Job id (from the `jobId` argument, or generated).
    pub job_id: String,
    /// Project folder.
    pub path: PathBuf,
    /// Output file.
    pub out_path: PathBuf,
    /// 0..1.
    pub progress: f64,
    /// Stage name (`copying`, `decoding`, `rendering`, `encoding`, `muxing`, `done`...).
    pub stage: String,
}

/// `export_project` response.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportResponse {
    /// Job id.
    pub job_id: String,
    /// Result.
    #[serde(flatten)]
    pub result: ExportResult,
}

/// `hotkey://pressed` payload.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyEvent {
    /// Which chord.
    pub action: frostcap_core::hotkeys::HotkeyAction,
    /// The backend already acted (stopped / paused / resumed). `false` for
    /// start-stop while idle: the UI should start a recording with its settings.
    pub handled: bool,
}

/// One registered chord.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyChord {
    /// Action.
    pub action: frostcap_core::hotkeys::HotkeyAction,
    /// e.g. `Ctrl+Alt+Shift+R`.
    pub chord: String,
    /// False if another app owns the chord.
    pub registered: bool,
}

/// `hotkeys` response.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyInfo {
    /// All chords.
    pub chords: Vec<HotkeyChord>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use frostcap_core::hotkeys::HotkeyAction;
    use frostcap_core::input::KeyCapture;
    use frostcap_core::project::{ProjectStatus, Rect, samples};
    use frostcap_core::recent::RecentProject;
    use frostcap_core::region::RegionSelection;
    use frostcap_core::sources::{
        MonitorInfo, NormRect, SourceItem, SourceItemKind, SourceList, WindowInfo,
    };
    use frostcap_core::webcam::CameraInfo;
    use frostcap_export::{ExportPreset, ExportRequest};
    use serde_json::{Value, json};
    use std::path::Path;

    fn fixtures_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures")
    }

    fn v<T: Serialize>(t: &T) -> Value {
        serde_json::to_value(t).unwrap()
    }

    fn err(code: ErrorCode, message: &str) -> Value {
        v(&CommandError {
            code,
            message: message.into(),
        })
    }

    /// Every command: `{ args, response, error? }`; events: payload.
    fn golden() -> Vec<(String, Value)> {
        let project_dir =
            PathBuf::from(r"C:\Users\me\Videos\Frostcap\Recording 2026-11-17 14.03.31.frostcap");
        let display = CaptureSource::Monitor {
            id: r"\\.\DISPLAY1".into(),
        };
        let window = CaptureSource::Window {
            hwnd: 65812,
            pid: Some(4242),
        };
        let sources = SourceList {
            monitors: vec![MonitorInfo {
                id: r"\\.\DISPLAY1".into(),
                name: "DELL U2720Q".into(),
                bounds: Rect {
                    x: 0,
                    y: 0,
                    width: 2560,
                    height: 1440,
                },
                refresh_rate: 60,
                scale_factor: 1.5,
                primary: true,
            }],
            windows: vec![WindowInfo {
                hwnd: 65812,
                pid: 4242,
                title: "Untitled - Notepad".into(),
                process_name: "Notepad.exe".into(),
                bounds: Rect {
                    x: -1200,
                    y: 80,
                    width: 900,
                    height: 600,
                },
            }],
            sources: vec![
                SourceItem {
                    id: r"display:\\.\DISPLAY1".into(),
                    kind: SourceItemKind::Display,
                    name: "DELL U2720Q".into(),
                    width: 2560,
                    height: 1440,
                    thumbnail: Some("data:image/png;base64,iVBORw0KGgo=".into()),
                    source: display.clone(),
                },
                SourceItem {
                    id: "window:65812".into(),
                    kind: SourceItemKind::Window,
                    name: "Untitled - Notepad".into(),
                    width: 900,
                    height: 600,
                    thumbnail: None,
                    source: window.clone(),
                },
            ],
        };
        let stop = StopResponse {
            project_dir: project_dir.clone(),
            duration_ms: 3489,
            warnings: vec![],
        };
        let export = ExportResponse {
            job_id: "job-1".into(),
            result: real_raw_export(),
        };
        let region = RegionSelection {
            monitor_id: r"\\.\DISPLAY1".into(),
            rect: NormRect {
                x: 0.25,
                y: 0.25,
                width: 0.5,
                height: 0.5,
            },
            screen_rect: Rect {
                x: 640,
                y: 360,
                width: 1280,
                height: 720,
            },
        };
        let recent = RecentProject {
            path: project_dir.clone(),
            name: "Recording 2026-11-17 14.03.31".into(),
            created_at_ms: 1_794_947_689_919,
            duration_ms: 3489,
            canvas: Size {
                width: 2560,
                height: 1440,
            },
            status: ProjectStatus::Recording,
            interrupted: true,
        };
        let cmd = |args: Value, response: Value, error: Option<Value>| {
            let mut o = json!({ "args": args, "response": response });
            if let Some(e) = error {
                o["error"] = e;
            }
            o
        };
        let project = samples::fixture();
        vec![
            (
                "commands/list_sources".into(),
                cmd(json!({ "thumbnails": true }), v(&sources), None),
            ),
            (
                "commands/list_cameras".into(),
                cmd(
                    json!({}),
                    v(&vec![CameraInfo {
                        id: r"\\?\usb#vid_04f2&pid_b6be#0".into(),
                        name: "Integrated Camera".into(),
                    }]),
                    None,
                ),
            ),
            (
                "commands/start_recording".into(),
                cmd(
                    json!({ "source": v(&display), "fps": 60, "cursor": false, "mic": true, "systemAudio": true, "keys": v(&KeyCapture::Shortcuts), "webcam": "default" }),
                    v(&StartResponse {
                        project_dir: project_dir.clone(),
                        canvas: Size {
                            width: 2560,
                            height: 1440,
                        },
                        start_ms: 1601,
                    }),
                    Some(err(
                        ErrorCode::DiskFull,
                        "not enough disk space: only 512 MB free on the recording drive (need 1024 MB)",
                    )),
                ),
            ),
            (
                "commands/pause_recording".into(),
                cmd(json!({}), json!(true), None),
            ),
            (
                "commands/resume_recording".into(),
                cmd(json!({}), json!(true), None),
            ),
            (
                "commands/stop_recording".into(),
                cmd(json!({}), v(&stop), None),
            ),
            (
                "commands/recording_state".into(),
                cmd(
                    json!({}),
                    v(&RecordingStateResponse {
                        recording: true,
                        paused: false,
                        elapsed_ms: 1200,
                        project_dir: Some(project_dir.clone()),
                    }),
                    None,
                ),
            ),
            (
                "commands/load_project".into(),
                cmd(
                    json!({ "path": project_dir }),
                    v(&project),
                    Some(err(
                        ErrorCode::InvalidPath,
                        "invalid path: C:\\Windows is not a .frostcap folder",
                    )),
                ),
            ),
            (
                "commands/save_project".into(),
                cmd(
                    json!({ "path": project_dir, "project": v(&project) }),
                    Value::Null,
                    None,
                ),
            ),
            (
                "commands/list_recent_projects".into(),
                cmd(json!({}), v(&vec![recent]), None),
            ),
            (
                "commands/recover_project".into(),
                cmd(json!({ "path": project_dir }), v(&project), None),
            ),
            (
                "commands/scrub_project".into(),
                cmd(
                    json!({ "path": project_dir, "keys": true, "titles": true }),
                    v(&project),
                    None,
                ),
            ),
            (
                "commands/generate_auto_zoom".into(),
                cmd(
                    json!({ "project": v(&project), "preset": "balanced" }),
                    v(&project.edits.zoom_segments),
                    None,
                ),
            ),
            (
                "commands/export_project".into(),
                cmd(
                    json!({ "path": project_dir, "preset": { "aspect": "16:9", "width": 1920, "height": 1080, "fps": 60, "format": "mp4" }, "outPath": r"C:\Users\me\Videos\Frostcap\clip.mp4", "jobId": "job-1" }),
                    v(&export),
                    Some(err(
                        ErrorCode::InvalidPath,
                        "invalid path: choose the export location with the save dialog",
                    )),
                ),
            ),
            (
                "commands/cancel_export".into(),
                cmd(json!({ "jobId": "job-1" }), json!(true), None),
            ),
            (
                "commands/pick_project".into(),
                cmd(json!({}), json!(project_dir), None),
            ),
            (
                "commands/pick_folder".into(),
                cmd(
                    json!({ "title": "Choose a folder" }),
                    json!(r"C:\Users\me\Desktop"),
                    None,
                ),
            ),
            (
                "commands/pick_save_path".into(),
                cmd(
                    json!({ "defaultName": "clip", "ext": "mp4" }),
                    json!(r"C:\Users\me\Desktop\clip.mp4"),
                    None,
                ),
            ),
            (
                "commands/reveal_in_folder".into(),
                cmd(
                    json!({ "path": r"C:\Users\me\Desktop\clip.mp4" }),
                    Value::Null,
                    None,
                ),
            ),
            (
                "commands/pick_region".into(),
                cmd(json!({}), v(&region), None),
            ),
            (
                "commands/default_output_dir".into(),
                cmd(json!({}), json!(r"C:\Users\me\Videos\Frostcap"), None),
            ),
            (
                "commands/hotkeys".into(),
                cmd(
                    json!({}),
                    v(&HotkeyInfo {
                        chords: vec![
                            HotkeyChord {
                                action: HotkeyAction::StartStop,
                                chord: "Ctrl+Alt+Shift+R".into(),
                                registered: true,
                            },
                            HotkeyChord {
                                action: HotkeyAction::PauseResume,
                                chord: "Ctrl+Alt+Shift+P".into(),
                                registered: false,
                            },
                        ],
                    }),
                    None,
                ),
            ),
            (
                "events/hotkey_pressed".into(),
                v(&HotkeyEvent {
                    action: HotkeyAction::PauseResume,
                    handled: true,
                }),
            ),
            (
                "events/recording_tick".into(),
                v(&Tick {
                    elapsed_ms: 1200,
                    paused: false,
                }),
            ),
            (
                "events/recording_error".into(),
                err(ErrorCode::SourceGone, "the recorded window was closed"),
            ),
            (
                "events/recording_warning".into(),
                v(&WarningEvent {
                    code: "warning".into(),
                    message: "microphone capture stopped unexpectedly".into(),
                }),
            ),
            (
                "events/recording_stopped".into(),
                v(&StoppedEvent {
                    result: Some(stop),
                    error: Some(err_struct()),
                }),
            ),
            (
                "events/export_progress".into(),
                v(&ExportProgress {
                    job_id: "job-1".into(),
                    path: project_dir.clone(),
                    out_path: r"C:\Users\me\Videos\Frostcap\clip.mp4".into(),
                    progress: 0.5,
                    stage: "rendering".into(),
                }),
            ),
            ("project.sample".into(), v(&project)),
        ]
    }

    /// A real `export_with` response (raw copy of an unedited project), with the
    /// machine-specific output path replaced by a stable example.
    fn real_raw_export() -> ExportResult {
        let base = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../target"))
            .join("test-tmp")
            .join(format!("fixture-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("p.frostcap");
        samples::sample().save(&dir).unwrap();
        std::fs::write(dir.join("screen.mp4"), vec![0u8; 1024]).unwrap();
        let out = base.join("clip.mp4");
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut r = frostcap_export::export_with(
            &dir,
            &ExportRequest::Legacy(ExportPreset::Raw),
            &out,
            &cancel,
            |_, _| {},
        )
        .expect("raw export");
        let _ = std::fs::remove_dir_all(&base);
        r.out_path = r"C:\Users\me\Videos\Frostcap\clip.mp4".into();
        r
    }

    fn err_struct() -> CommandError {
        CommandError {
            code: ErrorCode::DiskFull,
            message: "only 280 MB left on the recording drive; recording stopped".into(),
        }
    }

    /// Golden fixtures must match the Rust serialization. Regenerate with
    /// `FROSTCAP_UPDATE_FIXTURES=1 cargo test -p frostcap --lib golden`.
    #[test]
    fn golden_fixtures() {
        let update = std::env::var_os("FROSTCAP_UPDATE_FIXTURES").is_some();
        let mut stale = Vec::new();
        for (name, value) in golden() {
            let path = fixtures_dir().join(format!("{name}.json"));
            let text = serde_json::to_string_pretty(&value).unwrap() + "\n";
            if update {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, &text).unwrap();
            } else if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
                stale.push(name);
            }
        }
        assert!(
            stale.is_empty(),
            "stale/missing fixtures (set FROSTCAP_UPDATE_FIXTURES=1): {stale:?}"
        );
    }

    #[test]
    fn source_arg_accepts_both_shapes() {
        let a: SourceArg = serde_json::from_str(r#"{"kind":"window","hwnd":12,"pid":3}"#).unwrap();
        assert_eq!(
            a.into_source().unwrap(),
            CaptureSource::Window {
                hwnd: 12,
                pid: Some(3)
            }
        );
        let b: SourceArg = serde_json::from_str(r#""display:\\\\.\\DISPLAY2""#).unwrap();
        assert_eq!(
            b.into_source().unwrap(),
            CaptureSource::Monitor {
                id: r"\\.\DISPLAY2".into()
            }
        );
        let r: SourceArg = serde_json::from_str(
            r#"{"kind":"region","monitorId":"x","rect":{"x":0,"y":0,"width":0.5,"height":0.5}}"#,
        )
        .unwrap();
        assert!(matches!(
            r.into_source().unwrap(),
            CaptureSource::Region { .. }
        ));
        let bad: SourceArg = serde_json::from_str(r#""nope""#).unwrap();
        assert!(bad.into_source().is_err());
    }

    #[test]
    fn errors_are_code_message_objects() {
        let e: CommandError = Error::InvalidPath("x".into()).into();
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            json!({ "code": "invalid_path", "message": "invalid path: x" })
        );
        let _ = ExportRequest::default();
    }
}
