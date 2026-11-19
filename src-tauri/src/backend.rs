//! `TauriBackend`: the command surface exposed to the UI.
//!
//! Thin adapters only (argument parsing, path policy, app paths, events, state);
//! the real work is in `frostcap-core`. Every command returns
//! `Result<T, { code, message }>`. See `docs/CONTRACT.md` for the full contract
//! and `fixtures/` for golden JSON.
//!
//! Events:
//! * `recording://tick`    `{ elapsedMs, paused }` every 100 ms while recording
//! * `recording://warning` `{ code: "warning", message }`
//! * `recording://error`   `{ code, message }` (the session is then auto-stopped)
//! * `recording://stopped` `{ result?, error? }` after an automatic stop
//! * `export://progress`   `{ jobId, path, outPath, progress, stage }`

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use frostcap_core::affinity::{self, ExcludeGuard};
use frostcap_core::autozoom::{self, ZoomPreset};
use frostcap_core::error::Error;
use frostcap_core::hotkeys::{Bindings, HotkeyAction, Hotkeys};
use frostcap_core::input::KeyCapture;
use frostcap_core::project::{Project, ZoomSegment};
use frostcap_core::recent::{self, RecentProject};
use frostcap_core::recorder::{self, RecordOptions, RecordingSession};
use frostcap_core::region::{self, RegionSelection};
use frostcap_core::settings::CaptureSettings;
use frostcap_core::sources::{self, SourceList};
use frostcap_core::webcam::{self, CameraInfo};
use frostcap_core::{dialogs, recovery};
use frostcap_export::{self as export, ExportRequest};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::dto::{
    BatchResponse, CmdResult, CommandError, ExportProgress, ExportResponse, HotkeyChord,
    HotkeyEvent, HotkeyInfo, ImportBackgroundResponse, LaunchOptions, RecordingStateResponse,
    SourceArg, StartResponse, StopResponse, StoppedEvent, SuggestCutsResponse, Tick, WarningEvent,
};
use crate::paths::PathPolicy;

struct Active {
    session: Arc<Mutex<RecordingSession>>,
    monitor_stop: Arc<AtomicBool>,
    project_dir: PathBuf,
    /// Keeps Frostcap's own windows out of the capture until the recording ends.
    _own_windows_hidden: ExcludeGuard,
}

/// Recording slot: a start in progress blocks other starts without holding the
/// lock for the (seconds-long) `RecordingSession::start`.
#[derive(Default)]
enum Slot {
    #[default]
    Idle,
    Starting,
    Recording(Active),
}

impl Slot {
    fn begin_start(&mut self) -> CmdResult<()> {
        match self {
            Slot::Idle => {
                *self = Slot::Starting;
                Ok(())
            }
            Slot::Starting => Err(CommandError::internal("a recording is starting")),
            Slot::Recording(_) => Err(CommandError::internal("a recording is already in progress")),
        }
    }
    fn active(&self) -> Option<&Active> {
        match self {
            Slot::Recording(a) => Some(a),
            _ => None,
        }
    }
    fn take_active(&mut self) -> Option<Active> {
        match std::mem::take(self) {
            Slot::Recording(a) => Some(a),
            other => {
                *self = other;
                None
            }
        }
    }
}

/// Error if `dir` is the folder of the running recording.
fn ensure_not_live(live: Option<&Path>, dir: &Path) -> CmdResult<()> {
    if live == Some(dir) {
        return Err(CommandError::internal("this project is still recording"));
    }
    Ok(())
}

/// Managed app state.
#[derive(Default)]
pub struct AppState {
    active: Mutex<Slot>,
    last_stop: Mutex<Option<StopResponse>>,
    policy: OnceLock<PathPolicy>,
    exports: Mutex<HashMap<String, Arc<AtomicBool>>>,
    jobs: AtomicU64,
    hotkeys: Mutex<Option<(Hotkeys, Vec<String>, Bindings)>>,
    settings: Mutex<CaptureSettings>,
    /// Started with `--selftest` (decided once at launch; nothing else enables it).
    selftest: AtomicBool,
    /// Project folders created by this run while the self-test is on (the only ones it may delete).
    selftest_projects: Mutex<std::collections::HashSet<PathBuf>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn projects_root(app: &AppHandle) -> CmdResult<PathBuf> {
    Ok(app
        .path()
        .video_dir()
        .map_err(|e| CommandError::internal(e.to_string()))?
        .join("Frostcap"))
}

fn recent_store(app: &AppHandle) -> CmdResult<PathBuf> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| CommandError::internal(e.to_string()))?
        .join("recent.json"))
}

impl AppState {
    /// Initialize the path policy (called from `setup`).
    pub fn init(&self, app: &AppHandle) -> CmdResult<()> {
        let pol = PathPolicy::new(&projects_root(app)?)?;
        if let Ok(store) = recent_store(app) {
            pol.allow_recent(
                recent::list_recent(&store, None, None)
                    .into_iter()
                    .map(|r| r.path),
            );
        }
        let _ = self.policy.set(pol);
        self.selftest.store(
            crate::paths::selftest_requested(std::env::args_os()),
            Ordering::Relaxed,
        );
        if let Ok(p) = settings_store(app) {
            *lock(&self.settings) = CaptureSettings::load(&p);
        }
        let bindings = hotkeys_store(app)
            .ok()
            .and_then(|p| std::fs::read(p).ok())
            .filter(|b| b.len() < 4096)
            .and_then(|b| serde_json::from_slice::<Bindings>(&b).ok())
            .filter(|b| b.validate().is_ok())
            .unwrap_or_default();
        if let Err(e) = register_hotkeys(app, self, bindings) {
            eprintln!("frostcap: hotkeys unavailable: {}", e.message);
        }
        Ok(())
    }

