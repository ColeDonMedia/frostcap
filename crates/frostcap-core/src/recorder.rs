//! Recording session: screen (WGC -> Media Foundation H.264 MP4), optional webcam,
//! input metadata and WASAPI audio, written into a `<name>.frostcap/` folder.
//!
//! Lifecycle and crash safety:
//! 1. `start` checks free space, writes a provisional `project.json`
//!    (`status: "recording"`), starts audio + webcam + input (they drop data until
//!    the first video frame anchors the [`Timeline`]), then the screen capture.
//! 2. While running, `input.jsonl` (raw input events) and `session.json` (t0 and
//!    pauses) are kept up to date and WAV headers are re-patched every few seconds,
//!    so [`crate::recovery::recover_project`] can rebuild a crashed session.
//! 3. `stop` finalizes every file, writes the final `project.json` (even when the
//!    encoder failed: partial files are kept and a warning is recorded) and removes
//!    the sidecars. `Drop` performs the same cleanup if `stop` was never called.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

use crate::audio::{AudioCaptureInfo, AudioEndpoint, AudioRecorder};
use crate::clock::{self, HNS_PER_MS};
use crate::disk;
use crate::error::{Error, ErrorCode, ErrorPayload, Result};
use crate::input::{InputOptions, InputRecorder, KeyCapture, SourceOrigin};
use crate::project::{
    AudioTrack, EditList, MIC_AUDIO_FILE, MediaFiles, PROJECT_EXT, PROJECT_VERSION, PauseGap,
    Project, ProjectStatus, Rect, SCREEN_FILE, SYSTEM_AUDIO_FILE, Size, SourceDescriptor, Tracks,
    VideoTrack, WEBCAM_FILE,
};
use crate::screen::{ScreenCapture, ScreenConfig, ScreenTarget, StartTimings};
use crate::sources::{CaptureSource, resolve};
use crate::timeline::{Gap, Timeline};
use crate::webcam::WebcamRecorder;

/// Raw input sidecar (crash recovery).
pub const INPUT_SIDECAR: &str = "input.jsonl";
/// Session clock sidecar (crash recovery).
pub const SESSION_SIDECAR: &str = "session.json";

/// Options for a recording.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordOptions {
    /// What to record.
    pub source: CaptureSource,
    /// Max frame rate (default 60).
    pub fps: u32,
    /// Bake the system cursor into `screen.mp4` (default **false**: the editor
    /// renders its own cursor from the cursor track).
    pub cursor: bool,
    /// Record the default microphone to `audio-mic.wav`.
    pub mic: bool,
    /// Record system audio (loopback) to `audio-system.wav`.
    pub system_audio: bool,
    /// Keystroke capture mode (default off).
    pub keys: KeyCapture,
    /// Webcam: `None` = off, `Some("default")` = first camera, else a camera id.
    pub webcam: Option<String>,
    /// Store foreground window titles in the focus track (default off: opaque ids).
    pub window_titles: bool,
    /// Show the Windows capture border while recording (default **true**; hiding it
    /// needs Windows 11 and may be refused, which is reported as a warning).
    pub capture_border: bool,
}

impl Default for RecordOptions {
    fn default() -> Self {
        Self::new(CaptureSource::PrimaryMonitor)
    }
}

impl RecordOptions {
    /// Defaults for a source: 60 fps, no baked cursor, no audio, no keys, no webcam.
    pub fn new(source: CaptureSource) -> Self {
        Self {
            source,
            fps: 60,
            cursor: false,
            mic: false,
            system_audio: false,
            keys: KeyCapture::Off,
            webcam: None,
            window_titles: false,
            capture_border: true,
        }
    }
}

/// Outcome of [`RecordingSession::stop`].
#[derive(Clone, Debug)]
pub struct RecordingResult {
    /// Project folder.
    pub project_dir: PathBuf,
    /// Duration on the output timeline (equals the `screen.mp4` duration).
    pub duration_ms: u64,
    /// The project written to `project.json`.
    pub project: Project,
    /// Video frames delivered by WGC.
    pub frames: u64,
    /// Non-fatal problems.
    pub warnings: Vec<String>,
}

/// Something the UI should hear about while recording.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    /// Fatal problem: the caller must stop the session (`recording://error`).
    pub error: Option<ErrorPayload>,
    /// New non-fatal warnings since the last report (`recording://warning`).
    pub warnings: Vec<String>,
}

/// Recovery data kept next to the media while recording.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionSidecar {
    /// First video frame (hns, shared clock), once known.
    pub t0_hns: Option<i64>,
    /// Completed pauses (hns, shared clock).
    pub gaps: Vec<(i64, i64)>,
    /// Open pause start, if paused.
    pub paused_since: Option<i64>,
    /// Last time the session was alive (hns).
    pub heartbeat_hns: i64,
}

