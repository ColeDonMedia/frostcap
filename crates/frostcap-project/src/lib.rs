//! # frostcap-project
//!
//! The on-disk Frostcap project format: the `project.json` types, their serde
//! (de)serialization, validation of untrusted input, atomic save and the media
//! file-name constants. It depends on `serde` and `serde_json` only (no OS code),
//! so both the capture core and the export renderer build on one schema.
//!
//!
//! A project is a folder named `<name>.frostcap/` containing:
//!
//! | file                | required | contents                                             |
//! |---------------------|----------|------------------------------------------------------|
//! | `project.json`      | yes      | [`Project`] serialized with serde (camelCase keys)   |
//! | `screen.mp4`        | yes      | H.264 screen recording (OS Media Foundation encoder) |
//! | `webcam.mp4`        | no       | webcam recording (reserved, not produced yet)        |
//! | `audio-mic.wav`     | no       | microphone, WASAPI shared-mode mix format            |
//! | `audio-system.wav`  | no       | system audio, WASAPI loopback                        |
//!
//! ## Time base
//! Every timestamp in the metadata tracks (`tMs`) is **milliseconds since the first
//! video frame** of `screen.mp4` (t = 0 is the first presented frame). Input events
//! that happened before the first frame are dropped. Audio files carry an
//! `offsetMs` describing where their first sample sits on that same clock.
//!
//! ## Coordinate space
//! All positions are **physical pixels relative to the top-left of the captured
//! source** (the monitor or the window), i.e. the same pixel grid as `screen.mp4`
//! (`canvas`). Values can be negative / exceed the canvas when the cursor is outside
//! the captured area.
//!
//! The JSON uses camelCase keys so the TypeScript UI can consume it directly.

#![warn(missing_docs)]

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use std::fmt;

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors from loading, validating or saving a project.
#[derive(Debug)]
pub enum Error {
    /// Filesystem error on a specific path.
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// JSON serialization failed.
    Json(serde_json::Error),
    /// project.json was written by a newer Frostcap (or is version 0).
    UnsupportedVersion(u32),
    /// project.json is malformed, oversized or out of range.
    InvalidProject(String),
}

impl Error {
    /// Wrap an I/O error with its path.
    pub fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "I/O error on {}: {source}", path.display()),
            Self::Json(e) => write!(f, "JSON error: {e}"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported project version {v}"),
            Self::InvalidProject(m) => write!(f, "invalid project: {m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

/// Current `project.json` schema version. Bump on breaking changes.
pub const PROJECT_VERSION: u32 = 1;
/// File name of the project manifest inside a project folder.
pub const PROJECT_FILE: &str = "project.json";
/// File name of the screen recording inside a project folder.
pub const SCREEN_FILE: &str = "screen.mp4";
/// File name of the (optional) webcam recording inside a project folder.
pub const WEBCAM_FILE: &str = "webcam.mp4";
/// File name of the (optional) microphone recording inside a project folder.
pub const MIC_AUDIO_FILE: &str = "audio-mic.wav";
/// File name of the (optional) system-audio recording inside a project folder.
pub const SYSTEM_AUDIO_FILE: &str = "audio-system.wav";
/// Folder extension for projects.
pub const PROJECT_EXT: &str = "frostcap";

/// Root object of `project.json`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    /// Schema version, see [`PROJECT_VERSION`].
    pub version: u32,
    /// Human readable name (defaults to the folder name without extension).
    pub name: String,
    /// Creation time, Unix epoch milliseconds (UTC).
    pub created_at_ms: u64,
    /// What was recorded.
    pub source: SourceDescriptor,
    /// Pixel size of `screen.mp4` (and of the coordinate space of all tracks).
    pub canvas: Size,
    /// Requested capture frame rate. Windows Graphics Capture only delivers frames
    /// when the screen changes, so the real stream is variable-frame-rate up to this.
    pub fps: u32,
    /// Duration of the recording on the video clock, in milliseconds.
    pub duration_ms: u64,
    /// Media files inside the project folder (paths relative to the folder).
    pub media: MediaFiles,
    /// Recorded input metadata.
    pub tracks: Tracks,
    /// Non-destructive edits authored in the editor.
    #[serde(default)]
    pub edits: EditList,
    /// Pauses during recording. Paused time is *removed* from the output timeline
    /// (all `tMs` values and the media files skip it); each gap records where on
    /// the output timeline recording resumed and how long the pause lasted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pauses: Vec<PauseGap>,
    /// `recording` while a session owns the folder (or it crashed), `complete`
    /// after a clean stop, `recovered` after crash recovery.
    #[serde(default)]
    pub status: ProjectStatus,
    /// Non-fatal problems recorded during capture (shown by the editor).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// The system cursor is baked into `screen.mp4` (then the editor must not draw
    /// its own cursor on top).
    #[serde(default)]
    pub cursor_baked: bool,
}

/// Lifecycle state of a project folder.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum ProjectStatus {
    /// A recording session owns the folder (or crashed while owning it).
    Recording,
    /// Cleanly finished.
    #[default]
    Complete,
    /// Reconstructed after a crash; media may be partial.
    Recovered,
}

/// A pause on the output timeline.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PauseGap {
    /// Output-timeline position (ms) where recording resumed after the pause.
    pub at_ms: u64,
    /// Wall-clock length of the pause (ms).
    pub duration_ms: u64,
}

/// A secondary video file (webcam) on the output timeline.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoTrack {
    /// Relative file name (basename).
    pub file: String,
    /// Device name.
    pub name: String,
    /// Frame width.
    pub width: u32,
    /// Frame height.
    pub height: u32,
    /// Where the first frame sits on the output timeline (ms).
    pub offset_ms: i64,
    /// Duration (ms).
    pub duration_ms: u64,
}

/// What was captured.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceDescriptor {
    /// `"monitor"` or `"window"`.
    pub kind: SourceKind,
    /// Display name (monitor friendly name or window title) at record time.
    pub name: String,
    /// Bounds of the source on the virtual desktop at record start (physical px).
    pub bounds: Rect,
    /// Region capture: the recorded rectangle relative to `bounds` (source pixels).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<Rect>,
}

/// Kind of captured source.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SourceKind {
    /// A whole monitor.
    Monitor,
    /// A single top-level window.
    Window,
    /// A rectangle of a monitor.
    Region,
}

/// Width/height in pixels.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Size {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Axis-aligned rectangle in pixels.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub width: i32,
    /// Height.
    pub height: i32,
}

/// Media files of the project (relative file names).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct MediaFiles {
    /// Screen recording, normally `screen.mp4`.
    pub screen: String,
    /// Webcam recording, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webcam: Option<String>,
    /// Webcam recording details, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webcam_video: Option<VideoTrack>,
    /// Microphone audio, if recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_audio: Option<AudioTrack>,
    /// System (loopback) audio, if recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_audio: Option<AudioTrack>,
}

/// A WAV audio file plus its placement on the video clock.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AudioTrack {
    /// Relative file name.
    pub file: String,
    /// Where the first audio sample sits on the video clock (ms, may be negative).
    pub offset_ms: i64,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u16,
    /// Audio duration in ms.
    pub duration_ms: u64,
}

/// Recorded input metadata, all sorted by `tMs`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Tracks {
    /// Cursor position samples (~120 Hz, only stored when the position changes).
    pub cursor: Vec<CursorSample>,
    /// Mouse button transitions.
    pub clicks: Vec<ClickEvent>,
    /// Key-down events (empty when keystroke capture was disabled).
    pub keys: Vec<KeyEvent>,
    /// Foreground-window changes (new window or its rect moved/resized).
    pub focus: Vec<FocusChange>,
}

/// One cursor position sample.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CursorSample {
    /// Time on the video clock (ms).
    pub t_ms: u64,
    /// X in source pixels.
    pub x: i32,
    /// Y in source pixels.
    pub y: i32,
}

/// Mouse button.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MouseButton {
    /// Primary button.
    Left,
    /// Secondary button.
    Right,
    /// Wheel button.
    Middle,
}

/// Whether a button went down or up.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ButtonState {
    /// Pressed.
    Down,
    /// Released.
    Up,
}

/// A mouse button transition.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClickEvent {
    /// Time on the video clock (ms).
    pub t_ms: u64,
    /// X in source pixels.
    pub x: i32,
    /// Y in source pixels.
    pub y: i32,
    /// Which button.
    pub button: MouseButton,
    /// Down or up.
    pub state: ButtonState,
}

/// Modifier keys held during a key press.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Modifiers {
    /// Either Ctrl.
    pub ctrl: bool,
    /// Either Shift.
    pub shift: bool,
    /// Either Alt.
    pub alt: bool,
    /// Either Windows key.
    pub win: bool,
}

/// A key-down event. Only key presses (never releases, never text) are recorded.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KeyEvent {
    /// Time on the video clock (ms).
    pub t_ms: u64,
    /// Windows virtual-key code.
    pub vk: u16,
    /// Display label for the key overlay ("A", "Enter", "F5", ...).
    pub label: String,
    /// Modifiers held at the time.
    pub modifiers: Modifiers,
}

/// The foreground window changed (or moved/resized).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FocusChange {
    /// Time on the video clock (ms).
    pub t_ms: u64,
    /// Opaque per-session window id (1, 2, ...; equal ids = same window).
    #[serde(default)]
    pub window_id: u32,
    /// Window title; empty unless title capture was opted in.
    #[serde(default)]
    pub title: String,
    /// Window bounds in source pixels.
    pub rect: Rect,
}

/// Non-destructive edits. Empty after recording; owned by the editor.
///
/// The editor-only groups (frame, cursor, webcam, audio, captions, splits) are
/// typed so the exporter renders exactly what the editor previews. Projects saved
/// before they existed carry the same values in the opaque `ui` blob
/// (`{"v":1, ...}`); on deserialization every group that is *absent* is migrated
/// from that blob, groups that are present always win.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase", try_from = "serde_json::Value")]
pub struct EditList {
    /// Zoom segments (sorted, non-overlapping).
    pub zoom_segments: Vec<ZoomSegment>,
    /// Ranges of the timeline to cut out.
    pub trims: Vec<TrimRange>,
    /// Visual style of the composed output.
    pub style: Style,
    /// Caption cues (video clock).
    pub captions: Vec<CaptionCue>,
    /// Split points (ms, video clock): clip boundaries inside kept ranges. They
    /// have no visual effect; they are stored so the editor timeline round-trips.
    pub splits: Vec<u64>,
    /// Speed changes (source clock, never overlapping). Parts of the timeline
    /// outside every segment play at 1x. Applies to video and audio.
    pub speed: Vec<SpeedSegment>,
    /// On-screen annotations (arrows, text, shapes, spotlight, blur...).
    pub annotations: Vec<Annotation>,
    /// Opaque editor-only state that is purely cosmetic (zoom defaults...).
    /// Round-tripped verbatim; size-capped by [`Project::validate`].
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    pub ui: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditListWire {
    #[serde(default)]
    zoom_segments: Vec<ZoomSegment>,
    #[serde(default)]
    trims: Vec<TrimRange>,
    #[serde(default)]
    style: Style,
    #[serde(default)]
    captions: Vec<CaptionCue>,
    #[serde(default)]
    splits: Vec<u64>,
    #[serde(default)]
    speed: Vec<SpeedSegment>,
    #[serde(default)]
    annotations: Vec<Annotation>,
    #[serde(default)]
    ui: serde_json::Value,
}

/// Read `key` of a legacy `edits.ui` blob as `T` (lenient: bad shapes give `None`).
fn legacy<T: serde::de::DeserializeOwned>(ui: &serde_json::Value, key: &str) -> Option<T> {
    T::deserialize(ui.get(key)?).ok()
}

impl TryFrom<serde_json::Value> for EditList {
    type Error = String;