    fn policy(&self) -> CmdResult<&PathPolicy> {
        self.policy
            .get()
            .ok_or_else(|| CommandError::internal("path policy not initialized"))
    }

    fn live_dir(&self) -> Option<PathBuf> {
        lock(&self.active).active().map(|a| a.project_dir.clone())
    }
}

/// Basenames of the background images a project references (`style.backgroundImage`
/// and the webcam background image).
fn background_files(p: &Project) -> Vec<String> {
    let s = &p.edits.style;
    s.background_image
        .iter()
        .map(|b| b.file.clone())
        .chain(s.webcam.background.image.clone())
        .collect()
}

/// Let the webview load exactly the media files of this project (never
/// project.json or the recovery sidecars).
fn allow_project_assets(app: &AppHandle, dir: &Path) {
    use frostcap_core::project::{MIC_AUDIO_FILE, SCREEN_FILE, SYSTEM_AUDIO_FILE, WEBCAM_FILE};
    let scope = app.asset_protocol_scope();
    for f in [SCREEN_FILE, WEBCAM_FILE, MIC_AUDIO_FILE, SYSTEM_AUDIO_FILE] {
        let p = dir.join(f);
        if p.is_file() {
            let _ = scope.allow_file(&p);
        }
    }
    // Imported backgrounds: exact files only, resolved through the validating helper so
    // a hostile project cannot name another path. Re-granted on every load (the scope
    // is per session).
    if let Ok(project) = Project::load(dir) {
        for file in background_files(&project) {
            let b = frostcap_core::project::BackgroundImage {
                file,
                fit: Default::default(),
                blur: 0.0,
            };
            if let Ok(p) = frostcap_core::project::background_path(dir, &b) {
                let _ = scope.allow_file(&p);
            }
        }
    }
}

/// Window handle of a window source (the one window that must stay capturable).
fn recorded_hwnd(s: &frostcap_core::sources::CaptureSource) -> Option<isize> {
    match s {
        frostcap_core::sources::CaptureSource::Window { hwnd, .. } => Some(*hwnd as isize),
        _ => None,
    }
}

fn owner_hwnd(app: &AppHandle) -> Option<isize> {
    app.get_webview_window("main")
        .and_then(|w| w.hwnd().ok())
        .map(|h| h.0 as isize)
}

fn remember_recent(app: &AppHandle, dir: &Path) {
    if let Ok(store) = recent_store(app) {
        let _ = recent::add_recent(&store, dir);
    }
}

/// Global hotkey handler: stop / pause / resume act directly; start is left to
/// the UI (it owns the source and settings). Always emits `hotkey://pressed`.
fn on_hotkey(app: &AppHandle, action: HotkeyAction) {
    // Never block the hotkey message thread: stopping finalizes files.
    let app = app.clone();
    thread::spawn(move || {
        let r =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hotkey_action(&app, action)));
        if r.is_err() {
            let _ = app.emit(
                "recording://warning",
                WarningEvent {
                    code: "warning".into(),
                    message: "hotkey handler failed".into(),
                },
            );
        }
    });
}

fn main_window_visible(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .is_some_and(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(true))
}

fn hotkey_action(app: &AppHandle, action: HotkeyAction) {
    let state = app.state::<AppState>();
    let handled = match action {
        HotkeyAction::StartStop => match stop_active(app, &state) {
            Ok(Some(result)) => {
                let _ = app.emit(
                    "recording://stopped",
                    StoppedEvent {
                        result: Some(result),
                        error: None,
                    },
                );
                true
            }
            // Idle: never start silently. Only a visible app is told, and the UI
            // must run its visible countdown before calling start_recording.
            Ok(None) if !main_window_visible(app) => return,
            Ok(None) => false,
            Err(e) => {
                let _ = app.emit("recording://error", e);
                true
            }
        },
        HotkeyAction::PauseResume => {
            let active = lock(&state.active);
            match active.active() {
                Some(a) => {
                    let mut s = lock(&a.session);
                    if s.is_paused() {
                        s.resume();
                    } else {
                        s.pause();
                    }
                    true
                }
                None => false,
            }
        }
    };
    let _ = app.emit("hotkey://pressed", HotkeyEvent { action, handled });
}

fn settings_store(app: &AppHandle) -> CmdResult<PathBuf> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| CommandError::internal(e.to_string()))?
        .join("capture-settings.json"))
}

fn hotkeys_store(app: &AppHandle) -> CmdResult<PathBuf> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| CommandError::internal(e.to_string()))?
        .join("hotkeys.json"))
}

/// (Re)register `bindings`, replacing any previous registration.
fn register_hotkeys(app: &AppHandle, state: &AppState, bindings: Bindings) -> CmdResult<()> {
    let mut slot = lock(&state.hotkeys);
    *slot = None; // unregister the old chords first
    let handle = app.clone();
    let (hk, failed) = Hotkeys::start(&bindings, move |action| on_hotkey(&handle, action))?;
    *slot = Some((hk, failed, bindings));
    Ok(())
}

