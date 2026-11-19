//! Render model: everything the compositor needs, built from the typed,
//! validated [`frostcap_project::Project`].
//!
//! `tracks.*` are in **physical source pixels** and are normalized here,
//! `edits.zoomSegments` carry a normalized `center`, `edits.trims` become the kept
//! pieces of the output timeline, and `edits.style` maps to the editor-unit
//! [`Style`]. The project schema and its validation (size caps, plain media file
//! names, numeric ranges) live in `frostcap-project`; this module does no JSON
//! parsing of its own. Every editor field (frame, cursor, webcam bubble, audio mix
//! and processing, caption style and cues) is typed in the project; only clip speed
//! is not stored (pieces are always 1x) and can be set programmatically.

use std::path::{Path, PathBuf};

use frostcap_project::Project;

use crate::error::{RenderError, Result};

/// Zoom easing curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Easing {
    /// Constant speed.
    Linear,
    /// Cubic ease-in-out (`easeInOutCubic` / UI `easeInOut`).
    EaseInOutCubic,
    /// Cubic ease-out (UI `easeOut`).
    EaseOutCubic,
    /// Quintic ease-out (core `easeOutQuint`).
    EaseOutQuint,
    /// Damped spring (overshoots slightly).
    Spring,
}

/// Window frame drawn above the recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    /// No title bar.
    None,
    /// Thin bar with a centred pill.
    Minimal,
    /// Windows 11 style caption buttons.
    Windows,
    /// Traffic lights.
    MacOs,
}

/// Webcam bubble shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebcamShape {
    /// Circle.
    Circle,
    /// Rounded square (radius 22 %).
    Rounded,
    /// Square (6 px radius at 1080p).
    Square,
}

/// Linear gradient, stops evenly spaced, sRGB-encoded RGBA 0..1.
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    /// CSS angle in degrees.
    pub angle_deg: f64,
    /// Colour stops.
    pub stops: Vec<[f64; 4]>,
}

/// A zoom segment on the source clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomSegment {
    /// Start (ms, source clock).
    pub start_ms: f64,
    /// End (ms, inclusive like the preview).
    pub end_ms: f64,
    /// Focus x, 0..1 of the canvas.
    pub cx: f64,
    /// Focus y, 0..1.
    pub cy: f64,
    /// Magnification.
    pub scale: f64,
    /// Ramp curve.
    pub easing: Easing,
    /// Ramp length (ms, capped at half the segment).
    pub transition_ms: f64,
}

/// A kept piece of the source timeline, played at `speed`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    /// Source start (ms).
    pub src_start: f64,
    /// Source end (ms).
    pub src_end: f64,
    /// Playback speed (> 0).
    pub speed: f64,
}

impl Piece {
    /// Output duration of this piece (ms).
    pub fn out_len(&self) -> f64 {
        (self.src_end - self.src_start) / self.speed
    }
}

/// Caption cue (source clock).
#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    /// Start (ms).
    pub start: f64,
    /// End (ms).
    pub end: f64,
    /// Text.
    pub text: String,
}

/// Key press for the overlay (source clock).
#[derive(Clone, Debug, PartialEq)]
pub struct KeyPress {
    /// Time (ms).
    pub t: f64,
    /// Label including modifiers, e.g. "Ctrl + S".
    pub label: String,
}

/// An audio file placed on the video clock.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioRef {
    /// Absolute path.
    pub path: PathBuf,
    /// Where sample 0 sits on the video clock (ms, may be negative).
    pub offset_ms: f64,
}

/// Audio post-processing settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioStyle {
    /// Noise gate on the microphone.
    pub noise_gate: bool,
    /// Gate threshold, dBFS.
    pub gate_threshold_db: f64,
    /// High-pass on the microphone.
    pub high_pass: bool,
    /// High-pass corner (Hz).
    pub high_pass_hz: f64,
    /// Loudness-normalize the final mix.
    pub normalize: bool,
    /// Target integrated loudness (LUFS).
    pub target_lufs: f64,
    /// Microphone gain, linear (0 = muted).
    pub mic_gain: f64,
    /// System audio gain, linear (0 = muted).
    pub system_gain: f64,
}

impl AudioStyle {
    /// All processing off.
    pub const OFF: Self = Self {
        noise_gate: false,
        gate_threshold_db: -50.0,
        high_pass: false,
        high_pass_hz: 80.0,
        normalize: false,
        target_lufs: -16.0,
        mic_gain: 1.0,
        system_gain: 1.0,
    };
}

/// Visual style (units follow the editor: padding % / radius px at 1080p / shadow 0..100).
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    /// Background gradient.
    pub background: Gradient,
    /// Padding, percent of `min(W, H*1.78)`.
    pub padding: f64,
    /// Corner radius, px at 1080p.
    pub radius: f64,
    /// Shadow strength 0..100.
    pub shadow: f64,
    /// Window frame.
    pub frame: Frame,
    /// Draw the re-rendered cursor (still suppressed when the cursor is baked).
    pub cursor_visible: bool,
    /// Cursor smoothing 0..1.
    pub cursor_smoothing: f64,
    /// Cursor size multiplier.
    pub cursor_size: f64,
    /// Click ripples.
    pub click_ripple: bool,
    /// Cursor motion blur 0..1.
    pub motion_blur: f64,
    /// Webcam bubble on.
    pub webcam_enabled: bool,
    /// Webcam shape.
    pub webcam_shape: WebcamShape,
    /// Webcam size, fraction of output height.
    pub webcam_size: f64,
    /// Webcam corner: (left, top).
    pub webcam_corner: (bool, bool),
    /// Audio processing.
    pub audio: AudioStyle,
    /// Captions on.
    pub captions_enabled: bool,
    /// Caption font size, px at 1080p.
    pub caption_font: f64,
    /// Captions at the top instead of the bottom.
    pub caption_top: bool,
    /// Dark box behind captions.
    pub caption_bg: bool,
    /// Keystroke overlay.
    pub show_keys: bool,
}