    fn try_from(v: serde_json::Value) -> std::result::Result<Self, String> {
        let w = EditListWire::deserialize(&v).map_err(|e| e.to_string())?;
        let mut e = Self {
            zoom_segments: w.zoom_segments,
            trims: w.trims,
            style: w.style,
            captions: w.captions,
            splits: w.splits,
            speed: w.speed,
            annotations: w.annotations,
            ui: w.ui,
        };
        if e.ui.get("v").and_then(serde_json::Value::as_u64) != Some(1) {
            return Ok(e);
        }
        let present = |obj: Option<&serde_json::Value>, k: &str| {
            obj.and_then(|o| o.get(k)).is_some_and(|x| !x.is_null())
        };
        let style = v.get("style");
        let ui = &e.ui;
        let s = &mut e.style;
        if !present(style, "frame") {
            s.frame = legacy(ui, "frame").unwrap_or(s.frame);
        }
        if !present(style, "cursor") {
            s.cursor = legacy::<CursorStyle>(ui, "cursor").map_or(s.cursor, CursorStyle::sanitized);
        }
        if !present(style, "webcam") {
            s.webcam = legacy::<WebcamStyle>(ui, "webcam")
                .map_or_else(|| s.webcam.clone(), WebcamStyle::sanitized);
        }
        if !present(style, "audio") {
            s.audio = legacy::<AudioStyle>(ui, "audio").map_or(s.audio, AudioStyle::sanitized);
        }
        if !present(style, "captions") {
            s.captions =
                legacy::<CaptionStyle>(ui, "captions").map_or(s.captions, CaptionStyle::sanitized);
        }
        if !present(Some(&v), "captions")
            && let Some(cues) = ui.get("captionCues").and_then(serde_json::Value::as_array)
        {
            e.captions = cues
                .iter()
                .filter_map(legacy_cue)
                .take(limits::MAX_EDITS)
                .collect();
        }
        if !present(Some(&v), "splits")
            && let Some(a) = ui.get("splits").and_then(serde_json::Value::as_array)
        {
            e.splits = a.iter().filter_map(ms).take(limits::MAX_EDITS).collect();
        }
        Ok(e)
    }
}

/// Clamp into `lo..=hi`; non-finite values fall back to `default`.
fn clamp_or(v: f64, lo: f64, hi: f64, default: f64) -> f64 {
    if v.is_finite() {
        v.clamp(lo, hi)
    } else {
        default
    }
}

impl CursorStyle {
    fn sanitized(self) -> Self {
        let d = Self::default();
        Self {
            visible: self.visible,
            smoothing: clamp_or(self.smoothing, 0.0, 1.0, d.smoothing),
            motion_blur: clamp_or(self.motion_blur, 0.0, 1.0, d.motion_blur),
        }
    }
}

impl WebcamStyle {
    fn sanitized(self) -> Self {
        Self {
            size: clamp_or(self.size, 0.05, 0.6, Self::default().size),
            margin: clamp_or(self.margin, 0.0, 0.2, Self::DEFAULT_MARGIN),
            border_width: clamp_or(self.border_width, 0.0, 20.0, 0.0),
            brightness: clamp_or(self.brightness, -1.0, 1.0, 0.0),
            contrast: clamp_or(self.contrast, 0.0, 2.0, 1.0),
            saturation: clamp_or(self.saturation, 0.0, 2.0, 1.0),
            shadow: clamp_or(self.shadow, 0.0, 1.0, 0.4),
            ..self
        }
    }
}

impl AudioStyle {
    fn sanitized(self) -> Self {
        let d = Self::default();
        Self {
            noise_gate_threshold: clamp_or(
                self.noise_gate_threshold,
                -100.0,
                0.0,
                d.noise_gate_threshold,
            ),
            high_pass_hz: clamp_or(self.high_pass_hz, 10.0, 1000.0, d.high_pass_hz),
            target_lufs: clamp_or(self.target_lufs, -40.0, -5.0, d.target_lufs),
            mic_gain: clamp_or(self.mic_gain, 0.0, 4.0, d.mic_gain),
            system_gain: clamp_or(self.system_gain, 0.0, 4.0, d.system_gain),
            ..self
        }
    }
}

impl CaptionStyle {
    fn sanitized(self) -> Self {
        Self {
            font_size: clamp_or(self.font_size, 8.0, 200.0, Self::default().font_size),
            ..self
        }
    }
}

/// A finite number of milliseconds (negative clamps to 0) -> whole ms.
fn ms(v: &serde_json::Value) -> Option<u64> {
    v.as_f64()
        .filter(|x| x.is_finite())
        .map(|x| (x.max(0.0).round() as u64).min(limits::MAX_DURATION_MS))
}

/// A legacy cue `{start,end,text}`: inverted ranges are swapped, oversized text
/// is cut at a char boundary, malformed cues are dropped.
fn legacy_cue(c: &serde_json::Value) -> Option<CaptionCue> {
    let (a, b) = (ms(c.get("start")?)?, ms(c.get("end")?)?);
    let mut text = c.get("text")?.as_str()?.to_owned();
    let mut cut = limits::MAX_STRING.min(text.len());
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    Some(CaptionCue {
        start_ms: a.min(b),
        end_ms: a.max(b),
        text,
    })
}

/// A caption cue on the video clock.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CaptionCue {
    /// Start (ms).
    pub start_ms: u64,
    /// End (ms).
    pub end_ms: u64,
    /// Text (<= [`limits::MAX_STRING`] bytes).
    pub text: String,
}

/// Window frame drawn around the recording.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum WindowFrame {
    /// No title bar.
    None,
    /// Thin bar with a centred pill.
    #[default]
    Minimal,
    /// Windows 11 style caption buttons.
    Windows,
    /// Traffic lights.
    #[serde(rename = "macos")]
    MacOs,
    /// Laptop mockup: screen bezel on a keyboard base (drawn procedurally).
    Laptop,
    /// Phone mockup: thick rounded bezel with a camera pill (drawn procedurally).
    Phone,
}

/// Webcam bubble shape.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum WebcamShape {
    /// Circle.
    #[default]
    Circle,
    /// Rounded square.
    Rounded,
    /// Square.
    Square,
}

/// Output corner.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Corner {
    /// Top left.
    TopLeft,
    /// Top right.
    TopRight,
    /// Bottom left.
    BottomLeft,
    /// Bottom right.
    #[default]
    BottomRight,
}

/// Where captions sit.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum CaptionPosition {
    /// Bottom of the output.
    #[default]
    Bottom,
    /// Top of the output.
    Top,
}

/// Re-rendered cursor options (size, clicks and the smoothing on/off switch are
/// the flat [`Style`] fields `cursorScale`, `showClicks`, `cursorSmoothing`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct CursorStyle {
    /// Draw the re-rendered cursor (never drawn over a baked cursor).
    pub visible: bool,
    /// Smoothing amount 0..1 (used while `Style::cursor_smoothing` is on).
    pub smoothing: f64,
    /// Motion blur 0..1.
    pub motion_blur: f64,
}

impl Default for CursorStyle {
    fn default() -> Self {
        Self {
            visible: true,
            smoothing: 0.6,
            motion_blur: 0.3,
        }
    }
}

/// Webcam bubble.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct WebcamStyle {
    /// Draw the bubble.
    pub enabled: bool,
    /// Shape.
    pub shape: WebcamShape,
    /// Diameter as a fraction of the output height (0.05..=0.6).
    pub size: f64,
    /// Corner of the output.
    pub position: Corner,
    /// Distance from the output edge as a fraction of the output height
    /// (0..=0.2). The default is the original 28 px at 1080p.
    pub margin: f64,
    /// Border width in px at 1080p output (0..=20, 0 = none), scaled with the
    /// output. Drawn inside the bubble's rectangle, so `size` stays the outer diameter.
    pub border_width: f64,
    /// Border colour (`#RRGGBB`).
    pub border_color: Rgba,
    /// Flip the camera horizontally.
    pub mirror: bool,
    /// Brightness offset (-1..=1, 0 = unchanged).
    pub brightness: f64,
    /// Contrast (0..=2, 1 = unchanged).
    pub contrast: f64,
    /// Saturation (0..=2, 1 = unchanged, 0 = greyscale).
    pub saturation: f64,
    /// Drop shadow strength (0..=1; the default 0.4 is the original look).
    pub shadow: f64,
    /// Chroma key (green screen) removal.
    pub chroma_key: ChromaKey,
    /// What shows where the camera was keyed out.
    pub background: WebcamBackground,
}

/// Chroma key settings (a colour distance mask, no segmentation).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ChromaKey {
    /// Key on.
    pub enabled: bool,
    /// Colour to remove (`#RRGGBB`).
    pub color: Rgba,
    /// How close to `color` counts as background (0..=1).
    pub similarity: f64,
    /// Edge softness (0..=1).
    pub smoothness: f64,
    /// Spill suppression: pulls the key colour out of the kept edges (0..=1).
    pub spill: f64,
}

impl Default for ChromaKey {
    fn default() -> Self {
        Self {
            enabled: false,
            color: Rgba([0, 255, 0, 255]),
            similarity: 0.4,
            smoothness: 0.1,
            spill: 0.5,
        }
    }
}

/// Replacement for keyed-out camera pixels.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum WebcamBackgroundKind {
    /// Transparent (the recording shows through).
    #[default]
    None,
    /// `color`.
    Color,
    /// `gradient`.
    Gradient,
    /// `image`.
    Image,
    /// A blurred copy of the camera frame.
    Blur,
}

/// Webcam background replacement (uses the chroma-key mask).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct WebcamBackground {
    /// Kind.
    pub kind: WebcamBackgroundKind,
    /// Fill colour for `color`.
    pub color: Rgba,
    /// CSS gradient for `gradient` (same format as `style.background`).
    pub gradient: String,
    /// Project-folder image basename for `image` (same rules as `backgroundImage`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Blur amount for `blur` (0..=1).
    pub blur: f64,
}

impl Default for WebcamBackground {
    fn default() -> Self {
        Self {
            kind: WebcamBackgroundKind::None,
            color: Rgba([17, 20, 28, 255]),
            gradient: "linear-gradient(135deg,#a1c4fd,#c2e9fb)".to_owned(),
            image: None,
            blur: 0.5,
        }
    }
}

impl WebcamStyle {
    /// Default `margin`: 28 px at 1080p, what the exporter always used.
    pub const DEFAULT_MARGIN: f64 = 28.0 / 1080.0;
}

impl Default for WebcamStyle {
    fn default() -> Self {
        Self {
            enabled: false,
            shape: WebcamShape::Circle,
            size: 0.22,
            position: Corner::BottomRight,
            margin: Self::DEFAULT_MARGIN,
            border_width: 0.0,
            border_color: Rgba([255, 255, 255, 255]),
            mirror: false,
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            shadow: 0.4,
            chroma_key: ChromaKey::default(),
            background: WebcamBackground::default(),
        }
    }
}

/// An sRGB colour with alpha, written as `#rrggbb` (opaque) or `#rrggbbaa`;
/// `#rgb` is accepted on read too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba(pub [u8; 4]);

impl Serialize for Rgba {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let [r, g, b, a] = self.0;
        if a == 255 {
            s.serialize_str(&format!("#{r:02x}{g:02x}{b:02x}"))
        } else {
            s.serialize_str(&format!("#{r:02x}{g:02x}{b:02x}{a:02x}"))
        }
    }
}

impl<'de> Deserialize<'de> for Rgba {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("bad colour {s:?}")))
    }
}

impl Rgba {
    /// Parse `#rgb`, `#rrggbb` or `#rrggbbaa`.
    pub fn parse(s: &str) -> Option<Self> {
        let h = s.strip_prefix('#')?;
        if !h.is_ascii() {
            return None;
        }
        let hex = |i: usize, n: usize| u8::from_str_radix(h.get(i..i + n)?, 16).ok();
        match h.len() {
            3 => Some(Self([
                hex(0, 1)? * 17,
                hex(1, 1)? * 17,
                hex(2, 1)? * 17,
                255,
            ])),
            6 => Some(Self([hex(0, 2)?, hex(2, 2)?, hex(4, 2)?, 255])),
            8 => Some(Self([hex(0, 2)?, hex(2, 2)?, hex(4, 2)?, hex(6, 2)?])),
            _ => None,
        }
    }
}

/// Audio mix and processing.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AudioStyle {
    /// Noise gate on the microphone.
    pub noise_gate: bool,
    /// Gate threshold, dBFS (-100..=0).
    pub noise_gate_threshold: f64,
    /// High-pass on the microphone.
    pub high_pass: bool,
    /// High-pass corner, Hz (10..=1000).
    pub high_pass_hz: f64,
    /// Loudness-normalize the final mix.
    pub normalize: bool,
    /// Target integrated loudness, LUFS (-40..=-5).
    pub target_lufs: f64,
    /// Microphone gain, linear (0 = muted, 0..=4).
    pub mic_gain: f64,
    /// System audio gain, linear (0 = muted, 0..=4).
    pub system_gain: f64,
}