fn hotkey_info(state: &AppState) -> HotkeyInfo {
    let slot = lock(&state.hotkeys);
    let (failed, bindings) = slot
        .as_ref()
        .map_or((None, Bindings::default()), |s| (Some(s.1.clone()), s.2));
    HotkeyInfo {
        chords: [HotkeyAction::StartStop, HotkeyAction::PauseResume]
            .into_iter()
            .map(|a| {
                let chord = bindings.chord(a).to_string();
                let registered = failed.as_ref().is_some_and(|f| !f.contains(&chord));
                HotkeyChord {
                    action: a,
                    chord,
                    registered,
                }
            })
            .collect(),
    }
}

/// Registered global hotkeys (and whether each could be registered).
#[tauri::command(async)]
pub fn hotkeys(state: State<'_, AppState>) -> CmdResult<HotkeyInfo> {
    Ok(hotkey_info(&state))
}

/// Change the global hotkeys (`{ startStop, pauseResume }` chord strings such as
/// `"Ctrl+Alt+Shift+R"`; Ctrl or Win required). Persisted in the app data folder.
#[tauri::command(async)]
pub fn set_hotkeys(
    app: AppHandle,
    state: State<'_, AppState>,
    bindings: Bindings,
) -> CmdResult<HotkeyInfo> {
    bindings.validate()?;
    register_hotkeys(&app, &state, bindings)?;
    let store = hotkeys_store(&app)?;
    if let Some(p) = store.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    frostcap_core::project::write_atomic(
        &store,
        &serde_json::to_vec(&bindings).map_err(|e| CommandError::internal(e.to_string()))?,
    )?;
    Ok(hotkey_info(&state))
}
/// Current capture settings `{ captureBorder, excludeOwnWindows }` (both default true).
#[tauri::command(async)]
pub fn capture_settings(state: State<'_, AppState>) -> CmdResult<CaptureSettings> {
    Ok(*lock(&state.settings))
}

fn update_settings(
    app: &AppHandle,
    state: &AppState,
    change: impl FnOnce(&mut CaptureSettings),
) -> CmdResult<CaptureSettings> {
    let mut s = lock(&state.settings);
    let mut next = *s;
    change(&mut next);
    next.save(&settings_store(app)?)?;
    *s = next;
    Ok(next)
}

/// Yes/No decision, behind a trait so it is testable.
trait Confirm {
    fn confirm(&self, title: &str, text: &str) -> bool;
}

/// The real native message box.
struct NativeConfirm(Option<isize>);

impl Confirm for NativeConfirm {
    fn confirm(&self, title: &str, text: &str) -> bool {
        dialogs::confirm(self.0, title, text)
    }
}

/// Hiding the OS capture border (the recording indicator) must be a deliberate, native
/// decision: turning it off asks first; declining is a `cancelled` error. Turning it
/// on or leaving it unchanged never asks.
fn border_change_allowed(current: bool, next: bool, c: &dyn Confirm) -> CmdResult<()> {
    if current && !next
        && !c.confirm(
            "Hide the recording border?",
            "Windows draws a yellow border around what Frostcap records so you and others can see that a screen is being captured.

Hide it for future recordings?",
        )
    {
        return Err(CommandError {
            code: frostcap_core::ErrorCode::Cancelled,
            message: "the capture border stays on".into(),
        });
    }
    Ok(())
}