/// A recording in progress.
pub struct RecordingSession {
    timeline: Arc<Timeline>,
    screen: Option<ScreenCapture>,
    webcam: Option<WebcamRecorder>,
    mic: Option<AudioRecorder>,
    system: Option<AudioRecorder>,
    input: Option<InputRecorder>,
    warnings: Vec<String>,
    reported: ReportedFlags,
    project_dir: PathBuf,
    started: Instant,
    created_at_ms: u64,
    source: SourceDescriptor,
    canvas: Size,
    fps: u32,
    stopped: bool,
    cursor_baked: bool,
    timings: StartTimings,
    heartbeat: Option<Heartbeat>,
    /// Background rewrite of the provisional project.json (window sources); joined in
    /// `stop` so it can never overwrite the final project.
    canvas_save: Option<ProvisionalSave>,
}

#[derive(Default)]
struct ReportedFlags {
    warnings_sent: usize,
    mic: bool,
    system: bool,
    webcam: bool,
}

/// Make the process per-monitor DPI aware so cursor coordinates are physical
/// pixels. Harmless if already set (e.g. by the Tauri manifest).
pub fn ensure_dpi_awareness() {
    // SAFETY: plain call; failure (already set) is ignored.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Delete `.prewarm-<pid>.mp4` leftovers of other (killed) processes in `dir`.
fn remove_stale_prewarm_files(dir: &Path, own_pid: u32) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let pid = name
            .strip_prefix(".prewarm-")
            .and_then(|r| r.strip_suffix(".mp4"))
            .and_then(|p| p.parse::<u32>().ok());
        if pid.is_some_and(|p| p != own_pid) {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Load the D3D11, Media Foundation and H.264 encoder DLLs ahead of the first
/// recording (call once in the background at app start) so `start` is faster.
/// Writes and deletes a tiny file in `dir`. Returns the time taken (ms).
pub fn prewarm(dir: &Path) -> Result<u64> {
    let t = Instant::now();
    crate::com::ensure_mta();
    fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    remove_stale_prewarm_files(dir, std::process::id());
    let (device, _ctx) = crate::screen::shared_device()?;
    // Warm the encoder the way a real recording will use it: the primary monitor's size.
    let (w, h) = crate::sources::list_monitors()
        .ok()
        .and_then(|m| m.first().map(|m| (m.bounds.width, m.bounds.height)))
        .map_or((1920, 1080), |(w, h)| {
            (
                (w.max(2) as u32 & !1).min(8192),
                (h.max(2) as u32 & !1).min(8192),
            )
        });
    let path = dir.join(format!(".prewarm-{}.mp4", std::process::id()));
    let res = crate::mf::H264Writer::create(
        &path,
        crate::mf::WriterConfig::with_auto_bitrate(w, h, 60),
        crate::mf::WriterInput::D3D11Bgra(&device),
    );
    drop(res);
    let _ = fs::remove_file(&path);
    Ok(t.elapsed().as_millis() as u64)
}

/// Writes the provisional `project.json` on a background thread: its two fsyncs cost
/// ~0.7 s on a busy disk, which would otherwise sit on the start-latency critical path.
struct ProvisionalSave(std::thread::JoinHandle<Result<()>>);

impl ProvisionalSave {
    fn spawn(project: Project, dir: &Path) -> Self {
        let dir = dir.to_path_buf();
        Self(std::thread::spawn(move || Ok(project.save(&dir)?)))
    }

    fn wait(self) -> Result<()> {
        self.0
            .join()
            .unwrap_or_else(|_| Err(Error::Capture("project.json writer panicked".into())))
    }
}

/// Create a fresh, unique `Recording YYYY-MM-DD HH.MM.SS.frostcap` folder under `root`.
pub fn new_project_dir(root: &Path) -> Result<PathBuf> {
    fs::create_dir_all(root).map_err(|e| Error::io(root, e))?;
    // SAFETY: trivial.
    let t = unsafe { GetLocalTime() };
    let base = format!(
        "Recording {:04}-{:02}-{:02} {:02}.{:02}.{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    );
    for i in 1..10_000 {
        let name = if i == 1 {
            format!("{base}.{PROJECT_EXT}")
        } else {
            format!("{base} ({i}).{PROJECT_EXT}")
        };
        let dir = root.join(name);
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(Error::io(&dir, e)),
        }
    }
    Err(Error::Invalid(
        "could not create a unique project folder".into(),
    ))
}

/// `source.name` as stored in project.json: a window's title is private, so it
/// is replaced by a generic label unless the user opted into window titles.
fn source_name(kind: crate::project::SourceKind, name: &str, titles: bool) -> String {
    if kind == crate::project::SourceKind::Window && !titles {
        "Window".to_owned()
    } else {
        name.to_owned()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn folder_name(dir: &Path) -> String {
    dir.file_stem().map_or_else(
        || "Recording".to_owned(),
        |s| s.to_string_lossy().into_owned(),
    )
}

impl RecordingSession {
    /// Start recording into an (existing, empty) project folder.
    pub fn start(opts: RecordOptions, project_dir: &Path) -> Result<Self> {
        let t_start = Instant::now();
        ensure_dpi_awareness();
        fs::create_dir_all(project_dir).map_err(|e| Error::io(project_dir, e))?;
        if fs::read_dir(project_dir)
            .map_err(|e| Error::io(project_dir, e))?
            .next()
            .is_some()
        {
            return Err(Error::InvalidPath(format!(
                "{} is not empty",
                project_dir.display()
            )));
        }
        disk::ensure_free(project_dir, disk::MIN_FREE_TO_START)?;
        let resolved = resolve(&opts.source)?;
        let fps = opts.fps.clamp(1, 240);
        let origin = match (resolved.target, resolved.crop) {
            (ScreenTarget::Window(h), _) => SourceOrigin::Window(h),
            (ScreenTarget::Monitor(_), Some(c)) => SourceOrigin::Fixed(Rect {
                x: resolved.bounds.x + c.x,
                y: resolved.bounds.y + c.y,
                ..c
            }),
            (ScreenTarget::Monitor(_), None) => SourceOrigin::Fixed(resolved.bounds),
            // All displays: cursor coordinates relative to the union canvas origin.
            (ScreenTarget::AllDisplays, _) => SourceOrigin::Fixed(resolved.bounds),
        };
        let source = SourceDescriptor {
            kind: resolved.kind,
            name: source_name(resolved.kind, &resolved.name, opts.window_titles),
            bounds: resolved.bounds,
            crop: resolved.crop,
        };
        let created_at_ms = now_ms();
        let provisional_canvas = match resolved.crop {
            Some(c) => Size {
                width: c.width.max(2) as u32 & !1,
                height: c.height.max(2) as u32 & !1,
            },
            None => Size {
                width: resolved.bounds.width.max(2) as u32 & !1,
                height: resolved.bounds.height.max(2) as u32 & !1,
            },
        };
        let provisional = Project {
            version: PROJECT_VERSION,
            name: folder_name(project_dir),
            created_at_ms,
            source: source.clone(),
            canvas: provisional_canvas,
            fps,
            duration_ms: 0,
            media: MediaFiles {
                screen: SCREEN_FILE.into(),
                ..Default::default()
            },
            tracks: Tracks::default(),
            edits: EditList::default(),
            pauses: vec![],
            status: ProjectStatus::Recording,
            warnings: vec![],
            cursor_baked: opts.cursor,
        };
        let provisional_save = ProvisionalSave::spawn(provisional.clone(), project_dir);

        let timeline = Arc::new(Timeline::default());
        let mut warnings = Vec::new();

        // Side tracks first so they are running when the first frame anchors t0.
        // The screen pipeline (D3D device, WGC, encoder MFT: the slow part) starts in
        // parallel with the side tracks. Order does not matter for sync: every track
        // is aligned to the first video frame via the timeline (late audio is padded).
        let prep_ms = t_start.elapsed().as_millis() as u64;
        let screen_cfg = ScreenConfig {
            target: resolved.target,
            path: project_dir.join(SCREEN_FILE),
            crop: resolved.crop,
            size: None,
            fps,
            cursor: opts.cursor,
            border: opts.capture_border,
            monitors: resolved
                .all
                .as_ref()
                .map(|a| a.0.clone())
                .unwrap_or_default(),
            canvas: resolved.all.as_ref().map(|a| a.1.clone()),
        };
        let screen_starter = {
            let tl = timeline.clone();
            std::thread::Builder::new()
                .name("frostcap-screen-start".into())
                .spawn(move || ScreenCapture::start(screen_cfg, tl))
                .map_err(|e| Error::Capture(e.to_string()))?
        };
        let t_side = Instant::now();
        let mut open_audio = |enabled: bool, ep: AudioEndpoint, file: &str| {
            if !enabled {
                return None;
            }
            match AudioRecorder::start(ep, &project_dir.join(file), timeline.clone()) {
                Ok(r) => Some(r),
                Err(e) => {
                    warnings.push(format!("{ep:?} audio disabled: {e}"));
                    None
                }
            }
        };
        let mic = open_audio(opts.mic, AudioEndpoint::Microphone, MIC_AUDIO_FILE);
        let system = open_audio(
            opts.system_audio,
            AudioEndpoint::SystemLoopback,
            SYSTEM_AUDIO_FILE,
        );
        let webcam = match opts.webcam.as_deref() {
            None => None,
            Some(id) => {
                let id = (id != "default" && !id.is_empty()).then_some(id);
                match WebcamRecorder::start(
                    id,
                    &project_dir.join(WEBCAM_FILE),
                    timeline.clone(),
                    false,
                ) {
                    Ok(w) => Some(w),
                    Err(e) => {
                        warnings.push(format!("webcam disabled: {e}"));
                        None
                    }
                }
            }
        };
        let input = InputRecorder::start(
            origin,
            InputOptions {
                hz: 120,
                keys: opts.keys,
                sidecar: Some(project_dir.join(INPUT_SIDECAR)),
                titles: opts.window_titles,
                timeline: Some(timeline.clone()),
                scale: resolved.all.as_ref().map_or(1.0, |a| a.1.scale),
            },
        )
        .map_err(|e| Error::io(project_dir, e));
        let input = match input {
            Ok(i) => i,
            Err(e) => {
                if let Ok(Ok(mut s)) = screen_starter.join() {
                    let _ = s.stop();
                }
                drop((mic, system, webcam));
                let _ = provisional_save.wait();
                remove_session_files(project_dir);
                return Err(e);
            }
        };

        let side_tracks_ms = t_side.elapsed().as_millis() as u64;
        let t_wait = Instant::now();
        let screen = screen_starter
            .join()
            .unwrap_or_else(|_| Err(Error::Capture("screen start panicked".into())));
        let screen = match screen {
            Ok(s) => s,
            Err(e) => {
                // Everything else is dropped (threads stopped) on return.
                drop((mic, system, webcam, input));
                let _ = provisional_save.wait();
                remove_session_files(project_dir);
                return Err(e);
            }
        };
        let mut timings = screen.timings();
        timings.prep_ms = prep_ms;
        timings.side_tracks_ms = side_tracks_ms;
        timings.screen_wait_ms = t_wait.elapsed().as_millis() as u64;
        let canvas = Size {
            width: timings.width,
            height: timings.height,
        };
        if let Err(e) = provisional_save.wait() {
            drop((mic, system, webcam, input, screen));
            remove_session_files(project_dir);
            return Err(e);
        }
        // The encoder size is only known now (window sources): keep the
        // provisional project.json consistent for recovery.
        let canvas_save = (canvas != provisional.canvas).then(|| {
            let mut p = provisional;
            p.canvas = canvas;
            ProvisionalSave::spawn(p, project_dir)
        });

        let timeline_for_heartbeat = timeline.clone();
        let session = Self {
            timeline,
            screen: Some(screen),
            webcam,
            mic,
            system,
            input: Some(input),
            warnings,
            reported: ReportedFlags::default(),
            project_dir: project_dir.to_path_buf(),
            started: Instant::now(),
            created_at_ms,
            source,
            canvas,
            fps,
            stopped: false,
            cursor_baked: opts.cursor,
            timings,
            canvas_save,
            heartbeat: Some(Heartbeat::start(
                project_dir,
                timeline_for_heartbeat,
                std::time::Duration::from_secs(1),
            )),
        };
        session.write_session_sidecar();
        Ok(session)
    }

    /// Wall-clock time since the session started, excluding pauses.
    pub fn elapsed_ms(&self) -> u64 {
        self.timeline
            .position(clock::now_hns())
            .map_or(0, |p| (p / HNS_PER_MS) as u64)
    }

    /// Wall-clock time since start, including pauses.
    pub fn wall_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Project folder being written.
    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    /// Frames delivered so far.
    pub fn frames(&self) -> u64 {
        self.screen.as_ref().map_or(0, ScreenCapture::frames)
    }

    /// Encoder size locked at start.
    pub fn canvas(&self) -> Size {
        self.canvas
    }

    /// Startup diagnostics.
    pub fn timings(&self) -> StartTimings {
        self.timings
    }

    /// Whether the session is paused.
    pub fn is_paused(&self) -> bool {
        self.timeline.is_paused()
    }

    /// Pause (paused time is removed from every track). Returns false if already paused.
    pub fn pause(&mut self) -> bool {
        let r = self.timeline.pause(clock::now_hns());
        self.write_session_sidecar();
        r
    }

    /// Resume after [`Self::pause`]. Returns false if not paused.
    pub fn resume(&mut self) -> bool {
        let r = self.timeline.resume(clock::now_hns());
        self.write_session_sidecar();
        r
    }

    fn write_session_sidecar(&self) {
        write_session_sidecar(&self.project_dir, &self.timeline);
    }

    /// Poll for live problems (call ~every 100-500 ms). Also refreshes the
    /// recovery sidecar. A returned `error` means the caller must stop.
    pub fn health(&mut self) -> HealthReport {
        let mut error = None;
        if let Some(s) = &self.screen {
            if let Some(msg) = s.fault() {
                error = Some(ErrorPayload {
                    code: s.fault_code().unwrap_or(ErrorCode::Internal),
                    message: msg,
                });
            } else if s.source_closed() {
                error = Some(ErrorPayload {
                    code: ErrorCode::SourceGone,
                    message: "the recorded window was closed".into(),
                });
            }
        }
        if error.is_none()
            && let Some(free) = disk::free_bytes(&self.project_dir)
            && free < disk::MIN_FREE_WHILE_RECORDING
        {
            error = Some(ErrorPayload {
                code: ErrorCode::DiskFull,
                message: format!(
                    "only {} MB left on the recording drive; recording stopped",
                    free >> 20
                ),
            });
        }
        if !self.reported.mic && self.mic.as_ref().is_some_and(AudioRecorder::has_failed) {
            self.reported.mic = true;
            self.warnings
                .push("microphone capture stopped unexpectedly".into());
        }
        if !self.reported.system && self.system.as_ref().is_some_and(AudioRecorder::has_failed) {
            self.reported.system = true;
            self.warnings
                .push("system audio capture stopped unexpectedly".into());
        }
        if !self.reported.webcam && self.webcam.as_ref().is_some_and(WebcamRecorder::has_failed) {
            self.reported.webcam = true;
            self.warnings
                .push("webcam capture stopped unexpectedly".into());
        }
        let warnings = self.warnings[self.reported.warnings_sent..].to_vec();
        self.reported.warnings_sent = self.warnings.len();
        HealthReport { error, warnings }
    }

    /// Stop everything, finalize files and write `project.json`. A second call
    /// returns an error without side effects. Partial media is kept on failure.
    pub fn stop(&mut self) -> Result<RecordingResult> {
        if self.stopped {
            return Err(Error::Invalid("recording already stopped".into()));
        }
        self.stopped = true;
        // Every track stops at the same instant.
        self.timeline.end(clock::now_hns());
        drop(self.heartbeat.take());
        let mut warnings = std::mem::take(&mut self.warnings);

        // 1) Screen: hold frame + finalize (stops WGC first).
        let screen = self.screen.take().map(|mut s| s.stop());
        // 2) Side tracks.
        let webcam = self.webcam.take().map(|mut w| w.stop());
        let mut finish_audio =
            |rec: Option<AudioRecorder>, what: &str| -> Option<AudioCaptureInfo> {
                match rec?.stop() {
                    Ok(i) if i.frames > 0 => {
                        if let Some(e) = &i.error {
                            warnings.push(format!(
                                "{what} stopped early (kept {} ms): {}",
                                i.duration_ms(),
                                e.message
                            ));
                        }
                        warnings.extend(audio_warnings(what, &i));
                        Some(i)
                    }
                    Ok(_) => {
                        warnings.push(format!("{what}: no audio was captured"));
                        None
                    }
                    Err(e) => {
                        warnings.push(format!("{what}: {e}"));
                        None
                    }
                }
            };
        let mic = finish_audio(self.mic.take(), "microphone");
        let system = finish_audio(self.system.take(), "system audio");
        let raw_input = self.input.take().map(|mut i| i.stop()).unwrap_or_default();
        if raw_input.failed {
            warnings.push(
                "input tracking stopped unexpectedly; cursor/click tracks are missing".into(),
            );
        }

        let (frames, duration_hns, screen_err) = match screen {
            Some(Ok(r)) => {
                warnings.extend(r.warnings);
                (r.frames, r.duration_hns, None)
            }
            Some(Err(e)) => {
                warnings.push(format!("screen recording failed: {e}"));
                (0, 0, Some(e))
            }
            None => (
                0,
                0,
                Some(Error::Capture("screen capture was not running".into())),
            ),
        };
        let duration_ms = (duration_hns.max(0) / HNS_PER_MS) as u64;

        let (t0, gaps) = self.timeline.snapshot().unwrap_or((i64::MAX, vec![]));
        let closed_gaps: Vec<Gap> = gaps.iter().copied().filter(|g| g.end != i64::MAX).collect();
        let mut tracks = raw_input.into_tracks(t0, &closed_gaps);
        clamp_tracks(&mut tracks, duration_ms);

        let pauses = pause_gaps(t0, &closed_gaps);
        let audio_track = |i: AudioCaptureInfo, file: &str| AudioTrack {
            file: file.to_owned(),
            offset_ms: i.start_pos_hns / HNS_PER_MS,
            sample_rate: i.sample_rate,
            channels: i.channels,
            duration_ms: i.duration_ms(),
        };
        let webcam_video = match webcam {
            Some(Ok(w)) if w.frames > 0 => {
                let duration_ms = (w.duration_hns.max(0) / HNS_PER_MS) as u64;
                if let Some(e) = &w.error {
                    warnings.push(format!(
                        "webcam stopped early (kept {duration_ms} ms): {}",
                        e.message
                    ));
                }
                Some(VideoTrack {
                    file: WEBCAM_FILE.into(),
                    name: w.name,
                    width: w.width,
                    height: w.height,
                    offset_ms: w.first_hns / HNS_PER_MS,
                    duration_ms,
                })
            }
            Some(Ok(_)) => {
                warnings.push("webcam delivered no frames".into());
                let _ = fs::remove_file(self.project_dir.join(WEBCAM_FILE));
                None
            }
            Some(Err(e)) => {
                warnings.push(format!("webcam: {e}"));
                None
            }
            None => None,
        };
        if let Some(j) = self.canvas_save.take() {
            let _ = j.wait();
        }
        warnings.truncate(crate::project::limits::MAX_EDITS);
        let project = Project {
            version: PROJECT_VERSION,
            name: folder_name(&self.project_dir),
            created_at_ms: self.created_at_ms,
            source: self.source.clone(),
            canvas: self.canvas,
            fps: self.fps,
            duration_ms,
            media: MediaFiles {
                screen: SCREEN_FILE.to_owned(),
                webcam: webcam_video.as_ref().map(|w| w.file.clone()),
                webcam_video,
                mic_audio: mic.map(|i| audio_track(i, MIC_AUDIO_FILE)),
                system_audio: system.map(|i| audio_track(i, SYSTEM_AUDIO_FILE)),
            },
            tracks,
            edits: EditList::default(),
            pauses,
            status: ProjectStatus::Complete,
            warnings: warnings.clone(),
            cursor_baked: self.cursor_baked,
        };
        project.save(&self.project_dir)?;
        let _ = fs::remove_file(self.project_dir.join(INPUT_SIDECAR));
        let _ = fs::remove_file(self.project_dir.join(SESSION_SIDECAR));
        if frames == 0 {
            return Err(screen_err
                .unwrap_or_else(|| Error::Capture("no video frames were captured".into())));
        }
        Ok(RecordingResult {
            project_dir: self.project_dir.clone(),
            duration_ms,
            project,
            frames,
            warnings,
        })
    }
}

impl Drop for RecordingSession {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.stop();
        }
    }
}

/// Warnings for audio-device faults that were handled by padding or dropping.
fn audio_warnings(what: &str, i: &AudioCaptureInfo) -> Vec<String> {
    let mut w = Vec::new();
    if i.glitch_packets > 0 {
        w.push(format!(
            "{what}: the device reported {} glitchy packets (gaps were filled with silence)",
            i.glitch_packets
        ));
    }
    if i.unfilled_frames > 0 {
        w.push(format!(
            "{what}: {:.1} s of audio is missing (device stalled; the gap was too large to fill)",
            i.unfilled_frames as f64 / f64::from(i.sample_rate.max(1))
        ));
    }
    w
}

/// Write `session.json` (t0, pauses, heartbeat) for crash recovery.
fn write_session_sidecar(dir: &Path, timeline: &Timeline) {
    let (t0, gaps, paused) = match timeline.snapshot() {
        Some((t0, gaps)) => {
            let open = gaps.iter().find(|g| g.end == i64::MAX).map(|g| g.start);
            let done = gaps
                .iter()
                .filter(|g| g.end != i64::MAX)
                .map(|g| (g.start, g.end))
                .collect();
            (Some(t0), done, open)
        }
        None => (None, vec![], None),
    };
    let s = SessionSidecar {
        t0_hns: t0,
        gaps,
        paused_since: paused,
        heartbeat_hns: clock::now_hns(),
    };
    if let Ok(bytes) = serde_json::to_vec(&s) {
        let _ = crate::project::write_atomic(&dir.join(SESSION_SIDECAR), &bytes);
    }
}

/// Background thread refreshing `session.json` every `period`, so crash
/// recovery works without the caller polling [`RecordingSession::health`].
pub(crate) struct Heartbeat {
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Heartbeat {
    pub(crate) fn start(dir: &Path, timeline: Arc<Timeline>, period: std::time::Duration) -> Self {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dir = dir.to_path_buf();
        let handle = std::thread::Builder::new()
            .name("frostcap-heartbeat".into())
            .spawn({
                let stop = stop.clone();
                move || {
                    let step = std::time::Duration::from_millis(20).min(period);
                    while !stop.load(std::sync::atomic::Ordering::Acquire) {
                        write_session_sidecar(&dir, &timeline);
                        let t = Instant::now();
                        while t.elapsed() < period
                            && !stop.load(std::sync::atomic::Ordering::Acquire)
                        {
                            std::thread::sleep(step);
                        }
                    }
                }
            })
            .ok();
        Self { stop, handle }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Files a session may create (removed on a failed start; nothing else is touched).
const SESSION_FILES: [&str; 8] = [
    crate::project::PROJECT_FILE,
    crate::project::META_FILE,
    SCREEN_FILE,
    WEBCAM_FILE,
    MIC_AUDIO_FILE,
    SYSTEM_AUDIO_FILE,
    INPUT_SIDECAR,
    SESSION_SIDECAR,
];

/// Remove the files a session created, then the folder only if it is empty.
pub fn remove_session_files(dir: &Path) {
    for f in SESSION_FILES {
        let _ = fs::remove_file(dir.join(f));
    }
    let _ = fs::remove_dir(dir); // fails (and keeps the folder) if anything else is in it
}

/// Drop events after the end of the video.
pub(crate) fn clamp_tracks(t: &mut Tracks, duration_ms: u64) {
    t.cursor.retain(|s| s.t_ms <= duration_ms);
    t.clicks.retain(|s| s.t_ms <= duration_ms);
    t.keys.retain(|s| s.t_ms <= duration_ms);
    t.focus.retain(|s| s.t_ms <= duration_ms);
}

/// Convert shared-clock gaps to output-timeline pause markers.
pub(crate) fn pause_gaps(t0: i64, gaps: &[Gap]) -> Vec<PauseGap> {
    let mut removed = 0i64;
    gaps.iter()
        .filter(|g| g.end > t0)
        .map(|g| {
            let start = g.start.max(t0);
            let at = start - t0 - removed;
            removed += g.end - start;
            PauseGap {
                at_ms: (at.max(0) / HNS_PER_MS) as u64,
                duration_ms: ((g.end - start) / HNS_PER_MS) as u64,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::CursorSample;

    #[test]
    fn window_titles_are_not_stored_as_source_name_unless_opted_in() {
        use crate::project::SourceKind;
        assert_eq!(
            source_name(SourceKind::Window, "Bank - Secret doc", false),
            "Window"
        );
        assert_eq!(source_name(SourceKind::Window, "Notepad", true), "Notepad");
        // Monitor names are hardware labels, not titles.
        assert_eq!(
            source_name(SourceKind::Monitor, "DELL U2720Q", false),
            "DELL U2720Q"
        );
        assert_eq!(
            source_name(SourceKind::Region, "Display 1", false),
            "Display 1"
        );
    }

    #[test]
    fn pause_markers_on_output_timeline() {
        let ms = HNS_PER_MS;
        let gaps = [
            Gap {
                start: 1_000 * ms,
                end: 3_000 * ms,
            },
            Gap {
                start: 5_000 * ms,
                end: 5_500 * ms,
            },
        ];
        let p = pause_gaps(0, &gaps);
        assert_eq!(
            p,
            vec![
                PauseGap {
                    at_ms: 1_000,
                    duration_ms: 2_000
                },
                PauseGap {
                    at_ms: 3_000,
                    duration_ms: 500
                }
            ]
        );
    }

    #[test]
    #[ignore = "needs interactive desktop"]
    fn prewarm_leaves_no_files() {
        let dir = crate::test_tmp_dir().join(format!("prewarm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let ms = prewarm(&dir).unwrap();
        assert!(ms < 30_000);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn heartbeat_records_t0_and_pauses_without_caller_help() {
        let dir = crate::test_tmp_dir().join(format!("heartbeat-{}.frostcap", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let tl = Arc::new(Timeline::default());
        let hb = Heartbeat::start(&dir, tl.clone(), std::time::Duration::from_millis(50));
        tl.anchor(1_000);
        tl.pause(2_000);
        std::thread::sleep(std::time::Duration::from_millis(200));
        drop(hb); // stop writing before reading (Windows rename vs. open race)
        let s: SessionSidecar =
            serde_json::from_slice(&fs::read(dir.join(SESSION_SIDECAR)).unwrap()).unwrap();
        assert_eq!(s.t0_hns, Some(1_000));
        assert_eq!(s.paused_since, Some(2_000));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn start_refuses_a_non_empty_folder_and_leaves_it_alone() {
        let dir = crate::test_tmp_dir().join(format!("nonempty-{}.frostcap", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("keep.txt"), b"user data").unwrap();
        let err = RecordingSession::start(RecordOptions::default(), &dir)
            .err()
            .expect("must fail");
        assert!(matches!(err, Error::InvalidPath(_)), "{err}");
        assert_eq!(fs::read(dir.join("keep.txt")).unwrap(), b"user data");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "nothing written");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_removes_only_our_files() {
        let dir = crate::test_tmp_dir().join(format!("cleanup-{}.frostcap", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for f in [
            crate::project::PROJECT_FILE,
            crate::project::META_FILE,
            SCREEN_FILE,
            INPUT_SIDECAR,
        ] {
            fs::write(dir.join(f), b"x").unwrap();
        }
        remove_session_files(&dir);
        assert!(!dir.exists(), "empty folder we created is removed");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(SCREEN_FILE), b"x").unwrap();
        fs::write(dir.join("other.txt"), b"x").unwrap();
        remove_session_files(&dir);
        assert!(dir.join("other.txt").exists(), "foreign files survive");
        assert!(!dir.join(SCREEN_FILE).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tracks_clamped_to_duration() {
        let mut t = Tracks {
            cursor: vec![
                CursorSample {
                    t_ms: 10,
                    x: 0,
                    y: 0,
                },
                CursorSample {
                    t_ms: 5_000,
                    x: 0,
                    y: 0,
                },
            ],
            ..Default::default()
        };
        clamp_tracks(&mut t, 3_000);
        assert_eq!(t.cursor.len(), 1);
    }
}

#[cfg(test)]
mod option_tests {
    use super::*;

    #[test]
    fn capture_border_defaults_on_and_deserializes_when_absent() {
        assert!(RecordOptions::default().capture_border);
        let o: RecordOptions = serde_json::from_str(r#"{"fps":30}"#).unwrap();
        assert!(o.capture_border);
        let o: RecordOptions = serde_json::from_str(r#"{"captureBorder":false}"#).unwrap();
        assert!(!o.capture_border);
    }
}

#[cfg(test)]
mod provisional_tests {
    use super::*;

    #[test]
    fn provisional_save_runs_in_the_background_and_lands_on_disk() {
        let dir = crate::test_tmp_dir().join(format!("prov-{}.frostcap", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut p = crate::project::samples::fixture();
        p.status = ProjectStatus::Recording;
        let job = ProvisionalSave::spawn(p.clone(), &dir);
        job.wait().unwrap();
        let back = Project::load(&dir).unwrap();
        assert_eq!(back.status, ProjectStatus::Recording);
        // A failing save (invalid project) is reported by wait(), not swallowed.
        p.fps = 0;
        assert!(ProvisionalSave::spawn(p, &dir).wait().is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod audio_warning_tests {
    use super::*;

    fn info() -> AudioCaptureInfo {
        AudioCaptureInfo {
            path: PathBuf::from("a.wav"),
            start_pos_hns: 0,
            sample_rate: 48_000,
            channels: 2,
            frames: 48_000,
            padded_frames: 0,
            dropped_frames: 0,
            unfilled_frames: 0,
            glitch_packets: 0,
            error: None,
        }
    }

    #[test]
    fn clean_audio_has_no_warnings_and_faults_are_explained() {
        assert!(audio_warnings("microphone", &info()).is_empty());
        let mut i = info();
        i.glitch_packets = 3;
        i.unfilled_frames = 96_000;
        let w = audio_warnings("system audio", &i);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].contains("system audio") && w[0].contains('3'));
        assert!(w[1].contains("2.0 s"), "{w:?}");
    }
}

#[cfg(test)]
mod prewarm_tests {
    use super::*;

    #[test]
    fn stale_prewarm_files_of_other_processes_are_removed_and_nothing_else() {
        let dir = crate::test_tmp_dir().join(format!("prewarm-stale-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for f in [
            ".prewarm-111.mp4",
            ".prewarm-222.mp4",
            ".prewarm-999.mp4",
            "Recording.mp4",
            ".prewarm-x.txt",
        ] {
            fs::write(dir.join(f), b"").unwrap();
        }
        remove_stale_prewarm_files(&dir, 999);
        let mut left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [".prewarm-999.mp4", ".prewarm-x.txt", "Recording.mp4"]
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