impl Default for AudioStyle {
    fn default() -> Self {
        Self {
            noise_gate: true,
            noise_gate_threshold: -50.0,
            high_pass: true,
            high_pass_hz: 80.0,
            normalize: true,
            target_lufs: -16.0,
            mic_gain: 1.0,
            system_gain: 1.0,
        }
    }
}

/// Caption appearance.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct CaptionStyle {
    /// Draw captions.
    pub enabled: bool,
    /// Font size, px at 1080p (8..=200).
    pub font_size: f64,
    /// Top or bottom.
    pub position: CaptionPosition,
    /// Dark box behind the text.
    pub background: bool,
}

impl Default for CaptionStyle {
    fn default() -> Self {
        Self {
            enabled: true,
            font_size: 40.0,
            position: CaptionPosition::Bottom,
            background: true,
        }
    }
}

/// Easing curve used when zooming in/out.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum Easing {
    /// Constant speed.
    Linear,
    /// Cubic ease-in-out.
    #[default]
    EaseInOutCubic,
    /// Quintic ease-out (fast start, long settle).
    EaseOutQuint,
    /// Critically-damped spring feel.
    Spring,
}

/// Where a zoom segment came from.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum SegmentOrigin {
    /// Generated automatically (auto-zoom).
    #[default]
    Auto,
    /// Authored by the user.
    Manual,
}

/// Normalized point: `0.0..=1.0` across the canvas (0,0 = top-left).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct NormPoint {
    /// Horizontal, 0..1.
    pub x: f64,
    /// Vertical, 0..1.
    pub y: f64,
}

/// A zoom on part of the canvas during `[startMs, endMs)`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ZoomSegment {
    /// Start on the video clock (ms). Zoom-in transition begins here.
    pub start_ms: u64,
    /// End on the video clock (ms). Zoom-out transition ends here.
    pub end_ms: u64,
    /// Zoom focus, normalized to the canvas. Already clamped so the zoomed
    /// viewport stays inside the canvas.
    pub center: NormPoint,
    /// Magnification (1.0 = none, at most 8.0).
    pub scale: f64,
    /// Transition curve.
    pub easing: Easing,
    /// Length of the zoom-in and zoom-out transitions (ms each).
    pub transition_ms: u64,
    /// Auto or manual.
    #[serde(default)]
    pub origin: SegmentOrigin,
}

/// A cut: `[startMs, endMs)` is removed from the output.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrimRange {
    /// Start (ms, video clock).
    pub start_ms: u64,
    /// End (ms, video clock).
    pub end_ms: u64,
}

/// Output styling (background, padding, rounded corners, shadow, cursor).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Style {
    /// Background: CSS-like colour or gradient spec, e.g. `"#0b1020"` or
    /// `"linear-gradient(135deg,#8ec5fc,#e0c3fc)"`.
    pub background: String,
    /// Image background (stored in the project folder) drawn over `background`,
    /// which stays visible behind a `contain` fit. Absent = gradient/colour only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_image: Option<BackgroundImage>,
    /// Padding around the recording as a fraction of the output size (0..=0.45).
    pub padding: f64,
    /// Corner radius in output pixels.
    pub corner_radius: f64,
    /// Drop shadow strength 0..1.
    pub shadow: f64,
    /// Cursor scale multiplier for the re-rendered cursor (0.1..=8.0).
    pub cursor_scale: f64,
    /// Smooth (spring-filtered) cursor motion.
    pub cursor_smoothing: bool,
    /// Show click ripples.
    pub show_clicks: bool,
    /// Show the keystroke overlay.
    pub show_keys: bool,
    /// Window frame around the recording.
    pub frame: WindowFrame,
    /// Cursor visibility, smoothing amount and motion blur.
    pub cursor: CursorStyle,
    /// Webcam bubble.
    pub webcam: WebcamStyle,
    /// Audio mix and processing.
    pub audio: AudioStyle,
    /// Caption appearance.
    pub captions: CaptionStyle,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            background: "linear-gradient(135deg,#a1c4fd,#c2e9fb)".to_owned(),
            background_image: None,
            padding: 0.08,
            corner_radius: 12.0,
            shadow: 0.5,
            cursor_scale: 1.0,
            cursor_smoothing: true,
            show_clicks: true,
            show_keys: false,
            frame: WindowFrame::default(),
            cursor: CursorStyle::default(),
            webcam: WebcamStyle::default(),
            audio: AudioStyle::default(),
            captions: CaptionStyle::default(),
        }
    }
}

/// How a background image fills the output.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum BackgroundFit {
    /// Fill the output, cropping the overflow.
    #[default]
    Cover,
    /// Fit inside the output; the gradient/colour shows in the letterbox.
    Contain,
}

/// An image background stored inside the project folder.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundImage {
    /// Plain file name (`.png`, `.jpg`, `.jpeg`) inside the project folder; see
    /// [`import_background`].
    pub file: String,
    /// Fit mode.
    #[serde(default)]
    pub fit: BackgroundFit,
    /// Gaussian blur, sigma in px at 1080p (0..=100, 0 = sharp).
    #[serde(default)]
    pub blur: f64,
}

/// Annotation shape.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum AnnotationKind {
    /// Arrow from (x, y) to (x2, y2).
    #[default]
    Arrow,
    /// Text in the (x, y, w, h) box.
    Text,
    /// Rectangle outline / fill.
    Rect,
    /// Ellipse outline / fill.
    Ellipse,
    /// Translucent marker rectangle.
    Highlight,
    /// Darkens the screen outside the rectangle.
    Spotlight,
    /// Blurs the rectangle.
    Blur,
    /// Pixelates the rectangle.
    Pixelate,
}

/// An annotation drawn over the screen content. Geometry is normalized
/// (0..1) in screen-content coordinates, so it moves with zoom and pan.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Annotation {
    /// Editor id (<= 64 bytes).
    pub id: String,
    /// Shape.
    pub kind: AnnotationKind,
    /// Start (ms, video clock).
    pub start_ms: u64,
    /// End (ms, video clock, >= start, <= 24 h).
    pub end_ms: u64,
    /// Left / arrow start x, 0..=1.
    pub x: f64,
    /// Top / arrow start y, 0..=1.
    pub y: f64,
    /// Width, 0..=1.
    pub w: f64,
    /// Height, 0..=1.
    pub h: f64,
    /// Arrow end x, 0..=1.
    pub x2: f64,
    /// Arrow end y, 0..=1.
    pub y2: f64,
    /// Colour (`#RRGGBB`).
    pub color: Rgba,
    /// Stroke width, px at 1080p output (1..=40).
    pub stroke_width: f64,
    /// Fill the shape.
    pub fill: bool,
    /// Text (<= 500 chars; kind `text` only).
    pub text: String,
    /// Font size, px at 1080p (8..=200).
    pub font_size: f64,
    /// Opacity 0..=1.
    pub opacity: f64,
    /// Blur radius / pixel block size for blur and pixelate (0..=1).
    pub strength: f64,
    /// Spotlight darkening outside the rectangle (0..=1).
    pub dim: f64,
    /// Corner radius, px at 1080p (0..=200).
    pub corner_radius: f64,
    /// Fade in/out length (ms, 0..=2000).
    pub fade_ms: u64,
}

impl Default for Annotation {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: AnnotationKind::Arrow,
            start_ms: 0,
            end_ms: 0,
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
            x2: 0.0,
            y2: 0.0,
            color: Rgba([0x4e, 0xd2, 0xfb, 255]),
            stroke_width: 4.0,
            fill: false,
            text: String::new(),
            font_size: 40.0,
            opacity: 1.0,
            strength: 0.5,
            dim: 0.6,
            corner_radius: 0.0,
            fade_ms: 150,
        }
    }
}

impl Annotation {
    fn validate(&self) -> Result<()> {
        if self.id.len() > 64 {
            return Err(invalid("annotation id is too long"));
        }
        if self.end_ms < self.start_ms || self.end_ms > limits::MAX_DURATION_MS {
            return Err(invalid("annotation time range is invalid"));
        }
        for (f, v) in [
            ("x", self.x),
            ("y", self.y),
            ("w", self.w),
            ("h", self.h),
            ("x2", self.x2),
            ("y2", self.y2),
            ("opacity", self.opacity),
            ("strength", self.strength),
            ("dim", self.dim),
        ] {
            check_range(&format!("annotation.{f}"), v, 0.0, 1.0)?;
        }
        check_range("annotation.strokeWidth", self.stroke_width, 1.0, 40.0)?;
        check_range("annotation.fontSize", self.font_size, 8.0, 200.0)?;
        check_range("annotation.cornerRadius", self.corner_radius, 0.0, 200.0)?;
        if self.fade_ms > 2000 {
            return Err(invalid("annotation fadeMs is outside 0..=2000"));
        }
        if self.text.chars().count() > limits::MAX_ANNOTATION_TEXT {
            return Err(invalid("annotation text is too long"));
        }
        Ok(())
    }
}

/// A change of playback speed over `[startMs, endMs)` of the source clock.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpeedSegment {
    /// Start (ms, video clock).
    pub start_ms: u64,
    /// End (ms, video clock).
    pub end_ms: u64,
    /// Playback speed, 0.25..=4 (1 = normal). A ramp is several adjacent segments.
    pub speed: f64,
}

/// Resolve a user supplied path (project folder or its `project.json`) to the
/// project folder.
pub fn project_dir_of(path: &Path) -> PathBuf {
    if path.is_file() || path.file_name().is_some_and(|n| n == PROJECT_FILE) {
        path.parent().map(Path::to_path_buf).unwrap_or_default()
    } else {
        path.to_path_buf()
    }
}

/// Hard limits enforced by [`Project::validate`] and by the recorders.
pub mod limits {
    /// Max size of `project.json` accepted by [`super::Project::load`].
    pub const MAX_PROJECT_BYTES: u64 = 64 * 1024 * 1024;
    /// Max cursor samples (~4.6 h of continuous motion at 120 Hz).
    pub const MAX_CURSOR_SAMPLES: usize = 2_000_000;
    /// Max clicks / keys.
    pub const MAX_EVENTS: usize = 500_000;
    /// Max focus changes.
    pub const MAX_FOCUS: usize = 100_000;
    /// Max zoom segments / trims / pauses / warnings.
    pub const MAX_EDITS: usize = 10_000;
    /// Max string length (names, titles, labels, style strings).
    pub const MAX_STRING: usize = 4096;
    /// Max canvas side.
    pub const MAX_CANVAS: u32 = 16_384;
    /// Max serialized size of the opaque `edits.ui` state.
    pub const MAX_UI_STATE_BYTES: usize = 4 * 1024 * 1024;
    /// Max size of an imported background image (20 MiB).
    pub const MAX_BACKGROUND_BYTES: u64 = 20 * 1024 * 1024;
    /// Max side of a background image.
    pub const MAX_BACKGROUND_SIDE: u32 = 16_384;
    /// Max pixels of a background image (bounds the decoded RGBA buffer to 128 MB).
    pub const MAX_BACKGROUND_PIXELS: u64 = 32_000_000;
    /// Max annotations per project.
    pub const MAX_ANNOTATIONS: usize = 500;
    /// Max annotation text length (characters).
    pub const MAX_ANNOTATION_TEXT: usize = 500;
    /// Max recording duration (24 h): bounds frame lists and audio buffers.
    pub const MAX_DURATION_MS: u64 = 24 * 3600 * 1000;
}

/// Small summary written next to `project.json` so listings avoid parsing
/// full projects.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMeta {
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
}

/// File name of the summary next to `project.json`.
pub const META_FILE: &str = "meta.json";

fn invalid(msg: impl Into<String>) -> Error {
    Error::InvalidProject(msg.into())
}

fn check_str(field: &str, s: &str) -> Result<()> {
    if s.len() > limits::MAX_STRING {
        return Err(invalid(format!("{field} is too long")));
    }
    Ok(())
}

/// Windows reserved device names (with or without an extension).
fn is_reserved_device(stem: &str) -> bool {
    let u = stem.to_ascii_uppercase();
    matches!(
        u.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((u.starts_with("COM") || u.starts_with("LPT"))
        && u.len() == 4
        && u.as_bytes()[3].is_ascii_digit()
        && u.as_bytes()[3] != b'0')
}

fn check_basename(field: &str, s: &str, ext: &str) -> Result<()> {
    check_basename_ext(field, s, &[ext])
}

fn check_basename_ext(field: &str, s: &str, exts: &[&str]) -> Result<()> {
    check_str(field, s)?;
    let stem = s.split('.').next().unwrap_or("");
    let bad = s.is_empty()
        || s == "."
        || s == ".."
        || s.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
        || s.chars().any(char::is_control)
        || s.ends_with(['.', ' '])
        || is_reserved_device(stem.trim_end())
        || !Path::new(s)
            .extension()
            .is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)));
    if bad {
        return Err(invalid(format!(
            "{field} must be a plain file name, got {s:?}"
        )));
    }
    Ok(())
}