/// Show (`true`, default) or hide the Windows capture border for the next recording. Persisted.
#[tauri::command(async)]
pub fn set_capture_border(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> CmdResult<CaptureSettings> {
    let current = lock(&state.settings).capture_border;
    border_change_allowed(current, enabled, &NativeConfirm(owner_hwnd(&app)))?;
    update_settings(&app, &state, |s| s.capture_border = enabled)
}

/// Keep Frostcap's own windows out of recordings (`true`, default). Persisted.
#[tauri::command(async)]
pub fn set_exclude_own_windows(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> CmdResult<CaptureSettings> {
    update_settings(&app, &state, |s| s.exclude_own_windows = enabled)
}

/// `{ selftest }`: true only when the exe was started with `--selftest`.
#[tauri::command(async)]
pub fn launch_options(state: State<'_, AppState>) -> CmdResult<LaunchOptions> {
    Ok(LaunchOptions {
        selftest: state.selftest.load(Ordering::Relaxed),
    })
}

fn require_selftest(state: &AppState) -> CmdResult<()> {
    if state.selftest.load(Ordering::Relaxed) {
        Ok(())
    } else {
        Err(Error::Invalid("only available when started with --selftest".into()).into())
    }
}

/// Self-test only: write the JSON report to `<Videos>\Frostcap\selftest-report.json` (<= 1 MB).
#[tauri::command(async)]
pub fn write_selftest_report(state: State<'_, AppState>, report: String) -> CmdResult<()> {
    require_selftest(&state)?;
    let bytes = crate::paths::report_bytes(&report)?;
    let file = state.policy()?.root().join("selftest-report.json");
    Ok(frostcap_core::project::write_atomic(&file, &bytes)?)
}

/// Self-test only: delete the project folders this run created and its
/// `frostcap-selftest-*.mp4` exports (all paths validated first; anything else is refused).
#[tauri::command(async)]
pub fn selftest_cleanup(state: State<'_, AppState>, paths: Vec<PathBuf>) -> CmdResult<()> {
    require_selftest(&state)?;
    let created = lock(&state.selftest_projects).clone();
    let pol = state.policy()?;
    let targets = paths
        .iter()
        .map(|p| pol.selftest_target(p, &created))
        .collect::<Result<Vec<_>, _>>()?;
    for (p, kind) in targets {
        match kind {
            crate::paths::Deletable::Project => std::fs::remove_dir_all(&p),
            crate::paths::Deletable::Export => std::fs::remove_file(&p),
        }
        .map_err(|e| Error::io(&p, e))?;
    }
    Ok(())
}

fn strip_verbatim(p: PathBuf) -> PathBuf {
    match p.to_string_lossy().strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p,
    }
}

/// Absolute, expanded default output folder (`Videos\Frostcap`, known-folder API).
#[tauri::command(async)]
pub fn default_output_dir(state: State<'_, AppState>) -> CmdResult<PathBuf> {
    Ok(state.policy()?.root().to_path_buf())
}

// ------------------------------------------------------------------ sources

/// Monitors, windows and the flat UI list (with PNG thumbnails unless `thumbnails: false`).
#[tauri::command(async)]
pub fn list_sources(thumbnails: Option<bool>) -> CmdResult<SourceList> {
    Ok(sources::list_sources(thumbnails.unwrap_or(true))?)
}

/// Video capture devices (empty = no camera).
#[tauri::command(async)]
pub fn list_cameras() -> CmdResult<Vec<CameraInfo>> {
    Ok(webcam::list_cameras()?)
}

/// Interactive region picker overlay. `null` when cancelled (Esc / right-click).
#[tauri::command(async)]
pub fn pick_region(state: State<'_, AppState>) -> CmdResult<Option<RegionSelection>> {
    let exclude = lock(&state.settings).exclude_own_windows;
    Ok(region::pick_region_with(exclude)?)
}

// ---------------------------------------------------------------- recording

/// Start recording. Unspecified options use core defaults (cursor not baked,
/// no audio, no keys, no webcam).
#[allow(clippy::too_many_arguments)]
#[tauri::command(async)]
pub fn start_recording(
    app: AppHandle,
    state: State<'_, AppState>,
    source: SourceArg,
    fps: Option<u32>,
    cursor: Option<bool>,
    mic: Option<bool>,
    system_audio: Option<bool>,
    keys: Option<KeyCapture>,
    webcam: Option<String>,
    window_titles: Option<bool>,
) -> CmdResult<StartResponse> {
    lock(&state.active).begin_start()?;
    let started = start_inner(
        &app,
        &state,
        source,
        fps,
        cursor,
        mic,
        system_audio,
        keys,
        webcam,
        window_titles,
    );
    let mut slot = lock(&state.active);
    match started {
        Ok((active, resp)) => {
            *slot = Slot::Recording(active);
            Ok(resp)
        }
        Err(e) => {
            *slot = Slot::Idle;
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn start_inner(
    app: &AppHandle,
    state: &AppState,
    source: SourceArg,
    fps: Option<u32>,
    cursor: Option<bool>,
    mic: Option<bool>,
    system_audio: Option<bool>,
    keys: Option<KeyCapture>,
    webcam: Option<String>,
    window_titles: Option<bool>,
) -> CmdResult<(Active, StartResponse)> {
    let mut opts = RecordOptions::new(source.into_source()?);
    opts.fps = fps.unwrap_or(opts.fps).clamp(1, 240);
    opts.cursor = cursor.unwrap_or(opts.cursor);
    opts.mic = mic.unwrap_or(false);
    opts.system_audio = system_audio.unwrap_or(false);
    opts.keys = keys.unwrap_or_default();
    opts.webcam = webcam.filter(|w| !w.is_empty());
    opts.window_titles = window_titles.unwrap_or(false);

    let settings = *lock(&state.settings);
    opts.capture_border = settings.capture_border;
    let root = state.policy()?.root().to_path_buf();
    let dir = recorder::new_project_dir(&root)?;
    if state.selftest.load(Ordering::Relaxed)
        && let Ok(c) = dir.canonicalize()
    {
        lock(&state.selftest_projects).insert(strip_verbatim(c));
    }
    let t = Instant::now();
    // Hide our own windows before the first frame can be captured.
    let mut own = affinity::own_visible_windows();
    own.extend(owner_hwnd(app));
    let hidden = affinity::exclude_own_windows(
        &affinity::windows_to_exclude(&own, recorded_hwnd(&opts.source)),
        settings.exclude_own_windows,
    );
    let session = match RecordingSession::start(opts, &dir) {
        Ok(s) => s,
        Err(e) => {
            recorder::remove_session_files(&dir);
            return Err(e.into());
        }
    };
    let start_ms = t.elapsed().as_millis() as u64;
    let canvas = session.canvas();
    let session = Arc::new(Mutex::new(session));
    let monitor_stop = Arc::new(AtomicBool::new(false));
    spawn_monitor(app.clone(), session.clone(), monitor_stop.clone());
    *lock(&state.last_stop) = None;
    remember_recent(app, &dir);
    let active = Active {
        session,
        monitor_stop,
        project_dir: dir.clone(),
        _own_windows_hidden: hidden,
    };
    Ok((
        active,
        StartResponse {
            project_dir: dir,
            canvas,
            start_ms,
        },
    ))
}

/// Tick + health monitor; auto-stops on fatal errors.
fn spawn_monitor(app: AppHandle, session: Arc<Mutex<RecordingSession>>, stop: Arc<AtomicBool>) {
    thread::spawn(move || {
        let mut n = 0u64;
        while !stop.load(Ordering::Acquire) {
            let (tick, health) = {
                let mut s = lock(&session);
                let tick = Tick {
                    elapsed_ms: s.elapsed_ms(),
                    paused: s.is_paused(),
                };
                let health = n.is_multiple_of(5).then(|| s.health());
                (tick, health)
            };
            let _ = app.emit("recording://tick", tick);
            if let Some(h) = health {
                for w in h.warnings {
                    let _ = app.emit(
                        "recording://warning",
                        WarningEvent {
                            code: "warning".into(),
                            message: w,
                        },
                    );
                }
                if let Some(e) = h.error {
                    let err = CommandError::from(e);
                    let _ = app.emit("recording://error", err.clone());
                    let st = app.state::<AppState>();
                    // Ok(None): the user stopped at the same moment; no extra event.
                    match stop_active(&app, &st) {
                        Ok(Some(r)) => {
                            let _ = app.emit(
                                "recording://stopped",
                                StoppedEvent {
                                    result: Some(r),
                                    error: Some(err),
                                },
                            );
                        }
                        Ok(None) => {}
                        Err(e2) => {
                            let _ = app.emit(
                                "recording://stopped",
                                StoppedEvent {
                                    result: None,
                                    error: Some(e2),
                                },
                            );
                        }
                    }
                    return;
                }
            }
            n += 1;
            thread::sleep(Duration::from_millis(100));
        }
    });
}

/// Stop the active session if any (used by `stop_recording`, auto-stop and
/// window close). `Ok(None)` when nothing was recording.
pub fn stop_active(app: &AppHandle, state: &AppState) -> CmdResult<Option<StopResponse>> {
    let Some(active) = lock(&state.active).take_active() else {
        return Ok(None);
    };
    active.monitor_stop.store(true, Ordering::Release);
    let res = lock(&active.session).stop();
    allow_project_assets(app, &active.project_dir);
    let r = res?;
    let resp = StopResponse {
        project_dir: r.project_dir,
        duration_ms: r.duration_ms,
        warnings: r.warnings,
    };
    *lock(&state.last_stop) = Some(resp.clone());
    Ok(Some(resp))
}

/// Stop the current recording and write `project.json`. Idempotent: when nothing
/// is recording it returns the previous result (or an error if there is none).
#[tauri::command(async)]
pub fn stop_recording(app: AppHandle, state: State<'_, AppState>) -> CmdResult<StopResponse> {
    match stop_active(&app, &state)? {
        Some(r) => Ok(r),
        None => lock(&state.last_stop)
            .clone()
            .ok_or_else(|| CommandError::internal("not recording")),
    }
}

/// Pause (paused time is cut from every track). `false` if already paused.
#[tauri::command(async)]
pub fn pause_recording(state: State<'_, AppState>) -> CmdResult<bool> {
    let active = lock(&state.active);
    let a = active
        .active()
        .ok_or_else(|| CommandError::internal("not recording"))?;
    Ok(lock(&a.session).pause())
}

/// Resume after a pause. `false` if not paused.
#[tauri::command(async)]
pub fn resume_recording(state: State<'_, AppState>) -> CmdResult<bool> {
    let active = lock(&state.active);
    let a = active
        .active()
        .ok_or_else(|| CommandError::internal("not recording"))?;
    Ok(lock(&a.session).resume())
}

/// Whether a recording is running.
#[tauri::command(async)]
pub fn recording_state(state: State<'_, AppState>) -> CmdResult<RecordingStateResponse> {
    let active = lock(&state.active);
    Ok(match active.active() {
        Some(a) => {
            let s = lock(&a.session);
            RecordingStateResponse {
                recording: true,
                paused: s.is_paused(),
                elapsed_ms: s.elapsed_ms(),
                project_dir: Some(a.project_dir.clone()),
            }
        }
        None => RecordingStateResponse {
            recording: false,
            paused: false,
            elapsed_ms: 0,
            project_dir: None,
        },
    })
}

// ----------------------------------------------------------------- projects

/// Load a project (folder or its project.json). Path must pass the path policy.
#[tauri::command(async)]
pub fn load_project(
    app: AppHandle,
    state: State<'_, AppState>,
    path: PathBuf,
) -> CmdResult<Project> {
    let dir = state.policy()?.project_dir(&path)?;
    let project = Project::load(&dir)?;
    allow_project_assets(&app, &dir);
    remember_recent(&app, &dir);
    Ok(project)
}

/// Validate and atomically save a project into an existing, allowed project folder.
#[tauri::command(async)]
pub fn save_project(state: State<'_, AppState>, path: PathBuf, project: Project) -> CmdResult<()> {
    let dir = state.policy()?.project_dir(&path)?;
    if state.live_dir().as_deref() == Some(dir.as_path()) {
        return Err(CommandError::internal(
            "cannot save while this project is recording",
        ));
    }
    Ok(project.save(&dir)?)
}

/// Recent projects (store + scan of `Videos/Frostcap`), with `interrupted` flags.
#[tauri::command(async)]
pub fn list_recent_projects(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<Vec<RecentProject>> {
    let root = state.policy()?.root().to_path_buf();
    let live = state.live_dir();
    Ok(recent::list_recent(
        &recent_store(&app)?,
        Some(&root),
        live.as_deref(),
    ))
}

/// Rebuild an interrupted recording (status `recording` with no live session).
#[tauri::command(async)]
pub fn recover_project(
    app: AppHandle,
    state: State<'_, AppState>,
    path: PathBuf,
) -> CmdResult<Project> {
    let dir = state.policy()?.project_dir(&path)?;
    ensure_not_live(state.live_dir().as_deref(), &dir)?;
    let p = recovery::recover_project(&dir)?;
    allow_project_assets(&app, &dir);
    Ok(p)
}

/// Privacy scrub: remove key events and/or window titles, then save.
#[tauri::command(async)]
pub fn scrub_project(
    state: State<'_, AppState>,
    path: PathBuf,
    keys: bool,
    titles: bool,
) -> CmdResult<Project> {
    let dir = state.policy()?.project_dir(&path)?;
    ensure_not_live(state.live_dir().as_deref(), &dir)?;
    Ok(recovery::scrub_project_dir(&dir, keys, titles)?)
}

/// Run the auto-zoom generator on a project (pure; does not save).
#[tauri::command(async)]
pub fn generate_auto_zoom(
    project: Project,
    preset: Option<ZoomPreset>,
) -> CmdResult<Vec<ZoomSegment>> {
    project.validate()?;
    Ok(autozoom::generate_for_project(
        &project,
        preset.unwrap_or_default(),
    ))
}

// ------------------------------------------------------------------- export

/// Export a project. Progress: `export://progress` keyed by `jobId` (pass your
/// own to be able to cancel before the first event). Atomic: the output only
/// appears on success.
#[tauri::command(async)]
pub fn export_project(
    app: AppHandle,
    state: State<'_, AppState>,
    path: PathBuf,
    preset: Option<ExportRequest>,
    out_path: PathBuf,
    job_id: Option<String>,
) -> CmdResult<ExportResponse> {
    let pol = state.policy()?;
    let dir = pol.project_dir(&path)?;
    ensure_not_live(state.live_dir().as_deref(), &dir)?;
    let out = pol.export_target(&out_path, &dir)?;
    if let Some(ExportRequest::Output(p)) = &preset {
        // A format without an encoder on this PC (AV1) is refused up front.
        export::check_encoders(
            &[export::BatchRequest {
                preset: *p,
                out_path: out.clone(),
            }],
            &export::capabilities(),
        )?;
    }
    let job_id = job_id
        .filter(|j| !j.is_empty() && j.len() <= 64)
        .unwrap_or_else(|| format!("job-{}", state.jobs.fetch_add(1, Ordering::Relaxed) + 1));
    let guard = JobGuard::register(&state.exports, &job_id)?;
    let cancel = guard.cancel.clone();
    let mut last = (-1.0f64, String::new());
    let res = export::export_with(
        &dir,
        &preset.unwrap_or_default(),
        &out,
        &cancel,
        |p, stage| {
            if p - last.0 >= 0.01 || p >= 1.0 || stage != last.1 {
                last = (p, stage.to_owned());
                let _ = app.emit(
                    "export://progress",
                    ExportProgress {
                        job_id: job_id.clone(),
                        path: dir.clone(),
                        out_path: out.clone(),
                        progress: p,
                        stage: stage.into(),
                        job_index: None,
                    },
                );
            }
        },
    );
    drop(guard);
    let result = res?;
    state.policy()?.allow_picked_save(&result.out_path);
    // The self-test loads its export in a <video>: allow exactly that file and remember it
    // as this run's own (the only exports its cleanup may delete).
    allow_selftest_export(&app, &state, &result.out_path);
    Ok(ExportResponse { job_id, result })
}

/// Let the webview load exactly this exported file when it is a self-test export.
fn allow_selftest_export(app: &AppHandle, state: &AppState, out: &Path) {
    if state.selftest.load(Ordering::Relaxed)
        && out.file_name().is_some_and(|n| {
            n.to_string_lossy()
                .to_ascii_lowercase()
                .starts_with(crate::paths::SELFTEST_EXPORT_PREFIX)
        })
    {
        // Remember it: self-test cleanup may only delete exports this run wrote.
        if let Ok(c) = out.canonicalize() {
            lock(&state.selftest_projects).insert(strip_verbatim(c));
        }
        let _ = app.asset_protocol_scope().allow_file(out);
    }
}

/// Render several outputs of one project in one pass (decode and compositing are
/// shared). Progress: `export://progress` with `jobIndex`; cancel with `cancel_export`.
/// Every `outPath` goes through the export target policy; nothing starts if any
/// is refused. Returns one result per job (a failed job does not fail the others).
#[tauri::command(async)]
pub fn export_batch(
    app: AppHandle,
    state: State<'_, AppState>,
    project_path: PathBuf,
    jobs: Vec<export::BatchRequest>,
    job_id: Option<String>,
) -> CmdResult<BatchResponse> {
    let pol = state.policy()?;
    let dir = pol.project_dir(&project_path)?;
    ensure_not_live(state.live_dir().as_deref(), &dir)?;
    let outs: Vec<PathBuf> = jobs.iter().map(|j| j.out_path.clone()).collect();
    let targets = pol.batch_targets(&outs, &dir)?;
    let jobs: Vec<export::BatchRequest> = jobs
        .into_iter()
        .zip(&targets)
        .map(|(j, t)| export::BatchRequest {
            preset: j.preset,
            out_path: t.clone(),
        })
        .collect();
    let job_id = job_id
        .filter(|j| !j.is_empty() && j.len() <= 64)
        .unwrap_or_else(|| format!("job-{}", state.jobs.fetch_add(1, Ordering::Relaxed) + 1));
    let guard = JobGuard::register(&state.exports, &job_id)?;
    let cancel = guard.cancel.clone();
    let mut last: Vec<(f64, String)> = vec![(-1.0, String::new()); jobs.len()];
    let res = export::export_batch(&dir, &jobs, &cancel, |i, p, stage| {
        if let Some(l) = last.get_mut(i)
            && (p - l.0 >= 0.01 || p >= 1.0 || stage != l.1)
        {
            *l = (p, stage.to_owned());
            let _ = app.emit(
                "export://progress",
                ExportProgress {
                    job_id: job_id.clone(),
                    path: dir.clone(),
                    out_path: jobs[i].out_path.clone(),
                    progress: p,
                    stage: stage.into(),
                    job_index: Some(i),
                },
            );
        }
    });
    drop(guard);
    let results = res?;
    for r in results.iter().flatten() {
        state.policy()?.allow_picked_save(&r.out_path);
        allow_selftest_export(&app, &state, &r.out_path);
    }
    Ok(BatchResponse {
        job_id,
        results: crate::dto::batch_items(&targets, results),
    })
}

/// Which encoders this PC has: `{ h264, vp9, av1, formats }` (`formats` = usable
/// export `format` values). Export rejects a format that is not listed.
#[tauri::command(async)]
pub fn encoder_capabilities() -> CmdResult<export::Capabilities> {
    Ok(export::capabilities())
}

/// Suggest silence/idle cuts. Options (all optional) outside their ranges are rejected.
#[tauri::command(async)]
pub fn suggest_cuts(
    state: State<'_, AppState>,
    project_path: PathBuf,
    options: Option<export::SuggestRequest>,
) -> CmdResult<SuggestCutsResponse> {
    let dir = state.policy()?.project_dir(&project_path)?;
    let ranges = export::suggest_cuts(&dir, &options.unwrap_or_default())?;
    Ok(SuggestCutsResponse { ranges })
}

/// Native picker for a background image (.png/.jpg/.jpeg). `null` when cancelled.
#[tauri::command(async)]
pub fn pick_image(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Option<PathBuf>> {
    let picked = dialogs::pick_image(owner_hwnd(&app))?;
    if let Some(p) = &picked {
        state.policy()?.allow_picked(p);
    }
    Ok(picked)
}

/// Copy a picked image into the project as `background-<hash>.png|jpg` and return the
/// file name. The project must be under the projects root (or opened this session)
/// and not recording; the image must come from `pick_image` (<= 20 MiB, png/jpg).
#[tauri::command(async)]
pub fn import_background(
    app: AppHandle,
    state: State<'_, AppState>,
    project_path: PathBuf,
    source_path: PathBuf,
) -> CmdResult<ImportBackgroundResponse> {
    let pol = state.policy()?;
    let dir = pol.project_dir(&project_path)?;
    ensure_not_live(state.live_dir().as_deref(), &dir)?;
    // One open, validated through the handle, then handed over: no check-then-open race.
    let (src, ext) = pol.import_open(&source_path)?;
    let file = frostcap_core::project::import_background_file(&dir, src, &ext)?;
    // One exact-file allow so the editor preview can load it (not a wider scope).
    let _ = app.asset_protocol_scope().allow_file(dir.join(&file));
    Ok(ImportBackgroundResponse { file })
}

/// Registers an export job id; frees it on drop (also when the export panics).
struct JobGuard<'a> {
    jobs: &'a Mutex<HashMap<String, Arc<AtomicBool>>>,
    id: String,
    cancel: Arc<AtomicBool>,
}

impl<'a> JobGuard<'a> {
    fn register(jobs: &'a Mutex<HashMap<String, Arc<AtomicBool>>>, id: &str) -> CmdResult<Self> {
        let mut map = lock(jobs);
        if map.contains_key(id) {
            return Err(CommandError::internal(format!(
                "export {id} is already running"
            )));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        map.insert(id.to_owned(), cancel.clone());
        Ok(Self {
            jobs,
            id: id.to_owned(),
            cancel,
        })
    }
}

impl Drop for JobGuard<'_> {
    fn drop(&mut self) {
        lock(self.jobs).remove(&self.id);
    }
}

/// Request cancellation of a running export. `false` if no such job.
#[tauri::command(async)]
pub fn cancel_export(state: State<'_, AppState>, job_id: String) -> CmdResult<bool> {
    Ok(lock(&state.exports)
        .get(&job_id)
        .map(|c| c.store(true, Ordering::Relaxed))
        .is_some())
}

// ------------------------------------------------------------------ dialogs

/// Native folder picker for a `.frostcap` project. `null` when cancelled.
#[tauri::command(async)]
pub fn pick_project(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Option<PathBuf>> {
    let picked = dialogs::pick_project(owner_hwnd(&app))?;
    if let Some(p) = &picked {
        state.policy()?.allow_picked(p);
    }
    Ok(picked)
}

/// Native folder picker. `null` when cancelled.
#[tauri::command(async)]
pub fn pick_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    title: Option<String>,
) -> CmdResult<Option<PathBuf>> {
    let picked = dialogs::pick_folder(
        owner_hwnd(&app),
        title.as_deref().unwrap_or("Choose a folder"),
    )?;
    if let Some(p) = &picked {
        state.policy()?.allow_picked(p);
    }
    Ok(picked)
}

/// Native save dialog with one extension filter (`mp4` or `gif`). `null` when cancelled.
#[tauri::command(async)]
pub fn pick_save_path(
    app: AppHandle,
    state: State<'_, AppState>,
    default_name: String,
    ext: String,
) -> CmdResult<Option<PathBuf>> {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    if !crate::paths::EXPORT_EXTS.contains(&ext.as_str()) {
        return Err(Error::InvalidPath(format!("unsupported extension {ext:?}")).into());
    }
    let name: String = default_name
        .chars()
        .filter(|c| !r#"\/:*?"<>|"#.contains(*c))
        .take(120)
        .collect();
    let picked = dialogs::pick_save_path(owner_hwnd(&app), &name, &ext)?;
    if let Some(p) = &picked {
        state.policy()?.allow_picked_save(p);
    }
    Ok(picked)
}

/// Show a project folder or exported file in Explorer.
#[tauri::command(async)]
pub fn reveal_in_folder(state: State<'_, AppState>, path: PathBuf) -> CmdResult<()> {
    let pol = state.policy()?;
    let ok = pol.project_dir(&path).is_ok() || pol.is_known(&path);
    if !ok {
        return Err(Error::InvalidPath(format!(
            "{} is not a Frostcap project or export",
            path.display()
        ))
        .into());
    }
    Ok(dialogs::reveal_in_folder(&path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_allows_one_start_at_a_time() {
        let mut s = Slot::Idle;
        assert!(s.begin_start().is_ok());
        assert!(matches!(s, Slot::Starting));
        assert!(s.begin_start().is_err(), "second start while starting");
        s = Slot::Idle;
        assert!(s.begin_start().is_ok());
    }

    #[test]
    fn export_job_is_released_even_on_panic() {
        let jobs: Mutex<HashMap<String, Arc<AtomicBool>>> = Mutex::default();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = JobGuard::register(&jobs, "job-1").unwrap();
            assert!(
                JobGuard::register(&jobs, "job-1").is_err(),
                "duplicate id refused"
            );
            panic!("renderer bug");
        }));
        assert!(r.is_err());
        assert!(lock(&jobs).is_empty(), "the id is free again");
        assert!(JobGuard::register(&jobs, "job-1").is_ok());
    }

    #[test]
    fn verbatim_prefix_is_stripped_but_unc_is_kept() {
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\v\a.frostcap")),
            PathBuf::from(r"C:\v\a.frostcap")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\UNC\srv\share")),
            PathBuf::from(r"\\?\UNC\srv\share")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(r"C:\x")),
            PathBuf::from(r"C:\x")
        );
    }

    struct Answer(bool, std::cell::Cell<u32>);
    impl Confirm for Answer {
        fn confirm(&self, _title: &str, _text: &str) -> bool {
            self.1.set(self.1.get() + 1);
            self.0
        }
    }

    #[test]
    fn hiding_the_capture_border_needs_a_native_yes() {
        let no = Answer(false, 0.into());
        let e = border_change_allowed(true, false, &no).unwrap_err();
        assert_eq!(e.code, frostcap_core::ErrorCode::Cancelled);
        assert_eq!(no.1.get(), 1);
        let yes = Answer(true, 0.into());
        assert!(border_change_allowed(true, false, &yes).is_ok());
        assert_eq!(yes.1.get(), 1);
        // Turning it on, or no change, never asks.
        let never = Answer(false, 0.into());
        assert!(border_change_allowed(false, true, &never).is_ok());
        assert!(border_change_allowed(true, true, &never).is_ok());
        assert!(border_change_allowed(false, false, &never).is_ok());
        assert_eq!(never.1.get(), 0);
    }

    #[test]
    fn only_the_recorded_window_is_spared_from_exclusion() {
        use frostcap_core::sources::CaptureSource;
        assert_eq!(
            recorded_hwnd(&CaptureSource::Window {
                hwnd: 42,
                pid: None
            }),
            Some(42)
        );
        assert_eq!(recorded_hwnd(&CaptureSource::PrimaryMonitor), None);
        assert_eq!(recorded_hwnd(&CaptureSource::AllDisplays), None);
    }

    #[test]
    fn background_images_of_a_project_are_listed_for_the_asset_scope() {
        let mut p = frostcap_core::project::samples::fixture();
        assert!(background_files(&p).is_empty() || p.edits.style.background_image.is_some());
        p.edits.style.background_image = Some(frostcap_core::project::BackgroundImage {
            file: "background-aaaa.png".into(),
            fit: Default::default(),
            blur: 0.0,
        });
        p.edits.style.webcam.background.image = Some("background-bbbb.jpg".into());
        let files = background_files(&p);
        assert!(files.contains(&"background-aaaa.png".to_owned()));
        assert!(files.contains(&"background-bbbb.jpg".to_owned()));
        p.edits.style.background_image = None;
        p.edits.style.webcam.background.image = None;
        assert!(background_files(&p).is_empty());
    }

    #[test]
    fn live_folder_is_protected() {
        let live = PathBuf::from(r"C:\v\a.frostcap");
        assert!(ensure_not_live(Some(&live), &live).is_err());
        assert!(ensure_not_live(Some(&live), Path::new(r"C:\v\b.frostcap")).is_ok());
        assert!(ensure_not_live(None, &live).is_ok());
    }
}