impl Default for Style {
    /// Defaults of `frostcap_core::project::Style` translated to editor units.
    fn default() -> Self {
        Self {
            background: parse_background_str("linear-gradient(135deg,#a1c4fd,#c2e9fb)")
                .expect("default gradient"),
            padding: 8.0,
            radius: 12.0,
            shadow: 50.0,
            frame: Frame::None,
            cursor_visible: true,
            cursor_smoothing: 0.6,
            cursor_size: 1.0,
            click_ripple: true,
            motion_blur: 0.0,
            webcam_enabled: false,
            webcam_shape: WebcamShape::Circle,
            webcam_size: 0.22,
            webcam_corner: (false, false),
            audio: AudioStyle::OFF,
            captions_enabled: true,
            caption_font: 40.0,
            caption_top: false,
            caption_bg: true,
            show_keys: false,
        }
    }
}

/// Everything the renderer needs from a project.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderProject {
    /// Project folder.
    pub dir: PathBuf,
    /// Source pixel size.
    pub canvas: (u32, u32),
    /// Capture fps.
    pub fps: u32,
    /// Source duration (ms).
    pub duration_ms: f64,
    /// Screen video.
    pub screen: PathBuf,
    /// Webcam video, if any.
    pub webcam: Option<PathBuf>,
    /// Webcam start on the video clock (ms).
    pub webcam_offset_ms: f64,
    /// Microphone.
    pub mic: Option<AudioRef>,
    /// System audio.
    pub system: Option<AudioRef>,
    /// Cursor samples (t ms, x 0..1, y 0..1).
    pub cursor: Vec<(f64, f64, f64)>,
    /// Click-downs (t ms, x 0..1, y 0..1).
    pub clicks: Vec<(f64, f64, f64)>,
    /// Key presses.
    pub keys: Vec<KeyPress>,
    /// Zoom segments.
    pub zooms: Vec<ZoomSegment>,
    /// Output timeline: kept source pieces in order.
    pub pieces: Vec<Piece>,
    /// Captions.
    pub captions: Vec<Cue>,
    /// Style.
    pub style: Style,
    /// The OS cursor is already in `screen.mp4` (then no cursor is re-rendered).
    pub cursor_baked: bool,
}
impl From<frostcap_project::Easing> for Easing {
    fn from(e: frostcap_project::Easing) -> Self {
        match e {
            frostcap_project::Easing::Linear => Self::Linear,
            frostcap_project::Easing::EaseInOutCubic => Self::EaseInOutCubic,
            frostcap_project::Easing::EaseOutQuint => Self::EaseOutQuint,
            frostcap_project::Easing::Spring => Self::Spring,
        }
    }
}

/// Parse `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb(..)`, `rgba(..)`.
pub fn parse_color(s: &str) -> Option<[f64; 4]> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix('#') {
        let hex = |i: usize, n: usize| u32::from_str_radix(h.get(i..i + n)?, 16).ok();
        return match h.len() {
            3 | 4 => {
                let c = |i| hex(i, 1).map(|x| (x * 17) as f64 / 255.0);
                Some([c(0)?, c(1)?, c(2)?, if h.len() == 4 { c(3)? } else { 1.0 }])
            }
            6 | 8 => {
                let c = |i| hex(i, 2).map(|x| x as f64 / 255.0);
                Some([c(0)?, c(2)?, c(4)?, if h.len() == 8 { c(6)? } else { 1.0 }])
            }
            _ => None,
        };
    }
    let inner = s
        .strip_prefix("rgba(")
        .or_else(|| s.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let parts: Vec<f64> = inner
        .split([',', '/', ' '])
        .filter(|p| !p.is_empty())
        .map(|p| p.trim().parse::<f64>().ok().filter(|v| v.is_finite()))
        .collect::<Option<_>>()?;
    let c8 = |v: f64| (v / 255.0).clamp(0.0, 1.0);
    match parts.as_slice() {
        [r, g, b] => Some([c8(*r), c8(*g), c8(*b), 1.0]),
        [r, g, b, a] => Some([c8(*r), c8(*g), c8(*b), a.clamp(0.0, 1.0)]),
        _ => None,
    }
}