fn check_len(field: &str, len: usize, max: usize) -> Result<()> {
    if len > max {
        return Err(invalid(format!("{field} has {len} entries (max {max})")));
    }
    Ok(())
}

fn check_f64(field: &str, v: f64, min: f64, max: f64) -> Result<()> {
    if !v.is_finite() || v < min || v > max {
        return Err(invalid(format!("{field} = {v} is outside {min}..={max}")));
    }
    Ok(())
}

/// Range check for the editor-v2 fields (kept apart from `check_f64` so the UI's
/// drift test, which parses that helper's calls, stays valid until it learns them).
fn check_range(field: &str, v: f64, min: f64, max: f64) -> Result<()> {
    check_f64(field, v, min, max)
}

impl Project {
    /// Structural validation of untrusted input (from disk or the UI).
    pub fn validate(&self) -> Result<()> {
        if self.version == 0 || self.version > PROJECT_VERSION {
            return Err(Error::UnsupportedVersion(self.version));
        }
        check_str("name", &self.name)?;
        check_str("source.name", &self.source.name)?;
        if self.duration_ms > limits::MAX_DURATION_MS {
            return Err(invalid(format!(
                "durationMs {} exceeds the {} ms (24 h) maximum",
                self.duration_ms,
                limits::MAX_DURATION_MS
            )));
        }
        let c = self.canvas;
        if c.width == 0
            || c.height == 0
            || c.width > limits::MAX_CANVAS
            || c.height > limits::MAX_CANVAS
        {
            return Err(invalid(format!(
                "canvas {}x{} is invalid",
                c.width, c.height
            )));
        }
        if self.fps == 0 || self.fps > 240 {
            return Err(invalid(format!("fps {} is invalid", self.fps)));
        }
        check_basename("media.screen", &self.media.screen, "mp4")?;
        if let Some(w) = &self.media.webcam {
            check_basename("media.webcam", w, "mp4")?;
        }
        if let Some(w) = &self.media.webcam_video {
            check_basename("media.webcamVideo.file", &w.file, "mp4")?;
            check_str("media.webcamVideo.name", &w.name)?;
        }
        for (field, a) in [
            ("media.micAudio", &self.media.mic_audio),
            ("media.systemAudio", &self.media.system_audio),
        ] {
            if let Some(a) = a {
                check_basename(field, &a.file, "wav")?;
                if a.sample_rate == 0
                    || a.sample_rate > 768_000
                    || a.channels == 0
                    || a.channels > 32
                {
                    return Err(invalid(format!("{field} format is invalid")));
                }
            }
        }
        let t = &self.tracks;
        check_len("tracks.cursor", t.cursor.len(), limits::MAX_CURSOR_SAMPLES)?;
        check_len("tracks.clicks", t.clicks.len(), limits::MAX_EVENTS)?;
        check_len("tracks.keys", t.keys.len(), limits::MAX_EVENTS)?;
        check_len("tracks.focus", t.focus.len(), limits::MAX_FOCUS)?;
        for k in &t.keys {
            check_str("tracks.keys.label", &k.label)?;
        }
        for f in &t.focus {
            check_str("tracks.focus.title", &f.title)?;
        }
        let e = &self.edits;
        check_len(
            "edits.zoomSegments",
            e.zoom_segments.len(),
            limits::MAX_EDITS,
        )?;
        check_len("edits.trims", e.trims.len(), limits::MAX_EDITS)?;
        check_len("pauses", self.pauses.len(), limits::MAX_EDITS)?;
        check_len("warnings", self.warnings.len(), limits::MAX_EDITS)?;
        for w in &self.warnings {
            check_str("warnings", w)?;
        }
        for z in &e.zoom_segments {
            if z.end_ms < z.start_ms {
                return Err(invalid("zoom segment ends before it starts"));
            }
            check_f64("zoomSegment.center.x", z.center.x, 0.0, 1.0)?;
            check_f64("zoomSegment.center.y", z.center.y, 0.0, 1.0)?;
            check_f64("zoomSegment.scale", z.scale, 1.0, 8.0)?;
        }
        for r in &e.trims {
            if r.end_ms < r.start_ms {
                return Err(invalid("trim ends before it starts"));
            }
        }
        if !e.ui.is_null() {
            let n = serde_json::to_vec(&e.ui).map_or(usize::MAX, |v| v.len());
            if n > limits::MAX_UI_STATE_BYTES {
                return Err(invalid(format!(
                    "edits.ui is {n} bytes (max {})",
                    limits::MAX_UI_STATE_BYTES
                )));
            }
        }
        let s = &e.style;
        check_str("style.background", &s.background)?;
        check_f64("style.padding", s.padding, 0.0, 0.45)?;
        check_f64("style.cornerRadius", s.corner_radius, 0.0, 4096.0)?;
        check_f64("style.shadow", s.shadow, 0.0, 1.0)?;
        check_f64("style.cursorScale", s.cursor_scale, 0.1, 8.0)?;
        check_f64("style.cursor.smoothing", s.cursor.smoothing, 0.0, 1.0)?;
        check_f64("style.cursor.motionBlur", s.cursor.motion_blur, 0.0, 1.0)?;
        check_f64("style.webcam.size", s.webcam.size, 0.05, 0.6)?;
        check_range("style.webcam.margin", s.webcam.margin, 0.0, 0.2)?;
        let w = &s.webcam;
        check_range("style.webcam.borderWidth", w.border_width, 0.0, 20.0)?;
        check_range("style.webcam.brightness", w.brightness, -1.0, 1.0)?;
        check_range("style.webcam.contrast", w.contrast, 0.0, 2.0)?;
        check_range("style.webcam.saturation", w.saturation, 0.0, 2.0)?;
        check_range("style.webcam.shadow", w.shadow, 0.0, 1.0)?;
        check_range(
            "style.webcam.chromaKey.similarity",
            w.chroma_key.similarity,
            0.0,
            1.0,
        )?;
        check_range(
            "style.webcam.chromaKey.smoothness",
            w.chroma_key.smoothness,
            0.0,
            1.0,
        )?;
        check_range("style.webcam.chromaKey.spill", w.chroma_key.spill, 0.0, 1.0)?;
        check_range("style.webcam.background.blur", w.background.blur, 0.0, 1.0)?;
        check_str("style.webcam.background.gradient", &w.background.gradient)?;
        if let Some(img) = &w.background.image {
            check_basename_ext(
                "style.webcam.background.image",
                img,
                &["png", "jpg", "jpeg"],
            )?;
        }
        check_len(
            "edits.annotations",
            e.annotations.len(),
            limits::MAX_ANNOTATIONS,
        )?;
        for a in &e.annotations {
            a.validate()?;
        }
        if let Some(b) = &s.background_image {
            check_basename_ext(
                "style.backgroundImage.file",
                &b.file,
                &["png", "jpg", "jpeg"],
            )?;
            check_range("style.backgroundImage.blur", b.blur, 0.0, 100.0)?;
        }
        check_len("edits.speed", e.speed.len(), limits::MAX_EDITS)?;
        let mut spans: Vec<(u64, u64)> = Vec::with_capacity(e.speed.len());
        for sp in &e.speed {
            if sp.end_ms < sp.start_ms {
                return Err(invalid("speed segment ends before it starts"));
            }
            check_range("speed.speed", sp.speed, 0.25, 4.0)?;
            spans.push((sp.start_ms, sp.end_ms));
        }
        spans.sort_unstable();
        if spans.windows(2).any(|w| w[1].0 < w[0].1) {
            return Err(invalid("speed segments overlap"));
        }
        let a = &s.audio;
        check_f64(
            "style.audio.noiseGateThreshold",
            a.noise_gate_threshold,
            -100.0,
            0.0,
        )?;
        check_f64("style.audio.highPassHz", a.high_pass_hz, 10.0, 1000.0)?;
        check_f64("style.audio.targetLufs", a.target_lufs, -40.0, -5.0)?;
        check_f64("style.audio.micGain", a.mic_gain, 0.0, 4.0)?;
        check_f64("style.audio.systemGain", a.system_gain, 0.0, 4.0)?;
        check_f64("style.captions.fontSize", s.captions.font_size, 8.0, 200.0)?;
        check_len("edits.captions", e.captions.len(), limits::MAX_EDITS)?;
        check_len("edits.splits", e.splits.len(), limits::MAX_EDITS)?;
        for c in &e.captions {
            check_str("edits.captions.text", &c.text)?;
            if c.end_ms < c.start_ms {
                return Err(invalid("caption cue ends before it starts"));
            }
        }
        Ok(())
    }

    /// Load and validate `project.json` from a project folder (or a direct path
    /// to the json). Files over [`limits::MAX_PROJECT_BYTES`] are rejected unread.
    pub fn load(path: &Path) -> Result<Self> {
        let file = project_dir_of(path).join(PROJECT_FILE);
        let len = fs::metadata(&file).map_err(|e| Error::io(&file, e))?.len();
        if len > limits::MAX_PROJECT_BYTES {
            return Err(invalid(format!(
                "project.json is {len} bytes (max {})",
                limits::MAX_PROJECT_BYTES
            )));
        }
        let bytes = fs::read(&file).map_err(|e| Error::io(&file, e))?;
        let project: Project =
            serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
        project.validate()?;
        Ok(project)
    }

    /// Privacy scrub: drop all key events and/or blank every window title
    /// (focus track and, for window sources, the source name).
    pub fn scrub(&mut self, keys: bool, titles: bool) {
        if keys {
            self.tracks.keys.clear();
            self.edits.style.show_keys = false;
        }
        if titles {
            for f in &mut self.tracks.focus {
                f.title.clear();
            }
            if self.source.kind == SourceKind::Window {
                self.source.name = "Window".into();
            }
        }
    }

    /// Summary for listings.
    pub fn meta(&self) -> ProjectMeta {
        ProjectMeta {
            name: self.name.clone(),
            created_at_ms: self.created_at_ms,
            duration_ms: self.duration_ms,
            canvas: self.canvas,
            status: self.status,
        }
    }

    /// Validate, then atomically write compact `project.json` (temp + rename) and
    /// `meta.json` into the project folder.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let dir = project_dir_of(path);
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        write_atomic(&dir.join(PROJECT_FILE), &serde_json::to_vec(self)?)?;
        write_atomic(&dir.join(META_FILE), &serde_json::to_vec(&self.meta())?)?;
        Ok(())
    }
}

/// Image formats accepted as backgrounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
}

impl ImageKind {
    /// File extension used when importing.
    pub fn ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }
}

/// Identify a PNG/JPEG by its magic bytes and read its pixel size from the
/// header (no decoding). Rejects zero and oversized dimensions.
pub fn image_info(b: &[u8]) -> Result<(ImageKind, u32, u32)> {
    let (kind, w, h) = if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        if b.len() < 24 || &b[12..16] != b"IHDR" {
            return Err(invalid("truncated PNG header"));
        }
        let be = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        (ImageKind::Png, be(16), be(20))
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        let (w, h) = jpeg_size(b).ok_or_else(|| invalid("JPEG has no frame header"))?;
        (ImageKind::Jpeg, w, h)
    } else {
        return Err(invalid("not a PNG or JPEG image"));
    };
    if w == 0
        || h == 0
        || w > limits::MAX_BACKGROUND_SIDE
        || h > limits::MAX_BACKGROUND_SIDE
        || u64::from(w) * u64::from(h) > limits::MAX_BACKGROUND_PIXELS
    {
        return Err(invalid(format!("image size {w}x{h} is out of range")));
    }
    Ok((kind, w, h))
}

/// Walk JPEG markers to the first start-of-frame and return (width, height).
fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let m = b[i + 1];
        if m == 0xFF {
            i += 1; // fill byte
            continue;
        }
        if m == 0xD8 || m == 0x01 || (0xD0..=0xD7).contains(&m) {
            i += 2; // standalone markers
            continue;
        }
        let len = usize::from(u16::from_be_bytes([b[i + 2], b[i + 3]]));
        if len < 2 {
            return None;
        }
        if matches!(m, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            let s = b.get(i + 5..i + 9)?;
            return Some((
                u32::from(u16::from_be_bytes([s[2], s[3]])),
                u32::from(u16::from_be_bytes([s[0], s[1]])),
            ));
        }
        i += 2 + len;
    }
    None
}