/// Split on commas that are not inside parentheses.
fn split_top(s: &str) -> Vec<&str> {
    let (mut depth, mut start, mut out) = (0i32, 0usize, Vec::new());
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

/// Parse a CSS background: a colour or `linear-gradient(<angle>deg, c1, c2, ...)`.
/// Stop positions are ignored (stops are spread evenly, as in the editor).
pub fn parse_background_str(s: &str) -> Option<Gradient> {
    let s = s.trim();
    if let Some(inner) = s
        .strip_prefix("linear-gradient(")
        .and_then(|r| r.strip_suffix(')'))
    {
        let parts = split_top(inner);
        let mut angle = 180.0;
        let mut stops = Vec::new();
        for (i, p) in parts.iter().enumerate() {
            if i == 0 {
                if let Some(a) = p.strip_suffix("deg") {
                    // A non-finite angle keeps the default instead of poisoning the gradient.
                    angle = a
                        .trim()
                        .parse::<f64>()
                        .ok()
                        .filter(|a| a.is_finite())
                        .unwrap_or(180.0);
                    continue;
                }
                if let Some(dir) = p.strip_prefix("to ") {
                    angle = match dir.trim() {
                        "top" => 0.0,
                        "right" => 90.0,
                        "left" => 270.0,
                        _ => 180.0,
                    };
                    continue;
                }
            }
            // "color 40%" -> colour part only.
            let col = p
                .rsplit_once(' ')
                .filter(|(_, pos)| pos.ends_with('%'))
                .map_or(*p, |(c, _)| c.trim());
            stops.push(parse_color(col)?);
        }
        if stops.is_empty() {
            return None;
        }
        return Some(Gradient {
            angle_deg: angle,
            stops,
        });
    }
    parse_color(s).map(|c| Gradient {
        angle_deg: 180.0,
        stops: vec![c],
    })
}

fn style_from(s: &frostcap_project::Style) -> Style {
    use frostcap_project as fp;
    let mut st = Style::default();
    if let Some(bg) = parse_background_str(&s.background) {
        st.background = bg;
    }
    // The project stores fractions (padding 0..0.5, shadow 0..1); the editor
    // units are percents.
    st.padding = (s.padding.max(0.0) * 100.0).min(45.0);
    st.radius = s.corner_radius.max(0.0);
    st.shadow = (s.shadow.max(0.0) * 100.0).clamp(0.0, 100.0);
    st.frame = match s.frame {
        fp::WindowFrame::None => Frame::None,
        fp::WindowFrame::Minimal => Frame::Minimal,
        fp::WindowFrame::Windows => Frame::Windows,
        fp::WindowFrame::MacOs => Frame::MacOs,
    };
    st.cursor_visible = s.cursor.visible;
    st.cursor_size = s.cursor_scale.clamp(0.1, 8.0);
    // `cursorSmoothing` is the on/off switch, `cursor.smoothing` the amount.
    st.cursor_smoothing = if s.cursor_smoothing {
        s.cursor.smoothing.clamp(0.0, 1.0)
    } else {
        0.0
    };
    st.motion_blur = s.cursor.motion_blur.clamp(0.0, 1.0);
    st.click_ripple = s.show_clicks;
    st.show_keys = s.show_keys;
    let w = &s.webcam;
    st.webcam_enabled = w.enabled;
    st.webcam_shape = match w.shape {
        fp::WebcamShape::Circle => WebcamShape::Circle,
        fp::WebcamShape::Rounded => WebcamShape::Rounded,
        fp::WebcamShape::Square => WebcamShape::Square,
    };
    st.webcam_size = w.size.clamp(0.05, 0.6);
    st.webcam_corner = match w.position {
        fp::Corner::TopLeft => (true, true),
        fp::Corner::TopRight => (false, true),
        fp::Corner::BottomLeft => (true, false),
        fp::Corner::BottomRight => (false, false),
    };
    let a = &s.audio;
    st.audio = AudioStyle {
        noise_gate: a.noise_gate,
        gate_threshold_db: a.noise_gate_threshold.clamp(-100.0, 0.0),
        high_pass: a.high_pass,
        high_pass_hz: a.high_pass_hz.clamp(10.0, 1000.0),
        normalize: a.normalize,
        target_lufs: a.target_lufs.clamp(-40.0, -5.0),
        mic_gain: a.mic_gain.clamp(0.0, 4.0),
        system_gain: a.system_gain.clamp(0.0, 4.0),
    };
    let c = &s.captions;
    st.captions_enabled = c.enabled;
    st.caption_font = c.font_size.clamp(8.0, 200.0);
    st.caption_top = c.position == fp::CaptionPosition::Top;
    st.caption_bg = c.background;
    st
}

/// Cues that can be shown (positive duration, some visible text), in time order.
fn cues_from(cues: &[frostcap_project::CaptionCue]) -> Vec<Cue> {
    let mut out: Vec<Cue> = cues
        .iter()
        .filter_map(|c| {
            let text = c.text.trim();
            (c.end_ms > c.start_ms && !text.is_empty()).then(|| Cue {
                start: c.start_ms as f64,
                end: c.end_ms as f64,
                text: text.to_owned(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}
fn key_label(k: &frostcap_project::KeyEvent) -> String {
    let m = k.modifiers;
    let mut out = String::new();
    for (on, label) in [
        (m.ctrl, "Ctrl"),
        (m.alt, "Alt"),
        (m.shift, "Shift"),
        (m.win, "Win"),
    ] {
        if on {
            out.push_str(label);
            out.push_str(" + ");
        }
    }
    out.push_str(&k.label);
    out
}

fn audio_ref(dir: &Path, t: Option<&frostcap_project::AudioTrack>) -> Option<AudioRef> {
    t.map(|t| AudioRef {
        path: dir.join(&t.file),
        offset_ms: t.offset_ms as f64,
    })
}

impl RenderProject {
    /// Load and validate `<dir>/project.json` (size cap, schema and media file
    /// name checks all come from `frostcap-project`).
    pub fn load(dir: &Path) -> Result<Self> {
        let project = Project::load(dir).map_err(|e| match e {
            frostcap_project::Error::Io { path, source } => RenderError::Io { path, source },
            e => RenderError::Project(e.to_string()),
        })?;
        Self::from_project(dir, &project)
    }

    /// Build the render model from a typed project. The project is validated
    /// first, so a hostile project (media names that escape the folder, huge
    /// arrays, out-of-range numbers) can never reach the exporter even when the
    /// caller built it in memory.
    pub fn from_project(dir: &Path, p: &Project) -> Result<Self> {
        p.validate()
            .map_err(|e| RenderError::Project(e.to_string()))?;
        let (cw, ch) = (p.canvas.width, p.canvas.height);
        if cw < 2 || ch < 2 {
            return Err(RenderError::Project("missing canvas size".into()));
        }
        let (fw, fh) = (f64::from(cw), f64::from(ch));
        let duration_ms = p.duration_ms as f64;
        let m = &p.media;
        let (webcam, webcam_offset_ms) = match (&m.webcam_video, &m.webcam) {
            (Some(v), _) => (Some(dir.join(&v.file)), v.offset_ms as f64),
            (None, Some(f)) => (Some(dir.join(f)), 0.0),
            (None, None) => (None, 0.0),
        };

        let mut cursor: Vec<(f64, f64, f64)> = p
            .tracks
            .cursor
            .iter()
            .map(|s| (s.t_ms as f64, f64::from(s.x) / fw, f64::from(s.y) / fh))
            .collect();
        cursor.sort_by(|a, b| a.0.total_cmp(&b.0));
        let clicks: Vec<(f64, f64, f64)> = p
            .tracks
            .clicks
            .iter()
            .filter(|c| c.state == frostcap_project::ButtonState::Down)
            .map(|c| (c.t_ms as f64, f64::from(c.x) / fw, f64::from(c.y) / fh))
            .collect();
        let keys: Vec<KeyPress> = p
            .tracks
            .keys
            .iter()
            .map(|k| KeyPress {
                t: k.t_ms as f64,
                label: key_label(k),
            })
            .collect();

        let mut zooms: Vec<ZoomSegment> = p
            .edits
            .zoom_segments
            .iter()
            .filter(|z| z.end_ms > z.start_ms)
            .map(|z| ZoomSegment {
                start_ms: z.start_ms as f64,
                end_ms: z.end_ms as f64,
                cx: z.center.x.clamp(0.0, 1.0),
                cy: z.center.y.clamp(0.0, 1.0),
                scale: z.scale.clamp(1.0, 8.0),
                easing: z.easing.into(),
                transition_ms: z.transition_ms as f64,
            })
            .collect();
        zooms.sort_by(|a, b| a.start_ms.total_cmp(&b.start_ms));

        Ok(Self {
            dir: dir.to_path_buf(),
            canvas: (cw, ch),
            fps: p.fps.clamp(1, 240),
            duration_ms,
            screen: dir.join(&m.screen),
            webcam,
            webcam_offset_ms,
            mic: audio_ref(dir, m.mic_audio.as_ref()),
            system: audio_ref(dir, m.system_audio.as_ref()),
            cursor,
            clicks,
            keys,
            zooms,
            pieces: pieces_from_trims(&p.edits.trims, duration_ms),
            captions: cues_from(&p.edits.captions),
            style: style_from(&p.edits.style),
            cursor_baked: p.cursor_baked,
        })
    }

    /// Clamp the claimed duration (and every kept piece) to the real length of
    /// the decoded source, so a project that overstates `durationMs` cannot size
    /// frame lists or audio buffers beyond what exists. `real_ms <= 0` = unknown.
    pub fn clamp_to_source(&mut self, real_ms: f64) {
        if real_ms.is_nan() || real_ms <= 0.0 || real_ms >= self.duration_ms {
            return;
        }
        self.duration_ms = real_ms;
        for p in &mut self.pieces {
            p.src_end = p.src_end.min(real_ms);
        }
        self.pieces.retain(|p| p.src_end > p.src_start);
    }

    /// Total output duration (ms).
    pub fn output_ms(&self) -> f64 {
        self.pieces.iter().map(Piece::out_len).sum()
    }

    /// Map output time to source time (ms). Clamped to the last piece.
    pub fn source_time(&self, t_out: f64) -> f64 {
        let mut acc = 0.0;
        for p in &self.pieces {
            let len = p.out_len();
            if t_out < acc + len {
                return p.src_start + (t_out - acc).max(0.0) * p.speed;
            }
            acc += len;
        }
        self.pieces.last().map_or(0.0, |p| p.src_end)
    }

    /// Whether anything on the timeline differs from the raw recording
    /// (zooms, cuts, speed, captions). Style alone does not count.
    pub fn has_timeline_edits(&self) -> bool {
        let identity = self.pieces.len() == 1
            && self.pieces[0].src_start <= 0.5
            && self.pieces[0].src_end >= self.duration_ms - 0.5
            && (self.pieces[0].speed - 1.0).abs() < 1e-9;
        !self.zooms.is_empty()
            || !identity
            || (self.style.captions_enabled && !self.captions.is_empty())
    }

    /// Whether a re-rendered cursor is drawn.
    pub fn draws_cursor(&self) -> bool {
        self.style.cursor_visible && !self.cursor_baked && !self.cursor.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn empty_for_tests() -> Self {
        Self {
            dir: PathBuf::new(),
            canvas: (1920, 1080),
            fps: 60,
            duration_ms: 3000.0,
            screen: PathBuf::from("screen.mp4"),
            webcam: None,
            webcam_offset_ms: 0.0,
            mic: None,
            system: None,
            cursor: vec![],
            clicks: vec![],
            keys: vec![],
            zooms: vec![],
            pieces: vec![Piece {
                src_start: 0.0,
                src_end: 3000.0,
                speed: 1.0,
            }],
            captions: vec![],
            style: Style::default(),
            cursor_baked: false,
        }
    }
}

/// The kept source pieces: the complement of the (possibly overlapping) trims.
fn pieces_from_trims(trims: &[frostcap_project::TrimRange], duration: f64) -> Vec<Piece> {
    let mut trims: Vec<(f64, f64)> = trims
        .iter()
        .map(|t| (t.start_ms as f64, t.end_ms as f64))
        .filter(|(s, e)| e > s)
        .collect();
    trims.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut pieces = Vec::new();
    let mut cur = 0.0;
    for (s, e) in trims {
        if s > cur {
            pieces.push(Piece {
                src_start: cur,
                src_end: s.min(duration),
                speed: 1.0,
            });
        }
        cur = cur.max(e);
    }
    if duration > cur {
        pieces.push(Piece {
            src_start: cur,
            src_end: duration,
            speed: 1.0,
        });
    }
    pieces.retain(|p| p.src_end > p.src_start);
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;
    use frostcap_project::{
        AudioTrack, ButtonState, ClickEvent, CursorSample, Easing as PEasing, KeyEvent, Modifiers,
        MouseButton, NormPoint, SegmentOrigin, TrimRange, VideoTrack, ZoomSegment as PZoom,
        samples::sample,
    };
    use serde_json::json;

    fn core_project() -> Project {
        let mut p = sample();
        p.canvas = frostcap_project::Size {
            width: 2000,
            height: 1000,
        };
        p.duration_ms = 10_000;
        p.media.system_audio = Some(AudioTrack {
            file: "audio-system.wav".into(),
            offset_ms: -3589,
            sample_rate: 48_000,
            channels: 2,
            duration_ms: 7000,
        });
        p.tracks.cursor = vec![
            CursorSample {
                t_ms: 0,
                x: 1000,
                y: 500,
            },
            CursorSample {
                t_ms: 100,
                x: 2000,
                y: 0,
            },
        ];
        let click = |t_ms, state| ClickEvent {
            t_ms,
            x: 500,
            y: 250,
            button: MouseButton::Left,
            state,
        };
        p.tracks.clicks = vec![click(10, ButtonState::Down), click(20, ButtonState::Up)];
        p.tracks.keys = vec![KeyEvent {
            t_ms: 5,
            vk: 83,
            label: "S".into(),
            modifiers: Modifiers {
                ctrl: true,
                ..Default::default()
            },
        }];
        p.edits.zoom_segments = vec![PZoom {
            start_ms: 1000,
            end_ms: 3000,
            center: NormPoint { x: 0.3, y: 0.6 },
            scale: 2.0,
            easing: PEasing::EaseOutQuint,
            transition_ms: 400,
            origin: SegmentOrigin::Manual,
        }];
        p.edits.trims = vec![TrimRange {
            start_ms: 2000,
            end_ms: 4000,
        }];
        p.edits.style.cursor_scale = 1.5;
        p.edits.style.show_keys = true;
        p.cursor_baked = true;
        p
    }

    fn model(p: &Project) -> RenderProject {
        RenderProject::from_project(Path::new("C:/p.frostcap"), p).unwrap()
    }

    #[test]
    fn parses_core_schema() {
        let p = model(&core_project());
        assert_eq!(p.canvas, (2000, 1000));
        assert_eq!(p.cursor[1], (100.0, 1.0, 0.0));
        assert_eq!(p.clicks, vec![(10.0, 0.25, 0.25)]);
        assert_eq!(p.keys[0].label, "Ctrl + S");
        assert_eq!(p.zooms[0].easing, Easing::EaseOutQuint);
        assert_eq!(p.zooms[0].transition_ms, 400.0);
        assert_eq!((p.zooms[0].cx, p.zooms[0].cy), (0.3, 0.6));
        assert_eq!(p.system.as_ref().unwrap().offset_ms, -3589.0);
        assert!(
            p.system
                .as_ref()
                .unwrap()
                .path
                .ends_with("audio-system.wav")
        );
        assert!((p.style.padding - 8.0).abs() < 1e-9);
        assert!((p.style.shadow - 50.0).abs() < 1e-9);
        assert_eq!(p.style.radius, 12.0);
        assert_eq!(p.style.cursor_size, 1.5);
        assert!(p.style.show_keys);
        assert_eq!(p.style.background.stops.len(), 2);
        assert_eq!(p.style.background.angle_deg, 135.0);
        // Trim 2..4 s -> pieces [0,2000] + [4000,10000].
        assert_eq!(p.pieces.len(), 2);
        assert_eq!(p.output_ms(), 8000.0);
        assert_eq!(p.source_time(1999.0), 1999.0);
        assert_eq!(p.source_time(2000.0), 4000.0);
        assert!(p.has_timeline_edits());
        assert!(p.cursor_baked && !p.draws_cursor());
    }

    #[test]
    fn cursor_baked_flag_and_raw_detection() {
        let mut v = core_project();
        v.cursor_baked = false;
        v.edits.zoom_segments.clear();
        v.edits.trims.clear();
        let p = model(&v);
        assert!(p.draws_cursor());
        assert!(!p.has_timeline_edits());
        assert_eq!(p.output_ms(), 10000.0);
    }

    #[test]
    fn rejects_hostile_media_paths() {
        let hostile = [
            r"C:\Windows\win.ini",
            r"\attacker\share\x.wav",
            r"\?\C:\x.mp4",
            "../outside.mp4",
            r"..\outside.mp4",
            "sub/screen.mp4",
            r"sub\screen.mp4",
            "screen.mp4:ads",
            "..",
            "",
        ];
        for bad in hostile {
            let wav = |f: &str| AudioTrack {
                file: f.into(),
                offset_ms: 0,
                sample_rate: 48_000,
                channels: 2,
                duration_ms: 1,
            };
            let mut cases = Vec::new();
            let mut p = core_project();
            p.media.screen = bad.into();
            cases.push(("screen", p));
            let mut p = core_project();
            p.media.system_audio = Some(wav(bad));
            cases.push(("systemAudio", p));
            let mut p = core_project();
            p.media.mic_audio = Some(wav(bad));
            cases.push(("micAudio", p));
            let mut p = core_project();
            p.media.webcam_video = Some(VideoTrack {
                file: bad.into(),
                name: "c".into(),
                width: 2,
                height: 2,
                offset_ms: 0,
                duration_ms: 1,
            });
            cases.push(("webcamVideo", p));
            let mut p = core_project();
            p.media.webcam = Some(bad.into());
            cases.push(("webcam", p));
            for (field, p) in cases {
                let r = RenderProject::from_project(Path::new("C:/p.frostcap"), &p);
                assert!(r.is_err(), "{field} = {bad:?} was accepted");
            }
        }
        // Plain basenames are fine.
        let mut p = core_project();
        p.media.screen = "my screen.mp4".into();
        assert!(RenderProject::from_project(Path::new("C:/p.frostcap"), &p).is_ok());
    }

    #[test]
    fn hostile_numbers_and_sizes_are_rejected() {
        let mut p = core_project();
        p.edits.style.padding = f64::NAN;
        assert!(RenderProject::from_project(Path::new("x"), &p).is_err());
        let mut p = core_project();
        p.canvas.width = 1;
        assert!(RenderProject::from_project(Path::new("x"), &p).is_err());
        let mut p = core_project();
        p.version = 99;
        assert!(RenderProject::from_project(Path::new("x"), &p).is_err());
    }

    #[test]
    fn project_json_size_is_capped_by_the_project_crate() {
        let dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))
            .join("test-tmp")
            .join(format!("render-huge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("project.json"),
            serde_json::to_vec(&core_project()).unwrap(),
        )
        .unwrap();
        assert!(RenderProject::load(&dir).is_ok());
        let f = std::fs::File::create(dir.join("project.json")).unwrap();
        f.set_len(frostcap_project::limits::MAX_PROJECT_BYTES + 1)
            .unwrap();
        drop(f);
        assert!(matches!(
            RenderProject::load(&dir),
            Err(RenderError::Project(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_project_json_is_an_io_error() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("no-such-project");
        assert!(matches!(
            RenderProject::load(&dir),
            Err(RenderError::Io { .. })
        ));
    }

    #[test]
    fn webcam_video_track_from_core_schema() {
        let mut p = core_project();
        p.media.webcam_video = Some(VideoTrack {
            file: "webcam.mp4".into(),
            name: "Cam".into(),
            width: 1280,
            height: 720,
            offset_ms: 120,
            duration_ms: 9000,
        });
        let p = model(&p);
        assert!(p.webcam.as_ref().unwrap().ends_with("webcam.mp4"));
        assert_eq!(p.webcam_offset_ms, 120.0);
    }

    #[test]
    fn typed_editor_fields_reach_the_render_model() {
        use frostcap_project::{
            AudioStyle as PAudio, CaptionCue, CaptionPosition, CaptionStyle, Corner, CursorStyle,
            WebcamShape as PShape, WebcamStyle, WindowFrame,
        };
        let mut p = core_project();
        let s = &mut p.edits.style;
        s.frame = WindowFrame::MacOs;
        s.cursor = CursorStyle {
            visible: false,
            smoothing: 0.3,
            motion_blur: 0.5,
        };
        s.cursor_smoothing = true;
        s.show_clicks = false;
        s.webcam = WebcamStyle {
            enabled: true,
            shape: PShape::Rounded,
            size: 0.3,
            position: Corner::TopLeft,
        };
        s.audio = PAudio {
            noise_gate: false,
            noise_gate_threshold: -40.0,
            high_pass: true,
            high_pass_hz: 120.0,
            normalize: false,
            target_lufs: -14.0,
            mic_gain: 0.0,
            system_gain: 1.5,
        };
        s.captions = CaptionStyle {
            enabled: true,
            font_size: 48.0,
            position: CaptionPosition::Top,
            background: false,
        };
        p.edits.captions = vec![
            CaptionCue {
                start_ms: 500,
                end_ms: 900,
                text: "  hi  ".into(),
            },
            CaptionCue {
                start_ms: 100,
                end_ms: 100,
                text: "zero length".into(),
            },
            CaptionCue {
                start_ms: 100,
                end_ms: 200,
                text: "   ".into(),
            },
        ];
        p.edits.splits = vec![1000, 5000];
        let m = model(&p);
        let st = &m.style;
        assert_eq!(st.frame, Frame::MacOs);
        assert!(!st.cursor_visible && !st.click_ripple);
        assert_eq!((st.cursor_smoothing, st.motion_blur), (0.3, 0.5));
        assert!(st.webcam_enabled);
        assert_eq!(
            (st.webcam_shape, st.webcam_size, st.webcam_corner),
            (WebcamShape::Rounded, 0.3, (true, true))
        );
        let a = st.audio;
        assert!(!a.noise_gate && a.high_pass && !a.normalize);
        assert_eq!(
            (a.gate_threshold_db, a.high_pass_hz, a.target_lufs),
            (-40.0, 120.0, -14.0)
        );
        assert_eq!((a.mic_gain, a.system_gain), (0.0, 1.5));
        assert!(st.captions_enabled && st.caption_top && !st.caption_bg);
        assert_eq!(st.caption_font, 48.0);
        assert_eq!(
            m.captions,
            vec![Cue {
                start: 500.0,
                end: 900.0,
                text: "hi".into()
            }]
        );
        // Splits are bookkeeping only: the kept pieces come from the trims.
        let mut no_splits = p.clone();
        no_splits.edits.splits.clear();
        assert_eq!(m.pieces, model(&no_splits).pieces);
    }

    #[test]
    fn cursor_smoothing_switch_overrides_the_amount() {
        let mut p = core_project();
        p.edits.style.cursor_smoothing = false;
        p.edits.style.cursor.smoothing = 0.9;
        assert_eq!(model(&p).style.cursor_smoothing, 0.0);
    }

    #[test]
    fn webcam_corners_map_to_left_top_flags() {
        use frostcap_project::Corner::*;
        for (c, want) in [
            (TopLeft, (true, true)),
            (TopRight, (false, true)),
            (BottomLeft, (true, false)),
            (BottomRight, (false, false)),
        ] {
            let mut p = core_project();
            p.edits.style.webcam.position = c;
            assert_eq!(model(&p).style.webcam_corner, want, "{c:?}");
        }
    }

    #[test]
    fn a_fresh_project_gets_the_editor_defaults() {
        let m = model(&core_project());
        assert_eq!(m.style.frame, Frame::Minimal);
        assert!(m.style.cursor_visible && !m.style.webcam_enabled);
        assert!(m.style.audio.noise_gate && m.style.audio.high_pass && m.style.audio.normalize);
        assert_eq!(
            (m.style.audio.mic_gain, m.style.audio.system_gain),
            (1.0, 1.0)
        );
        assert_eq!(
            (
                m.style.caption_font,
                m.style.caption_top,
                m.style.caption_bg
            ),
            (40.0, false, true)
        );
        assert!(m.captions.is_empty());
    }

    #[test]
    fn clamp_to_source_shortens_duration_and_pieces() {
        let mut p = model(&core_project()); // 10 s, trim 2..4 s -> [0,2000] + [4000,10000]
        let before = p.clone();
        p.clamp_to_source(0.0); // unknown duration: untouched
        p.clamp_to_source(20_000.0); // longer than claimed: untouched
        assert_eq!(p, before);
        p.clamp_to_source(5_000.0);
        assert_eq!(p.duration_ms, 5_000.0);
        assert_eq!(p.output_ms(), 3_000.0); // 2000 + 1000
        p.clamp_to_source(3_000.0);
        assert_eq!(p.pieces.len(), 1);
        assert_eq!(p.output_ms(), 2_000.0);
        p.clamp_to_source(1_500.0);
        assert_eq!((p.duration_ms, p.output_ms()), (1_500.0, 1_500.0));
    }
    #[test]
    fn root_cursor_baked_is_read_from_the_typed_project() {
        // The flag is a root field of project.json.
        let mut v = serde_json::to_value(core_project()).unwrap();
        v["cursorBaked"] = json!(false);
        let p: Project = serde_json::from_value(v).unwrap();
        assert!(model(&p).draws_cursor());
    }

    /// Characterization of the golden fixture (`fixtures/project.sample.json`):
    /// pins the render model across the typed-project refactor.
    #[test]
    fn golden_fixture_render_model() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/project.sample.json");
        let p: Project = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let p = RenderProject::from_project(Path::new("C:/p.frostcap"), &p).unwrap();
        assert_eq!(p.canvas, (1920, 1080));
        assert_eq!((p.fps, p.duration_ms), (60, 3000.0));
        assert!(p.screen.ends_with("screen.mp4"));
        assert!(p.webcam.as_ref().unwrap().ends_with("webcam.mp4"));
        assert_eq!(p.webcam_offset_ms, 0.0);
        assert!(p.mic.as_ref().unwrap().path.ends_with("audio-mic.wav"));
        assert!(p.system.is_none());
        assert_eq!(p.cursor, vec![(0.0, 1.0 / 1920.0, 2.0 / 1080.0)]);
        assert_eq!(p.clicks, vec![(5.0, 1.0 / 1920.0, 2.0 / 1080.0)]);
        assert_eq!(p.keys.len(), 1);
        assert_eq!(p.keys[0].label, "Ctrl + S");
        assert_eq!(p.zooms.len(), 1);
        let z = p.zooms[0];
        assert_eq!((z.start_ms, z.end_ms, z.scale), (0.0, 1800.0, 1.8));
        assert_eq!((z.cx, z.cy, z.transition_ms), (0.278, 0.278, 650.0));
        assert_eq!(z.easing, Easing::EaseInOutCubic);
        assert_eq!(
            p.pieces,
            vec![Piece {
                src_start: 0.0,
                src_end: 3000.0,
                speed: 1.0
            }]
        );
        // Fixture v2: every editor field is populated (frame, cursor, webcam, audio, captions).
        assert_eq!(p.captions.len(), 2);
        assert_eq!((p.captions[0].start, p.captions[0].end), (200.0, 1400.0));
        assert_eq!(p.captions[1].text, "Save with Ctrl + S");
        assert!(!p.cursor_baked && p.draws_cursor());
        let s = &p.style;
        assert_eq!((s.radius, s.cursor_size, s.motion_blur), (12.0, 1.0, 0.25));
        assert_eq!(s.cursor_smoothing, 0.5);
        assert!(s.click_ripple && s.show_keys && s.cursor_visible);
        assert_eq!(s.background.angle_deg, 135.0);
        assert_eq!(s.background.stops.len(), 2);
        assert_eq!(s.frame, Frame::Windows);
        assert!(s.webcam_enabled && s.captions_enabled);
        assert_eq!(
            (s.webcam_shape, s.webcam_size, s.webcam_corner),
            (WebcamShape::Rounded, 0.25, (false, true))
        );
        assert_eq!(
            (s.audio.mic_gain, s.audio.system_gain, s.audio.high_pass_hz),
            (1.0, 0.5, 100.0)
        );
        assert_eq!((s.caption_font, s.caption_top), (44.0, false));
        assert!(p.has_timeline_edits());
    }

    #[test]
    fn colors_and_angles_are_finite_and_clamped() {
        assert!(parse_color("rgb(nan,0,0)").is_none());
        assert!(parse_color("rgb(inf,0,0)").is_none());
        assert!(parse_color("rgba(0,0,0,NaN)").is_none());
        assert!(parse_color("rgb(1e999,0,0)").is_none());
        assert_eq!(parse_color("rgb(300,-5,255)"), Some([1.0, 0.0, 1.0, 1.0]));
        assert_eq!(parse_color("rgba(0,0,0,7)"), Some([0.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_color("rgba(0,0,0,-2)"), Some([0.0, 0.0, 0.0, 0.0]));
        for bad in ["nan", "inf", "-inf", "1e999"] {
            let g = parse_background_str(&format!("linear-gradient({bad}deg,#fff,#000)")).unwrap();
            assert!(g.angle_deg.is_finite(), "{bad}");
            assert_eq!(g.angle_deg, 180.0);
        }
        let g = parse_background_str("linear-gradient(135deg,#fff,#000)").unwrap();
        assert_eq!(g.angle_deg, 135.0);
    }
    #[test]
    fn colors_and_backgrounds() {
        assert_eq!(parse_color("#fff"), Some([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(parse_color("#000000"), Some([0.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_color("rgba(255,0,0,0.5)"), Some([1.0, 0.0, 0.0, 0.5]));
        assert!(parse_color("nope").is_none());
        let g = parse_background_str("linear-gradient(90deg, #ff0000 0%, rgb(0, 0, 255) 100%)")
            .unwrap();
        assert_eq!(g.angle_deg, 90.0);
        assert_eq!(g.stops[1], [0.0, 0.0, 1.0, 1.0]);
        let solid = parse_background_str("#0b1020").unwrap();
        assert_eq!(solid.stops.len(), 1);
    }

    #[test]
    fn typed_style_maps_to_editor_units() {
        let mut p = core_project();
        p.edits.style.padding = 0.45; // validate max == renderer clamp
        p.edits.style.shadow = 1.0;
        p.edits.style.cursor_smoothing = false;
        p.edits.style.show_clicks = false;
        p.edits.style.background = "#0b1020".into();
        let s = model(&p).style;
        assert_eq!(
            (s.padding, s.shadow, s.cursor_smoothing),
            (45.0, 100.0, 0.0)
        );
        assert!(!s.click_ripple);
        assert_eq!(s.background.stops.len(), 1);
    }

    #[test]
    fn overlapping_trims_leave_the_complement() {
        let mut p = core_project();
        p.edits.trims = vec![
            TrimRange {
                start_ms: 5000,
                end_ms: 6000,
            },
            TrimRange {
                start_ms: 1000,
                end_ms: 2000,
            },
            TrimRange {
                start_ms: 1500,
                end_ms: 5500,
            },
        ];
        let m = model(&p);
        let spans: Vec<(f64, f64)> = m.pieces.iter().map(|q| (q.src_start, q.src_end)).collect();
        assert_eq!(spans, vec![(0.0, 1000.0), (6000.0, 10000.0)]);
    }
}