/// Copy an image into the project folder as a background and return its file
/// name (`background-<hash>.png|jpg`, to store in `style.backgroundImage.file`).
///
/// The source must be a regular `.png`/`.jpg`/`.jpeg` file of at most
/// [`limits::MAX_BACKGROUND_BYTES`] whose *content* is a PNG/JPEG with a sane
/// size (the stored extension follows the content). The name is derived from the
/// bytes, so importing the same image twice is a no-op and a project that still
/// references an older background keeps working.
pub fn import_background(project_dir: &Path, source: &Path) -> Result<String> {
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| invalid("background must be a .png or .jpg file"))?;
    let file = fs::File::open(source).map_err(|e| Error::io(source, e))?;
    import_background_file(project_dir, file, ext)
}

/// [`import_background] on an already-open handle: the caller opens the source
/// once, checks what it needs on the handle, and no later path lookup can swap
/// the file (no check-then-open race). `ext` is the source's extension without
/// the dot (`"png"`, `"jpg"`, `"jpeg"`, any case).
pub fn import_background_file(project_dir: &Path, mut file: fs::File, ext: &str) -> Result<String> {
    use std::io::Read as _;
    let source = Path::new("background");
    if !["png", "jpg", "jpeg"]
        .iter()
        .any(|x| ext.eq_ignore_ascii_case(x))
    {
        return Err(invalid("background must be a .png or .jpg file"));
    }
    let meta = file.metadata().map_err(|e| Error::io(source, e))?;
    if !meta.is_file() {
        return Err(invalid("background must be a regular file"));
    }
    if meta.len() > limits::MAX_BACKGROUND_BYTES {
        return Err(invalid(format!(
            "background is {} bytes (max {})",
            meta.len(),
            limits::MAX_BACKGROUND_BYTES
        )));
    }
    if !project_dir.is_dir() {
        return Err(invalid("project folder does not exist"));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    (&mut file)
        .take(limits::MAX_BACKGROUND_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::io(source, e))?;
    if bytes.len() as u64 > limits::MAX_BACKGROUND_BYTES {
        return Err(invalid("background is too large"));
    }
    let (kind, _, _) = image_info(&bytes)?;
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    // The name is a 64-bit hash, so never trust it: a file that already has the
    // name but other bytes is left alone and a suffixed name is used instead.
    for n in 0..100u32 {
        let name = if n == 0 {
            format!("background-{hash:016x}.{}", kind.ext())
        } else {
            format!("background-{hash:016x}-{n}.{}", kind.ext())
        };
        let path = project_dir.join(&name);
        match fs::read(&path) {
            Ok(existing) if existing == bytes => return Ok(name),
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                write_atomic(&path, &bytes)?;
                return Ok(name);
            }
            Err(e) => return Err(Error::io(&path, e)),
        }
    }
    Err(invalid("too many background images with the same name"))
}

/// Absolute path of a background image inside `project_dir`: the name must be a
/// plain `.png`/`.jpg` basename and the file a regular file within the size cap.
pub fn background_path(project_dir: &Path, b: &BackgroundImage) -> Result<PathBuf> {
    check_basename_ext(
        "style.backgroundImage.file",
        &b.file,
        &["png", "jpg", "jpeg"],
    )?;
    let path = project_dir.join(&b.file);
    let meta = fs::metadata(&path).map_err(|e| Error::io(&path, e))?;
    if !meta.is_file() || meta.len() > limits::MAX_BACKGROUND_BYTES {
        return Err(invalid(format!(
            "background {} is not a regular file within the size cap",
            b.file
        )));
    }
    Ok(path)
}

/// Write `bytes` to `file` via a sibling temp file + rename (+ fsync).
pub fn write_atomic(file: &Path, bytes: &[u8]) -> Result<()> {
    let name = file
        .file_name()
        .map_or_else(|| "out".into(), |n| n.to_string_lossy().into_owned());
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = file.with_file_name(format!(".{name}.{}-{n}.tmp", std::process::id()));
    let res = (|| {
        let mut f = fs::File::create(&tmp).map_err(|e| Error::io(&tmp, e))?;
        f.write_all(bytes).map_err(|e| Error::io(&tmp, e))?;
        f.sync_all().map_err(|e| Error::io(&tmp, e))?;
        drop(f);
        fs::rename(&tmp, file).map_err(|e| Error::io(file, e))
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// Read `meta.json` (cheap), if present and valid.
pub fn read_meta(dir: &Path) -> Option<ProjectMeta> {
    let file = dir.join(META_FILE);
    if fs::metadata(&file).ok()?.len() > 64 * 1024 {
        return None;
    }
    serde_json::from_slice(&fs::read(file).ok()?).ok()
}

/// Example projects for tests and golden fixtures (not used by the app).
#[doc(hidden)]
pub mod samples {
    use super::*;

    /// A small valid project.
    pub fn sample() -> Project {
        Project {
            version: PROJECT_VERSION,
            name: "Test".into(),
            created_at_ms: 1,
            source: SourceDescriptor {
                kind: SourceKind::Monitor,
                name: "Display".into(),
                bounds: Rect {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
                crop: None,
            },
            canvas: Size {
                width: 1920,
                height: 1080,
            },
            fps: 60,
            duration_ms: 3000,
            media: MediaFiles {
                screen: SCREEN_FILE.into(),
                ..Default::default()
            },
            tracks: Tracks {
                cursor: vec![CursorSample {
                    t_ms: 0,
                    x: 1,
                    y: 2,
                }],
                clicks: vec![ClickEvent {
                    t_ms: 5,
                    x: 1,
                    y: 2,
                    button: MouseButton::Left,
                    state: ButtonState::Down,
                }],
                keys: vec![],
                focus: vec![],
            },
            edits: EditList::default(),
            pauses: vec![],
            status: ProjectStatus::Complete,
            warnings: vec![],
            cursor_baked: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::samples::sample;
    use super::*;

    fn tmp_dir() -> PathBuf {
        let base = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
        let dir = base.join("test-tmp");
        fs::create_dir_all(&dir).expect("create test tmp dir");
        dir
    }

    #[test]
    fn hostile_projects_are_rejected() {
        let ok = sample();
        ok.validate().unwrap();
        let mut p = sample();
        p.media.screen = "..\\..\\Windows\\win.ini".into();
        assert!(p.validate().is_err());
        let mut p = sample();
        p.canvas.width = 0;
        assert!(p.validate().is_err());
        let mut p = sample();
        p.edits.style.padding = f64::NAN;
        assert!(p.validate().is_err());
        let mut p = sample();
        p.name = "x".repeat(limits::MAX_STRING + 1);
        assert!(p.validate().is_err());
        let mut p = sample();
        p.version = 99;
        assert!(p.validate().is_err());
        // Malformed JSON and oversize files on disk.
        let dir = tmp_dir().join(format!("hostile-project-{}.frostcap", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(PROJECT_FILE), b"{\"version\":1,").unwrap();
        assert!(matches!(Project::load(&dir), Err(Error::InvalidProject(_))));
        let f = fs::File::create(dir.join(PROJECT_FILE)).unwrap();
        f.set_len(limits::MAX_PROJECT_BYTES + 1).unwrap();
        drop(f);
        assert!(matches!(Project::load(&dir), Err(Error::InvalidProject(_))));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn media_names_reject_devices_trailing_dots_and_wrong_extensions() {
        for (field, bad) in [
            ("screen", "CON.mp4"),
            ("screen", "nul.MP4"),
            ("screen", "COM1.mp4"),
            ("screen", "lpt9.mp4"),
            ("screen", "CONIN$.mp4"),
            ("screen", "screen.mp4."),
            ("screen", "screen.mp4 "),
            ("screen", "screen.wav"),
            ("mic", "audio.mp4"),
        ] {
            let mut p = sample();
            match field {
                "screen" => p.media.screen = bad.into(),
                _ => {
                    p.media.mic_audio = Some(AudioTrack {
                        file: bad.into(),
                        offset_ms: 0,
                        sample_rate: 48_000,
                        channels: 2,
                        duration_ms: 1,
                    })
                }
            }
            assert!(p.validate().is_err(), "{field} = {bad:?} must be rejected");
        }
        let mut ok = sample();
        ok.media.screen = "screen.mp4".into();
        ok.validate().unwrap();
    }

    #[test]
    fn ui_edit_state_roundtrips_and_is_capped() {
        let mut p = sample();
        p.edits.ui = serde_json::json!({"frame": {"radius": 12}, "captions": [{"text": "hi"}], "splits": [1, 2]});
        p.cursor_baked = true;
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"ui\":{") && s.contains("\"cursorBaked\":true"));
        let back: Project = serde_json::from_str(&s).unwrap();
        assert_eq!(back.edits.ui, p.edits.ui);
        back.validate().unwrap();
        // Missing in old files -> null / false.
        let mut v = serde_json::to_value(sample()).unwrap();
        v["edits"].as_object_mut().unwrap().remove("ui");
        v.as_object_mut().unwrap().remove("cursorBaked");
        let old: Project = serde_json::from_value(v).unwrap();
        assert!(old.edits.ui.is_null() && !old.cursor_baked);
        // Oversized opaque UI state is rejected.
        let mut big = sample();
        big.edits.ui = serde_json::json!({"blob": "x".repeat(limits::MAX_UI_STATE_BYTES + 1)});
        assert!(matches!(big.validate(), Err(Error::InvalidProject(_))));
        // Non-finite numbers cannot even be represented in serde_json::Value (become null).
        assert!(serde_json::to_value(f64::NAN).unwrap().is_null());
    }

    #[test]
    fn golden_fixture_loads_validates_and_roundtrips() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/project.sample.json");
        let raw: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let p: Project = serde_json::from_value(raw.clone()).unwrap();
        p.validate().unwrap();
        assert_eq!(serde_json::to_value(&p).unwrap(), raw);
        let back: Project = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }

    fn legacy_ui_project() -> serde_json::Value {
        let mut v = serde_json::to_value(sample()).unwrap();
        let edits = v["edits"].as_object_mut().unwrap();
        // A project written before the typed fields existed.
        let style = edits["style"].as_object_mut().unwrap();
        for k in ["frame", "cursor", "webcam", "audio", "captions"] {
            style.remove(k);
        }
        edits.remove("captions");
        edits.remove("splits");
        edits.insert(
            "ui".into(),
            serde_json::json!({
                "v": 1,
                "frame": "macos",
                "zoom": {"preset": "snappy", "scale": 2.5, "easing": "linear", "transitionMs": 300},
                "cursor": {"visible": false, "smoothing": 0.25, "motionBlur": 0.7},
                "webcam": {"enabled": true, "shape": "rounded", "size": 0.3, "position": "top-left"},
                "audio": {"noiseGate": false, "noiseGateThreshold": -40, "highPass": true, "highPassHz": 120,
                          "normalize": false, "targetLufs": -14, "micGain": 0.0, "systemGain": 1.5},
                "captions": {"enabled": true, "fontSize": 52, "position": "top", "background": false},
                "captionCues": [{"start": 100, "end": 900.4, "text": "hello"}],
                "splits": [1000, 2000]
            }),
        );
        v
    }

    #[test]
    fn legacy_fixture_with_ui_v1_blob_migrates() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/legacy/project.ui-v1.json");
        let p: Project = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        p.validate().unwrap();
        let s = &p.edits.style;
        assert_eq!(s.frame, WindowFrame::Windows);
        assert!(!s.cursor.visible && s.webcam.enabled && s.show_keys);
        assert_eq!((s.audio.mic_gain, s.audio.system_gain), (0.0, 1.5));
        assert_eq!(s.captions.position, CaptionPosition::Top);
        assert_eq!(p.edits.captions.len(), 2);
        assert_eq!(p.edits.captions[1].text, "world");
        assert_eq!(p.edits.splits, vec![1000, 2000]);
        // Re-saving writes the typed fields, so a second load needs no migration.
        let again: Project = serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert_eq!(again, p);
    }

    #[test]
    fn style_defaults_match_the_editor() {
        let s = Style::default();
        assert_eq!(s.frame, WindowFrame::Minimal);
        assert_eq!(
            (s.cursor.visible, s.cursor.smoothing, s.cursor.motion_blur),
            (true, 0.6, 0.3)
        );
        assert!(!s.webcam.enabled);
        assert_eq!(
            (s.webcam.shape, s.webcam.size, s.webcam.position),
            (WebcamShape::Circle, 0.22, Corner::BottomRight)
        );
        assert!(s.audio.noise_gate && s.audio.high_pass && s.audio.normalize);
        assert_eq!(
            (
                s.audio.noise_gate_threshold,
                s.audio.high_pass_hz,
                s.audio.target_lufs
            ),
            (-50.0, 80.0, -16.0)
        );
        assert_eq!((s.audio.mic_gain, s.audio.system_gain), (1.0, 1.0));
        assert_eq!(
            (
                s.captions.enabled,
                s.captions.font_size,
                s.captions.position,
                s.captions.background
            ),
            (true, 40.0, CaptionPosition::Bottom, true)
        );
    }

    #[test]
    fn typed_edit_fields_roundtrip_with_editor_wire_names() {
        let mut p = sample();
        p.edits.style.frame = WindowFrame::MacOs;
        p.edits.style.webcam.position = Corner::TopLeft;
        p.edits.style.webcam.shape = WebcamShape::Rounded;
        p.edits.style.captions.position = CaptionPosition::Top;
        p.edits.captions = vec![CaptionCue {
            start_ms: 10,
            end_ms: 20,
            text: "hi".into(),
        }];
        p.edits.splits = vec![500];
        let v = serde_json::to_value(&p).unwrap();
        let st = &v["edits"]["style"];
        assert_eq!(st["frame"], "macos");
        assert_eq!(st["webcam"]["position"], "top-left");
        assert_eq!(st["webcam"]["shape"], "rounded");
        assert_eq!(st["captions"]["position"], "top");
        assert_eq!(st["cursor"]["motionBlur"], 0.3);
        assert_eq!(st["audio"]["micGain"], 1.0);
        assert_eq!(v["edits"]["captions"][0]["startMs"], 10);
        assert_eq!(v["edits"]["splits"][0], 500);
        let back: Project = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
        back.validate().unwrap();
    }

    #[test]
    fn legacy_ui_blob_migrates_into_typed_fields() {
        let p: Project = serde_json::from_value(legacy_ui_project()).unwrap();
        let s = &p.edits.style;
        assert_eq!(s.frame, WindowFrame::MacOs);
        assert_eq!(
            (s.cursor.visible, s.cursor.smoothing, s.cursor.motion_blur),
            (false, 0.25, 0.7)
        );
        assert!(s.webcam.enabled);
        assert_eq!(
            (s.webcam.shape, s.webcam.size, s.webcam.position),
            (WebcamShape::Rounded, 0.3, Corner::TopLeft)
        );
        assert!(!s.audio.noise_gate && !s.audio.normalize && s.audio.high_pass);
        assert_eq!(
            (
                s.audio.high_pass_hz,
                s.audio.target_lufs,
                s.audio.mic_gain,
                s.audio.system_gain
            ),
            (120.0, -14.0, 0.0, 1.5)
        );
        assert_eq!(
            (
                s.captions.font_size,
                s.captions.position,
                s.captions.background
            ),
            (52.0, CaptionPosition::Top, false)
        );
        assert_eq!(
            p.edits.captions,
            vec![CaptionCue {
                start_ms: 100,
                end_ms: 900,
                text: "hello".into()
            }]
        );
        assert_eq!(p.edits.splits, vec![1000, 2000]);
        // The opaque blob is kept verbatim.
        assert_eq!(p.edits.ui["v"], 1);
        p.validate().unwrap();
    }

    #[test]
    fn typed_fields_win_over_the_legacy_blob() {
        let mut v = legacy_ui_project();
        v["edits"]["style"]["frame"] = serde_json::json!("windows");
        v["edits"]["splits"] = serde_json::json!([7]);
        let p: Project = serde_json::from_value(v).unwrap();
        assert_eq!(p.edits.style.frame, WindowFrame::Windows);
        assert_eq!(p.edits.splits, vec![7]);
        // Groups still absent come from the blob.
        assert_eq!(p.edits.style.captions.font_size, 52.0);
    }

    #[test]
    fn no_blob_means_defaults_and_unknown_blobs_are_ignored() {
        let mut v = legacy_ui_project();
        v["edits"]["ui"] = serde_json::json!({"v": 2, "frame": "macos"});
        let p: Project = serde_json::from_value(v).unwrap();
        assert_eq!(p.edits.style.frame, WindowFrame::Minimal);
        let mut v = legacy_ui_project();
        v["edits"].as_object_mut().unwrap().remove("ui");
        let p: Project = serde_json::from_value(v).unwrap();
        assert_eq!(p.edits.style, Style::default());
        assert!(p.edits.captions.is_empty() && p.edits.splits.is_empty());
    }

    #[test]
    fn hostile_typed_edit_fields_are_rejected() {
        type Mutate = Box<dyn Fn(&mut Project)>;
        let bad: Vec<(&str, Mutate)> = vec![
            (
                "smoothing",
                Box::new(|p| p.edits.style.cursor.smoothing = 2.0),
            ),
            (
                "blur nan",
                Box::new(|p| p.edits.style.cursor.motion_blur = f64::NAN),
            ),
            ("webcam size", Box::new(|p| p.edits.style.webcam.size = 0.9)),
            ("gain", Box::new(|p| p.edits.style.audio.mic_gain = 9.0)),
            (
                "neg gain",
                Box::new(|p| p.edits.style.audio.system_gain = -1.0),
            ),
            ("lufs", Box::new(|p| p.edits.style.audio.target_lufs = 3.0)),
            ("hp", Box::new(|p| p.edits.style.audio.high_pass_hz = 0.0)),
            (
                "font",
                Box::new(|p| p.edits.style.captions.font_size = 1000.0),
            ),
            (
                "cue order",
                Box::new(|p| {
                    p.edits.captions = vec![CaptionCue {
                        start_ms: 9,
                        end_ms: 1,
                        text: "x".into(),
                    }]
                }),
            ),
            (
                "cue text",
                Box::new(|p| {
                    p.edits.captions = vec![CaptionCue {
                        start_ms: 0,
                        end_ms: 1,
                        text: "x".repeat(limits::MAX_STRING + 1),
                    }]
                }),
            ),
            (
                "cue count",
                Box::new(|p| {
                    p.edits.captions = vec![
                        CaptionCue {
                            start_ms: 0,
                            end_ms: 1,
                            text: "x".into()
                        };
                        limits::MAX_EDITS + 1
                    ]
                }),
            ),
            (
                "splits",
                Box::new(|p| p.edits.splits = vec![1; limits::MAX_EDITS + 1]),
            ),
        ];
        for (name, f) in bad {
            let mut p = sample();
            f(&mut p);
            assert!(p.validate().is_err(), "{name} was accepted");
        }
    }
    #[test]
    fn absurd_durations_are_rejected() {
        let mut p = sample();
        p.duration_ms = limits::MAX_DURATION_MS;
        p.validate().unwrap();
        p.duration_ms = limits::MAX_DURATION_MS + 1;
        assert!(p.validate().is_err());
        p.duration_ms = u64::MAX;
        assert!(p.validate().is_err());
        const { assert!(limits::MAX_DURATION_MS == 24 * 3600 * 1000) };
    }

    #[test]
    fn validate_ranges_match_what_the_renderer_clamps_to() {
        let ok = |f: &dyn Fn(&mut Project)| {
            let mut p = sample();
            f(&mut p);
            p.validate().is_ok()
        };
        assert!(ok(&|p| p.edits.style.padding = 0.45));
        assert!(!ok(&|p| p.edits.style.padding = 0.4501));
        assert!(ok(&|p| p.edits.style.cursor_scale = 8.0));
        assert!(!ok(&|p| p.edits.style.cursor_scale = 8.01));
        let zoom = |scale| {
            move |p: &mut Project| {
                p.edits.zoom_segments = vec![ZoomSegment {
                    start_ms: 0,
                    end_ms: 10,
                    center: NormPoint { x: 0.5, y: 0.5 },
                    scale,
                    easing: Easing::Linear,
                    transition_ms: 1,
                    origin: SegmentOrigin::Manual,
                }];
            }
        };
        assert!(ok(&zoom(8.0)));
        assert!(!ok(&zoom(8.01)));
    }

    #[test]
    fn out_of_range_legacy_blob_is_clamped_so_old_projects_still_load() {
        let mut v = legacy_ui_project();
        v["edits"]["ui"] = serde_json::json!({
            "v": 1,
            "cursor": {"visible": true, "smoothing": 7.5, "motionBlur": -3},
            "webcam": {"enabled": true, "shape": "circle", "size": 9, "position": "top-left"},
            "audio": {"noiseGate": true, "noiseGateThreshold": 55, "highPass": true, "highPassHz": 99999,
                      "normalize": true, "targetLufs": 12, "micGain": -4, "systemGain": 100},
            "captions": {"enabled": true, "fontSize": 9999, "position": "top", "background": true},
            "captionCues": [
                {"start": 900, "end": 100, "text": "inverted"},
                {"start": 5, "end": 6, "text": "x".repeat(limits::MAX_STRING + 50)},
                {"start": "bad", "end": 1, "text": "dropped"}
            ],
            "splits": [10, -5, 1e12]
        });
        let p: Project = serde_json::from_value(v).unwrap();
        p.validate().unwrap();
        let s = &p.edits.style;
        assert_eq!((s.cursor.smoothing, s.cursor.motion_blur), (1.0, 0.0));
        assert_eq!(s.webcam.size, 0.6);
        assert_eq!(
            (
                s.audio.noise_gate_threshold,
                s.audio.high_pass_hz,
                s.audio.target_lufs
            ),
            (0.0, 1000.0, -5.0)
        );
        assert_eq!((s.audio.mic_gain, s.audio.system_gain), (0.0, 4.0));
        assert_eq!(s.captions.font_size, 200.0);
        // Inverted cue swapped, oversized text truncated, malformed cue dropped.
        assert_eq!(p.edits.captions.len(), 2);
        assert_eq!(
            (p.edits.captions[0].start_ms, p.edits.captions[0].end_ms),
            (100, 900)
        );
        assert_eq!(p.edits.captions[1].text.len(), limits::MAX_STRING);
        assert_eq!(p.edits.splits.len(), 3);
    }

    #[test]
    fn roundtrip_json_uses_camel_case() {
        let p = sample();
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"createdAtMs\""));
        assert!(s.contains("\"zoomSegments\""));
        assert!(s.contains("\"tMs\""));
        let back: Project = serde_json::from_str(&s).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn edits_default_when_missing() {
        let mut v = serde_json::to_value(sample()).unwrap();
        v.as_object_mut().unwrap().remove("edits");
        let p: Project = serde_json::from_value(v).unwrap();
        assert_eq!(p.edits, EditList::default());
    }

    #[test]
    fn save_and_load() {
        let dir = tmp_dir().join(format!("save-load-{}.frostcap", std::process::id()));
        let p = sample();
        p.save(&dir).unwrap();
        let back = Project::load(&dir).unwrap();
        assert_eq!(back, p);
        let back2 = Project::load(&dir.join(PROJECT_FILE)).unwrap();
        assert_eq!(back2, p);
        let _ = fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------ editor v2 fields

    fn with_speed(segs: &[(u64, u64, f64)]) -> Project {
        let mut p = sample();
        p.edits.speed = segs
            .iter()
            .map(|&(start_ms, end_ms, speed)| SpeedSegment {
                start_ms,
                end_ms,
                speed,
            })
            .collect();
        p
    }

    #[test]
    fn speed_segments_roundtrip_with_wire_names_and_default_to_empty() {
        let p = with_speed(&[(1000, 2000, 2.0)]);
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["edits"]["speed"][0]["startMs"], 1000);
        assert_eq!(v["edits"]["speed"][0]["endMs"], 2000);
        assert_eq!(v["edits"]["speed"][0]["speed"], 2.0);
        let back: Project = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(back, p);
        let mut old = v;
        old["edits"].as_object_mut().unwrap().remove("speed");
        let old: Project = serde_json::from_value(old).unwrap();
        assert!(old.edits.speed.is_empty());
        old.validate().unwrap();
    }

    #[test]
    fn speed_range_order_and_overlap_are_validated() {
        let ok = |segs: &[(u64, u64, f64)]| with_speed(segs).validate().is_ok();
        assert!(ok(&[(0, 100, 0.25)]));
        assert!(ok(&[(0, 100, 4.0)]));
        assert!(!ok(&[(0, 100, 0.2499)]));
        assert!(!ok(&[(0, 100, 4.0001)]));
        assert!(!ok(&[(0, 100, f64::NAN)]));
        assert!(!ok(&[(0, 100, f64::INFINITY)]));
        assert!(!ok(&[(0, 100, 0.0)]));
        assert!(!ok(&[(0, 100, -1.0)]));
        assert!(!ok(&[(50, 10, 2.0)]), "inverted range");
        assert!(ok(&[(0, 100, 2.0), (100, 200, 0.5)]), "touching is fine");
        assert!(!ok(&[(0, 100, 2.0), (99, 200, 0.5)]), "overlap");
        assert!(!ok(&[(100, 200, 2.0), (0, 150, 0.5)]), "unsorted overlap");
        let mut many = sample();
        many.edits.speed = vec![
            SpeedSegment {
                start_ms: 0,
                end_ms: 0,
                speed: 1.0,
            };
            limits::MAX_EDITS + 1
        ];
        assert!(many.validate().is_err());
    }

    #[test]
    fn device_frames_have_wire_names() {
        for (f, name) in [
            (WindowFrame::Laptop, "laptop"),
            (WindowFrame::Phone, "phone"),
        ] {
            let mut p = sample();
            p.edits.style.frame = f;
            let v = serde_json::to_value(&p).unwrap();
            assert_eq!(v["edits"]["style"]["frame"], name);
            let back: Project = serde_json::from_value(v).unwrap();
            assert_eq!(back.edits.style.frame, f);
        }
    }

    #[test]
    fn webcam_margin_and_border_defaults_ranges_and_wire() {
        let d = WebcamStyle::default();
        assert_eq!(
            d.margin,
            28.0 / 1080.0,
            "default keeps the old 28 px at 1080p"
        );
        assert_eq!(d.border_width, 0.0);
        assert_eq!(d.border_color, Rgba([255, 255, 255, 255]));
        let v = serde_json::to_value(sample()).unwrap();
        let w = &v["edits"]["style"]["webcam"];
        assert_eq!(w["borderWidth"], 0.0);
        assert_eq!(w["borderColor"], "#ffffff");
        assert!(w["margin"].as_f64().unwrap() > 0.025);
        let ok = |f: &dyn Fn(&mut WebcamStyle)| {
            let mut p = sample();
            f(&mut p.edits.style.webcam);
            p.validate().is_ok()
        };
        assert!(ok(&|w| w.margin = 0.0));
        assert!(ok(&|w| w.margin = 0.2));
        assert!(!ok(&|w| w.margin = 0.2001));
        assert!(!ok(&|w| w.margin = -0.001));
        assert!(!ok(&|w| w.margin = f64::NAN));
        assert!(ok(&|w| w.border_width = 20.0));
        assert!(!ok(&|w| w.border_width = 20.01));
        assert!(!ok(&|w| w.border_width = -0.1));
    }

    #[test]
    fn hex_colors_parse_and_reject_garbage() {
        let parse = |s: &str| serde_json::from_value::<Rgba>(serde_json::json!(s));
        assert_eq!(parse("#4ED2FB").unwrap(), Rgba([0x4e, 0xd2, 0xfb, 255]));
        assert_eq!(parse("#fff").unwrap(), Rgba([255, 255, 255, 255]));
        assert_eq!(parse("#11223380").unwrap(), Rgba([0x11, 0x22, 0x33, 0x80]));
        for bad in [
            "",
            "red",
            "#12",
            "#12345",
            "#gggggg",
            "123456",
            "#1234567890",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} accepted");
        }
        assert_eq!(
            serde_json::to_value(Rgba([1, 2, 3, 255])).unwrap(),
            "#010203"
        );
        assert_eq!(
            serde_json::to_value(Rgba([1, 2, 3, 4])).unwrap(),
            "#01020304"
        );
    }

    #[test]
    fn background_image_roundtrips_and_is_validated() {
        let mut p = sample();
        assert!(
            serde_json::to_value(&p).unwrap()["edits"]["style"]
                .get("backgroundImage")
                .is_none(),
            "absent when unset"
        );
        p.edits.style.background_image = Some(BackgroundImage {
            file: "background-0123456789abcdef.png".into(),
            fit: BackgroundFit::Contain,
            blur: 12.5,
        });
        let v = serde_json::to_value(&p).unwrap();
        let bi = &v["edits"]["style"]["backgroundImage"];
        assert_eq!(bi["file"], "background-0123456789abcdef.png");
        assert_eq!(bi["fit"], "contain");
        assert_eq!(bi["blur"], 12.5);
        let back: Project = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
        back.validate().unwrap();
        let bad = |f: &dyn Fn(&mut BackgroundImage)| {
            let mut p = p.clone();
            f(p.edits.style.background_image.as_mut().unwrap());
            p.validate().is_err()
        };
        assert!(bad(&|b| b.file = "..\\evil.png".into()));
        assert!(bad(&|b| b.file = "sub/evil.png".into()));
        assert!(bad(&|b| b.file = "C:\\x.png".into()));
        assert!(bad(&|b| b.file = "bg.gif".into()));
        assert!(bad(&|b| b.file = "bg.mp4".into()));
        assert!(bad(&|b| b.file = "NUL.png".into()));
        assert!(bad(&|b| b.blur = 100.5));
        assert!(bad(&|b| b.blur = -1.0));
        assert!(bad(&|b| b.blur = f64::NAN));
        let mut ok = p.clone();
        ok.edits.style.background_image.as_mut().unwrap().file = "photo.JPG".into();
        ok.edits.style.background_image.as_mut().unwrap().blur = 100.0;
        ok.validate().unwrap();
    }

    fn png_header(w: u32, h: u32) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        v.extend(w.to_be_bytes());
        v.extend(h.to_be_bytes());
        v.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        v
    }

    fn jpeg_header(w: u16, h: u16) -> Vec<u8> {
        // SOI, APP0 (JFIF, 16 bytes), SOF0 (17 bytes).
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16];
        v.extend(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        v.extend([0xFF, 0xC0, 0, 17, 8]);
        v.extend(h.to_be_bytes());
        v.extend(w.to_be_bytes());
        v.extend([3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        v
    }

    fn import_fixture(name: &str) -> (PathBuf, PathBuf) {
        let base = tmp_dir().join(format!("import-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let proj = base.join("p.frostcap");
        fs::create_dir_all(&proj).unwrap();
        (base, proj)
    }

    #[test]
    fn import_background_copies_validated_images_by_content_hash() {
        let (base, proj) = import_fixture("ok");
        let src = base.join("My Photo (1).PNG");
        fs::write(&src, png_header(1920, 1080)).unwrap();
        let name = import_background(&proj, &src).unwrap();
        assert!(
            name.starts_with("background-") && name.ends_with(".png"),
            "{name}"
        );
        assert_eq!(fs::read(proj.join(&name)).unwrap(), png_header(1920, 1080));
        // The imported name is a valid project reference and importing twice is stable.
        let mut p = sample();
        p.edits.style.background_image = Some(BackgroundImage {
            file: name.clone(),
            fit: BackgroundFit::Cover,
            blur: 0.0,
        });
        p.validate().unwrap();
        assert_eq!(import_background(&proj, &src).unwrap(), name);
        // A JPEG with a .jpeg extension is stored as .jpg.
        let jsrc = base.join("photo.jpeg");
        fs::write(&jsrc, jpeg_header(800, 600)).unwrap();
        let jn = import_background(&proj, &jsrc).unwrap();
        assert!(jn.ends_with(".jpg"), "{jn}");
        assert_ne!(jn, name);
        // The content decides the type, not the extension.
        let liar = base.join("liar.png");
        fs::write(&liar, jpeg_header(10, 10)).unwrap();
        assert!(import_background(&proj, &liar).unwrap().ends_with(".jpg"));
        assert_eq!(
            image_info(&png_header(7, 9)).unwrap(),
            (ImageKind::Png, 7, 9)
        );
        assert_eq!(
            image_info(&jpeg_header(7, 9)).unwrap(),
            (ImageKind::Jpeg, 7, 9)
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn import_background_rejects_hostile_files() {
        let (base, proj) = import_fixture("bad");
        let put = |n: &str, b: &[u8]| {
            let f = base.join(n);
            fs::write(&f, b).unwrap();
            f
        };
        let entries = |d: &Path| fs::read_dir(d).unwrap().count();
        let before = entries(&proj);
        // Wrong extension, not an image, empty, truncated header.
        assert!(import_background(&proj, &put("a.gif", &png_header(10, 10))).is_err());
        assert!(import_background(&proj, &put("a.txt", &png_header(10, 10))).is_err());
        assert!(import_background(&proj, &put("noext", &png_header(10, 10))).is_err());
        assert!(import_background(&proj, &put("b.png", b"GIF89a....")).is_err());
        assert!(import_background(&proj, &put("c.png", b"")).is_err());
        assert!(import_background(&proj, &put("d.png", &png_header(10, 10)[..20])).is_err());
        assert!(import_background(&proj, &put("e.jpg", &[0xFF, 0xD8, 0xFF, 0xD9])).is_err());
        // Absurd or zero dimensions.
        assert!(import_background(&proj, &put("f.png", &png_header(0, 10))).is_err());
        assert!(import_background(&proj, &put("g.png", &png_header(20_000, 10))).is_err());
        assert!(import_background(&proj, &put("h.png", &png_header(16_384, 16_384))).is_err());
        assert!(import_background(&proj, &put("i.jpg", &jpeg_header(0, 5))).is_err());
        // Over the size cap (header is valid, the file is just too large).
        let big = base.join("big.png");
        let mut f = fs::File::create(&big).unwrap();
        f.write_all(&png_header(100, 100)).unwrap();
        f.set_len(limits::MAX_BACKGROUND_BYTES + 1).unwrap();
        drop(f);
        assert!(import_background(&proj, &big).is_err());
        // Exactly at the cap is accepted.
        let edge = base.join("edge.png");
        let mut f = fs::File::create(&edge).unwrap();
        f.write_all(&png_header(100, 100)).unwrap();
        f.set_len(limits::MAX_BACKGROUND_BYTES).unwrap();
        drop(f);
        assert!(import_background(&proj, &edge).is_ok());
        // A folder, a missing source, a missing project folder.
        assert!(import_background(&proj, &base).is_err());
        assert!(import_background(&proj, &base.join("nope.png")).is_err());
        let ok = put("ok.png", &png_header(10, 10));
        assert!(import_background(&base.join("missing.frostcap"), &ok).is_err());
        // Only the accepted edge file was written, and no temp files remain.
        assert_eq!(entries(&proj), before + 1);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn import_background_never_overwrites_a_different_file_with_the_same_name() {
        let (base, proj) = import_fixture("collide");
        let src = base.join("a.png");
        fs::write(&src, png_header(32, 32)).unwrap();
        let first = import_background(&proj, &src).unwrap();
        // Someone (or a hash collision) put other bytes under that exact name.
        fs::write(proj.join(&first), b"different content").unwrap();
        let second = import_background(&proj, &src).unwrap();
        assert_ne!(second, first, "a differing file keeps its name");
        assert!(
            second.starts_with("background-") && second.ends_with(".png"),
            "{second}"
        );
        assert_eq!(fs::read(proj.join(&first)).unwrap(), b"different content");
        assert_eq!(fs::read(proj.join(&second)).unwrap(), png_header(32, 32));
        // Importing again finds the identical suffixed copy instead of adding a third.
        assert_eq!(import_background(&proj, &src).unwrap(), second);
        // The suffixed name is a valid project reference.
        let mut p = sample();
        p.edits.style.background_image = Some(BackgroundImage {
            file: second,
            fit: BackgroundFit::Cover,
            blur: 0.0,
        });
        p.validate().unwrap();
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn import_background_file_takes_an_open_handle_and_the_wrapper_agrees() {
        let (base, proj) = import_fixture("handle");
        let src = base.join("h.PNG");
        fs::write(&src, png_header(16, 16)).unwrap();
        let via_path = import_background(&proj, &src).unwrap();
        let via_handle =
            import_background_file(&proj, fs::File::open(&src).unwrap(), "PNG").unwrap();
        assert_eq!(via_path, via_handle);
        // Extension and content are validated on the handle too.
        let gif = base.join("g.gif");
        fs::write(&gif, png_header(4, 4)).unwrap();
        assert!(import_background_file(&proj, fs::File::open(&gif).unwrap(), "gif").is_err());
        let junk = base.join("j.png");
        fs::write(&junk, b"not an image").unwrap();
        assert!(import_background_file(&proj, fs::File::open(&junk).unwrap(), "png").is_err());
        let big = base.join("big.png");
        let mut f = fs::File::create(&big).unwrap();
        f.write_all(&png_header(10, 10)).unwrap();
        f.set_len(limits::MAX_BACKGROUND_BYTES + 1).unwrap();
        drop(f);
        assert!(import_background_file(&proj, fs::File::open(&big).unwrap(), "png").is_err());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn background_path_resolves_only_plain_files_inside_the_project() {
        let (base, proj) = import_fixture("path");
        let src = base.join("x.png");
        fs::write(&src, png_header(4, 4)).unwrap();
        let name = import_background(&proj, &src).unwrap();
        let bi = |file: &str| BackgroundImage {
            file: file.into(),
            fit: BackgroundFit::Cover,
            blur: 0.0,
        };
        assert_eq!(
            background_path(&proj, &bi(&name)).unwrap(),
            proj.join(&name)
        );
        assert!(background_path(&proj, &bi("..\\x.png")).is_err());
        assert!(background_path(&proj, &bi("missing.png")).is_err());
        fs::create_dir(proj.join("dir.png")).unwrap();
        assert!(background_path(&proj, &bi("dir.png")).is_err());
        let _ = fs::remove_dir_all(&base);
    }

    // ------------------------------------------------- annotations + webcam effects

    fn ann(kind: AnnotationKind) -> Annotation {
        Annotation {
            id: "a1".into(),
            kind,
            ..Annotation::default()
        }
    }

    #[test]
    fn annotations_roundtrip_with_wire_names_and_defaults() {
        let mut p = sample();
        p.edits.annotations = vec![Annotation {
            id: "x".into(),
            kind: AnnotationKind::Arrow,
            start_ms: 100,
            end_ms: 900,
            x: 0.1,
            y: 0.2,
            x2: 0.5,
            y2: 0.6,
            color: Rgba([255, 0, 0, 255]),
            stroke_width: 6.0,
            ..Annotation::default()
        }];
        let v = serde_json::to_value(&p).unwrap();
        let a = &v["edits"]["annotations"][0];
        assert_eq!(a["kind"], "arrow");
        assert_eq!(a["startMs"], 100);
        assert_eq!(a["color"], "#ff0000");
        assert_eq!(a["strokeWidth"], 6.0);
        for k in [
            "fill",
            "text",
            "fontSize",
            "opacity",
            "strength",
            "dim",
            "cornerRadius",
            "fadeMs",
            "x2",
            "y2",
            "w",
            "h",
        ] {
            assert!(a.get(k).is_some(), "{k} is always written");
        }
        let back: Project = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(back, p);
        back.validate().unwrap();
        // A minimal UI object (only id/kind/times/geometry) fills the rest with defaults.
        let mut v2 = v;
        v2["edits"]["annotations"] = serde_json::json!([{"id":"m","kind":"spotlight","startMs":0,"endMs":10,"x":0.2,"y":0.2,"w":0.3,"h":0.3}]);
        let m: Project = serde_json::from_value(v2).unwrap();
        assert_eq!(m.edits.annotations[0].kind, AnnotationKind::Spotlight);
        assert_eq!(m.edits.annotations[0].opacity, 1.0);
        m.validate().unwrap();
        for (k, n) in [
            (AnnotationKind::Text, "text"),
            (AnnotationKind::Rect, "rect"),
            (AnnotationKind::Ellipse, "ellipse"),
            (AnnotationKind::Highlight, "highlight"),
            (AnnotationKind::Blur, "blur"),
            (AnnotationKind::Pixelate, "pixelate"),
        ] {
            assert_eq!(serde_json::to_value(k).unwrap(), n);
        }
        // Missing field in older projects -> empty.
        let mut old = serde_json::to_value(sample()).unwrap();
        old["edits"].as_object_mut().unwrap().remove("annotations");
        let old: Project = serde_json::from_value(old).unwrap();
        assert!(old.edits.annotations.is_empty());
    }

    #[test]
    fn annotation_ranges_are_validated() {
        let bad = |f: &dyn Fn(&mut Annotation)| {
            let mut p = sample();
            let mut a = ann(AnnotationKind::Rect);
            f(&mut a);
            p.edits.annotations = vec![a];
            p.validate().is_err()
        };
        let ok = |f: &dyn Fn(&mut Annotation)| !bad(f);
        assert!(ok(&|_| {}));
        assert!(bad(&|a| a.id = "x".repeat(65)));
        assert!(ok(&|a| a.id = "x".repeat(64)));
        assert!(bad(&|a| {
            a.start_ms = 5;
            a.end_ms = 4;
        }));
        assert!(ok(&|a| a.end_ms = limits::MAX_DURATION_MS));
        assert!(bad(&|a| a.end_ms = limits::MAX_DURATION_MS + 1));
        for set in [
            (|a: &mut Annotation, v| a.x = v) as fn(&mut Annotation, f64),
            |a, v| a.y = v,
            |a, v| a.w = v,
            |a, v| a.h = v,
            |a, v| a.x2 = v,
            |a, v| a.y2 = v,
            |a, v| a.opacity = v,
            |a, v| a.strength = v,
            |a, v| a.dim = v,
        ] {
            assert!(bad(&|a| set(a, -0.01)));
            assert!(bad(&|a| set(a, 1.01)));
            assert!(bad(&|a| set(a, f64::NAN)));
            assert!(ok(&|a| set(a, 0.0)));
            assert!(ok(&|a| set(a, 1.0)));
        }
        assert!(ok(&|a| a.stroke_width = 1.0) && ok(&|a| a.stroke_width = 40.0));
        assert!(bad(&|a| a.stroke_width = 0.99) && bad(&|a| a.stroke_width = 40.1));
        assert!(ok(&|a| a.font_size = 8.0) && ok(&|a| a.font_size = 200.0));
        assert!(bad(&|a| a.font_size = 7.0) && bad(&|a| a.font_size = 201.0));
        assert!(ok(&|a| a.corner_radius = 200.0) && bad(&|a| a.corner_radius = 200.5));
        assert!(ok(&|a| a.fade_ms = 2000) && bad(&|a| a.fade_ms = 2001));
        assert!(ok(&|a| a.text = "é".repeat(500)));
        assert!(bad(&|a| a.text = "x".repeat(501)));
        let mut many = sample();
        many.edits.annotations = vec![ann(AnnotationKind::Rect); 501];
        assert!(many.validate().is_err());
        many.edits.annotations.truncate(500);
        many.validate().unwrap();
        assert!(serde_json::from_str::<Annotation>(r#"{"kind":"laser"}"#).is_err());
        assert!(serde_json::from_str::<Annotation>(r##"{"kind":"rect","color":"red"}"##).is_err());
    }

    #[test]
    fn webcam_effects_defaults_wire_and_ranges() {
        let d = WebcamStyle::default();
        assert!(!d.mirror && !d.chroma_key.enabled);
        assert_eq!((d.brightness, d.contrast, d.saturation), (0.0, 1.0, 1.0));
        assert_eq!(d.shadow, 0.4, "default keeps the old fixed shadow");
        assert_eq!(d.background.kind, WebcamBackgroundKind::None);
        let v = serde_json::to_value(sample()).unwrap();
        let w = &v["edits"]["style"]["webcam"];
        assert_eq!(w["mirror"], false);
        assert_eq!(w["chromaKey"]["color"], "#00ff00");
        assert_eq!(w["background"]["kind"], "none");
        for k in [
            "brightness",
            "contrast",
            "saturation",
            "shadow",
            "borderWidth",
            "borderColor",
        ] {
            assert!(w.get(k).is_some(), "{k}");
        }
        let kinds = ["none", "color", "gradient", "image", "blur"];
        for k in kinds {
            let b: WebcamBackground =
                serde_json::from_value(serde_json::json!({ "kind": k })).unwrap();
            assert_eq!(serde_json::to_value(b.kind).unwrap(), k);
        }
        let ok = |f: &dyn Fn(&mut WebcamStyle)| {
            let mut p = sample();
            f(&mut p.edits.style.webcam);
            p.validate().is_ok()
        };
        assert!(ok(&|w| w.brightness = -1.0) && ok(&|w| w.brightness = 1.0));
        assert!(!ok(&|w| w.brightness = 1.01) && !ok(&|w| w.brightness = -1.01));
        assert!(
            ok(&|w| w.contrast = 0.0) && ok(&|w| w.contrast = 2.0) && !ok(&|w| w.contrast = 2.01)
        );
        assert!(ok(&|w| w.saturation = 2.0) && !ok(&|w| w.saturation = -0.1));
        assert!(ok(&|w| w.border_width = 20.0) && !ok(&|w| w.border_width = 20.1));
        assert!(ok(&|w| w.shadow = 1.0) && !ok(&|w| w.shadow = 1.1));
        for set in [
            (|w: &mut WebcamStyle, v| w.chroma_key.similarity = v) as fn(&mut WebcamStyle, f64),
            |w, v| w.chroma_key.smoothness = v,
            |w, v| w.chroma_key.spill = v,
            |w, v| w.background.blur = v,
        ] {
            assert!(ok(&|w| set(w, 0.0)) && ok(&|w| set(w, 1.0)));
            assert!(
                !ok(&|w| set(w, 1.01)) && !ok(&|w| set(w, -0.01)) && !ok(&|w| set(w, f64::NAN))
            );
        }
        // Background image / gradient names and strings are validated.
        assert!(ok(&|w| w.background.image = Some("bg.png".into())));
        assert!(!ok(&|w| w.background.image = Some("..\\bg.png".into())));
        assert!(!ok(&|w| w.background.image = Some("bg.gif".into())));
        assert!(!ok(
            &|w| w.background.gradient = "x".repeat(limits::MAX_STRING + 1)
        ));
    }

    /// `fixtures/project.editor-v2.json`: every editor-v2 field populated, so the UI
    /// side can build against (and test with) the exact wire shape.
    #[test]
    fn editor_v2_fixture_matches_the_types() {
        let mut p = sample();
        p.name = "Editor v2 fixture".into();
        p.edits.speed = vec![
            SpeedSegment {
                start_ms: 500,
                end_ms: 1000,
                speed: 2.0,
            },
            SpeedSegment {
                start_ms: 1000,
                end_ms: 2000,
                speed: 0.5,
            },
        ];
        p.edits.style.frame = WindowFrame::Laptop;
        p.edits.style.background_image = Some(BackgroundImage {
            file: "background-0123456789abcdef.png".into(),
            fit: BackgroundFit::Contain,
            blur: 12.0,
        });
        let w = &mut p.edits.style.webcam;
        w.enabled = true;
        w.margin = 0.05;
        w.border_width = 4.0;
        w.border_color = Rgba([0x4e, 0xd2, 0xfb, 255]);
        w.mirror = true;
        w.brightness = 0.1;
        w.contrast = 1.1;
        w.saturation = 1.2;
        w.shadow = 0.6;
        w.chroma_key.enabled = true;
        w.background.kind = WebcamBackgroundKind::Image;
        w.background.image = Some("background-0123456789abcdef.png".into());
        let kinds = [
            AnnotationKind::Arrow,
            AnnotationKind::Text,
            AnnotationKind::Rect,
            AnnotationKind::Ellipse,
            AnnotationKind::Highlight,
            AnnotationKind::Spotlight,
            AnnotationKind::Blur,
            AnnotationKind::Pixelate,
        ];
        p.edits.annotations = kinds
            .iter()
            .enumerate()
            .map(|(i, &kind)| Annotation {
                id: format!("ann-{i}"),
                kind,
                start_ms: 100 * i as u64,
                end_ms: 100 * i as u64 + 1500,
                x: 0.1,
                y: 0.2,
                w: 0.3,
                h: 0.2,
                x2: 0.7,
                y2: 0.6,
                text: if kind == AnnotationKind::Text {
                    "Look here".into()
                } else {
                    String::new()
                },
                ..Annotation::default()
            })
            .collect();
        p.validate().unwrap();
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/project.editor-v2.json");
        let value = serde_json::to_value(&p).unwrap();
        if std::env::var_os("FROSTCAP_UPDATE_FIXTURES").is_some() {
            fs::write(&path, serde_json::to_string_pretty(&value).unwrap() + "\n").unwrap();
        }
        let raw: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw, value, "stale fixture (set FROSTCAP_UPDATE_FIXTURES=1)");
        let back: Project = serde_json::from_value(raw).unwrap();
        assert_eq!(back, p);
        back.validate().unwrap();
    }
}
