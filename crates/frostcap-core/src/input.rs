//! Input metadata recorder.
//!
//! Runs on its own thread and polls at ~120 Hz (high-resolution waitable timer):
//! `GetCursorPos` for the cursor, `GetAsyncKeyState` for mouse buttons and key-downs,
//! `GetForegroundWindow` + DWM bounds for focus changes. No hooks are installed and
//! nothing leaves the machine; the data only ends up in the local `project.json`.
//!
//! Keystrokes: only *key-down* events are kept (virtual-key code, display label,
//! modifiers, timestamp). Text is never reconstructed, key releases are not stored,
//! and the mode is [`KeyCapture`]: `off` (default), `shortcuts` (Win chords, chords
//! with exactly one of Ctrl/Alt -- Ctrl+Alt alone is AltGr -- and F-keys) or `all`
//! (opt-in, needed for typing auto-zoom). Nothing is recorded while paused.
//! Foreground windows are stored as opaque per-session ids; titles only when opted
//! in. The crash-recovery sidecar never contains keys or titles.
//!
//! All events are stamped on the QPC clock ([`crate::clock`]) and converted to the
//! video clock when the recording stops.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, POINT};
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer,
    TIMER_ALL_ACCESS, WaitForSingleObject,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyNameTextW, MAPVK_VK_TO_VSC, MapVirtualKeyW, VK_CONTROL, VK_LBUTTON,
    VK_LWIN, VK_MBUTTON, VK_MENU, VK_RBUTTON, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetWindowTextW, IsIconic,
};
use windows::core::PCWSTR;

use crate::clock::{self, HNS_PER_MS};
use crate::project::limits;
use crate::project::{
    ButtonState, ClickEvent, CursorSample, FocusChange, KeyEvent, Modifiers, MouseButton, Rect,
    Tracks,
};
use crate::sources::window_rect;
use crate::timeline::{Gap, Timeline, map_with};

/// Origin of the capture source, used to convert screen to source coordinates.
#[derive(Clone, Copy, Debug)]
pub enum SourceOrigin {
    /// Fixed rectangle (monitor).
    Fixed(Rect),
    /// A window that may move; its rect is re-queried every tick.
    Window(usize),
}

impl SourceOrigin {
    /// Current origin; `None` while a window source is minimized or gone (the
    /// caller keeps the last known origin).
    fn origin(&self) -> Option<(i32, i32)> {
        match *self {
            SourceOrigin::Fixed(r) => Some((r.x, r.y)),
            SourceOrigin::Window(h) => {
                let hwnd = HWND(h as *mut _);
                // SAFETY: plain queries; invalid handles just fail.
                if unsafe { IsIconic(hwnd) }.as_bool() {
                    return None;
                }
                window_rect(hwnd).map(|r| (r.x, r.y))
            }
        }
    }
}

/// Options for [`InputRecorder::start`].
#[derive(Clone, Debug)]
pub struct InputOptions {
    /// Sampling rate in Hz (default 120).
    pub hz: u32,
    /// Which key-downs to record.
    pub keys: KeyCapture,
    /// Crash-safety sidecar: new events are appended here as JSON lines every
    /// ~2 s (raw QPC timestamps; see [`read_sidecar`]).
    pub sidecar: Option<std::path::PathBuf>,
    /// Store foreground window titles (opt-in; default: opaque ids only).
    pub titles: bool,
    /// Recording timeline: nothing is sampled while it is paused.
    pub timeline: Option<Arc<Timeline>>,
    /// Canvas pixels per desktop pixel (1.0; below 1 when an all-displays canvas was
    /// scaled down to fit the encoder). Cursor and focus coordinates are multiplied by it.
    pub scale: f64,
}

/// One line of the `input.jsonl` crash-recovery sidecar (raw QPC hns timestamps).
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "k", rename_all = "camelCase")]
pub enum SidecarEvent {
    /// Cursor sample.
    C {
        /// Timestamp (hns, shared clock).
        ts: i64,
        /// X.
        x: i32,
        /// Y.
        y: i32,
    },
    /// Button transition.
    B {
        /// Timestamp.
        ts: i64,
        /// X.
        x: i32,
        /// Y.
        y: i32,
        /// Button.
        button: MouseButton,
        /// Down/up.
        state: ButtonState,
    },
    /// Focus change (opaque per-session window id; titles are never written here).
    F {
        /// Timestamp.
        ts: i64,
        /// Per-session window id.
        id: u32,
        /// Rect.
        rect: Rect,
    },
}

/// Max `input.jsonl` size accepted by [`read_sidecar`].
pub const MAX_SIDECAR_BYTES: u64 = 512 * 1024 * 1024;
/// Lines longer than this are ignored.
pub const MAX_SIDECAR_LINE: usize = 512;

/// Rebuild [`RawInput`] from a sidecar written by a crashed session.
pub fn read_sidecar(path: &std::path::Path) -> RawInput {
    let mut raw = RawInput::default();
    if std::fs::metadata(path).map_or(true, |m| m.len() > MAX_SIDECAR_BYTES) {
        return raw;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return raw;
    };
    for line in text.lines().filter(|l| l.len() <= MAX_SIDECAR_LINE) {
        match serde_json::from_str::<SidecarEvent>(line) {
            Ok(SidecarEvent::C { ts, x, y }) if raw.cursor.len() < limits::MAX_CURSOR_SAMPLES => {
                raw.cursor.push(Stamped { ts, ev: (x, y) });
            }
            Ok(SidecarEvent::B {
                ts,
                x,
                y,
                button,
                state,
            }) if raw.clicks.len() < limits::MAX_EVENTS => {
                raw.clicks.push(Stamped {
                    ts,
                    ev: (x, y, button, state),
                });
            }
            Ok(SidecarEvent::F { ts, id, rect }) if raw.focus.len() < limits::MAX_FOCUS => {
                raw.focus.push(Stamped {
                    ts,
                    ev: (id, String::new(), rect),
                });
            }
            _ => {} // torn last line or over cap
        }
    }
    raw
}

/// Appends not-yet-flushed events to the sidecar.
struct SidecarWriter {
    file: std::io::BufWriter<std::fs::File>,
    done: [usize; 3],
}

impl SidecarWriter {
    fn open(path: &std::path::Path) -> Option<Self> {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        Some(Self {
            file: std::io::BufWriter::new(f),
            done: [0; 3],
        })
    }
    fn flush(&mut self, raw: &RawInput) -> std::io::Result<()> {
        use std::io::Write as _;
        let mut line = |e: &SidecarEvent| -> std::io::Result<()> {
            serde_json::to_writer(&mut self.file, e).map_err(std::io::Error::other)?;
            self.file.write_all(b"\n")
        };
        for s in &raw.cursor[self.done[0]..] {
            line(&SidecarEvent::C {
                ts: s.ts,
                x: s.ev.0,
                y: s.ev.1,
            })?;
        }
        for s in &raw.clicks[self.done[1]..] {
            let (x, y, button, state) = s.ev;
            line(&SidecarEvent::B {
                ts: s.ts,
                x,
                y,
                button,
                state,
            })?;
        }
        // Keys and window titles are never written to the sidecar.
        for s in &raw.focus[self.done[2]..] {
            line(&SidecarEvent::F {
                ts: s.ts,
                id: s.ev.0,
                rect: s.ev.2,
            })?;
        }
        self.done = [raw.cursor.len(), raw.clicks.len(), raw.focus.len()];
        self.file.flush()
    }
}

/// Keystroke capture mode (privacy: default off).
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum KeyCapture {
    /// No keys recorded.
    #[default]
    Off,
    /// Only shortcuts: keys pressed with Win, or with exactly one of Ctrl / Alt
    /// (Ctrl+Alt alone is AltGr and is ignored), plus F1-F24.
    Shortcuts,
    /// Every key-down (still no releases and no text).
    All,
}

impl KeyCapture {
    fn accepts(self, vk: u16, m: Modifiers) -> bool {
        match self {
            KeyCapture::Off => false,
            KeyCapture::All => true,
            // Ctrl+Alt without Win is AltGr (typing characters on many layouts).
            KeyCapture::Shortcuts => m.win || (m.ctrl != m.alt) || (0x70..=0x87).contains(&vk),
        }
    }
}

impl Default for InputOptions {
    fn default() -> Self {
        Self {
            hz: 120,
            keys: KeyCapture::Off,
            sidecar: None,
            titles: false,
            timeline: None,
            scale: 1.0,
        }
    }
}

/// Raw event stamped on the QPC clock (hns).
#[derive(Clone, Debug)]
struct Stamped<T> {
    ts: i64,
    ev: T,
}

/// Everything captured by the input thread, on the QPC clock.
#[derive(Default, Debug)]
pub struct RawInput {
    cursor: Vec<Stamped<(i32, i32)>>,
    clicks: Vec<Stamped<(i32, i32, MouseButton, ButtonState)>>,
    keys: Vec<Stamped<(u16, Modifiers)>>,
    /// (per-session window id, title (empty unless opted in), rect).
    focus: Vec<Stamped<(u32, String, Rect)>>,
    /// Number of polling ticks executed (diagnostics).
    pub ticks: u64,
    /// The input thread panicked; tracks are lost (reported as a warning).
    pub failed: bool,
}

/// Join the input thread; a panic yields an empty, `failed` result.
fn collect(h: Option<JoinHandle<RawInput>>) -> RawInput {
    match h.map(JoinHandle::join) {
        Some(Ok(r)) => r,
        Some(Err(_)) => RawInput {
            failed: true,
            ..RawInput::default()
        },
        None => RawInput::default(),
    }
}

impl RawInput {
    /// Convert to project tracks on the output timeline (first video frame `t0`,
    /// pauses in `gaps` removed; events inside a pause are dropped).
    /// Events before `t0` are dropped, except that the latest cursor position and
    /// focused window before `t0` are carried over to `t = 0`.
    pub fn into_tracks(self, t0: i64, gaps: &[Gap]) -> Tracks {
        let mut tracks = Tracks::default();
        let ms = |ts: i64| map_with(t0, gaps, None, ts).map(|v| (v / HNS_PER_MS) as u64);

        let pre_cursor = self.cursor.iter().rev().find(|s| s.ts < t0).map(|s| s.ev);
        if let Some((x, y)) = pre_cursor {
            tracks.cursor.push(CursorSample { t_ms: 0, x, y });
        }
        tracks.cursor.extend(self.cursor.iter().filter_map(|s| {
            Some(CursorSample {
                t_ms: ms(s.ts)?,
                x: s.ev.0,
                y: s.ev.1,
            })
        }));
        tracks.clicks = self
            .clicks
            .iter()
            .filter_map(|s| {
                let (x, y, button, state) = s.ev;
                Some(ClickEvent {
                    t_ms: ms(s.ts)?,
                    x,
                    y,
                    button,
                    state,
                })
            })
            .collect();
        tracks.keys = self
            .keys
            .iter()
            .filter_map(|s| {
                Some(KeyEvent {
                    t_ms: ms(s.ts)?,
                    vk: s.ev.0,
                    label: vk_label(s.ev.0),
                    modifiers: s.ev.1,
                })
            })
            .collect();
        if let Some(pre) = self.focus.iter().rev().find(|s| s.ts < t0) {
            tracks.focus.push(FocusChange {
                t_ms: 0,
                window_id: pre.ev.0,
                title: pre.ev.1.clone(),
                rect: pre.ev.2,
            });
        }
        tracks.focus.extend(self.focus.into_iter().filter_map(|s| {
            Some(FocusChange {
                t_ms: ms(s.ts)?,
                window_id: s.ev.0,
                title: s.ev.1,
                rect: s.ev.2,
            })
        }));
        tracks
    }
}

/// Handle to a running input recorder thread.
pub struct InputRecorder {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<RawInput>>,
}

impl InputRecorder {
    /// Spawn the polling thread.
    pub fn start(origin: SourceOrigin, opts: InputOptions) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let handle = thread::Builder::new()
            .name("frostcap-input".into())
            .spawn({
                let stop = stop.clone();
                move || run(origin, opts, &stop)
            })?;
        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }

    /// Stop the thread and return everything it captured.
    pub fn stop(&mut self) -> RawInput {
        self.stop.store(true, Ordering::Release);
        collect(self.handle.take())
    }
}

impl Drop for InputRecorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

struct Ticker {
    timer: Option<HANDLE>,
    period_hns: i64,
    next: i64,
}

impl Ticker {
    fn new(hz: u32) -> Self {
        let period_hns = clock::HNS_PER_SEC / i64::from(hz.clamp(1, 1000));
        // SAFETY: creating an unnamed timer; handle is closed in Drop.
        let timer = unsafe {
            CreateWaitableTimerExW(
                None,
                PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
        }
        .ok();
        Self {
            timer,
            period_hns,
            next: clock::now_hns() + period_hns,
        }
    }

    fn wait(&mut self) {
        let now = clock::now_hns();
        let remaining = self.next - now;
        if remaining <= 0 {
            // Fell behind; resync instead of bursting.
            self.next = now + self.period_hns;
            return;
        }
        self.next += self.period_hns;
        match self.timer {
            Some(t) => {
                let due = -remaining; // negative = relative, in 100 ns
                // SAFETY: valid timer handle and due-time pointer.
                unsafe {
                    if SetWaitableTimer(t, &due, 0, None, None, false).is_ok() {
                        WaitForSingleObject(t, INFINITE);
                    } else {
                        thread::sleep(Duration::from_nanos(remaining as u64 * 100));
                    }
                }
            }
            None => thread::sleep(Duration::from_nanos(remaining as u64 * 100)),
        }
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        if let Some(t) = self.timer.take() {
            // SAFETY: we own the handle.
            let _ = unsafe { CloseHandle(t) };
        }
    }
}

fn key_down(vk: u16) -> (bool, bool) {
    // SAFETY: plain query.
    let s = unsafe { GetAsyncKeyState(i32::from(vk)) };
    (s < 0, s & 1 != 0)
}

fn modifiers() -> Modifiers {
    Modifiers {
        ctrl: key_down(VK_CONTROL.0).0,
        shift: key_down(VK_SHIFT.0).0,
        alt: key_down(VK_MENU.0).0,
        win: key_down(VK_LWIN.0).0 || key_down(VK_RWIN.0).0,
    }
}

/// Virtual keys never reported as key events (mouse buttons and modifiers).
fn is_ignored_vk(vk: u16) -> bool {
    matches!(vk, 0x01..=0x07 | 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5 | 0xFF)
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    // SAFETY: valid buffer.
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Multiply a coordinate by the canvas scale (identity at 1.0).
fn scaled(v: i32, scale: f64) -> i32 {
    if scale == 1.0 {
        v
    } else {
        (f64::from(v) * scale).round() as i32
    }
}

/// One poll of the OS input state (absolute screen coordinates).
#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    pub ts: i64,
    /// Current source origin (screen px).
    pub origin: (i32, i32),
    pub cursor: Option<(i32, i32)>,
    /// Left, right, middle.
    pub buttons: [bool; 3],
    /// Per VK: (down now, pressed since last poll). All false when keys are off.
    pub keys: [(bool, bool); 256],
    pub mods: Modifiers,
    /// Foreground window (handle, absolute rect).
    pub foreground: Option<(usize, Rect)>,
    /// Canvas pixels per desktop pixel.
    pub scale: f64,
}

/// Pure per-tick event extraction (unit-testable without the OS).
pub(crate) struct Sampler {
    keys_mode: KeyCapture,
    titles: bool,
    last_pos: Option<(i32, i32)>,
    buttons_down: [bool; 3],
    keys_down: [bool; 256],
    last_fg: Option<(usize, Rect)>,
    last_focus_ts: i64,
    window_ids: std::collections::HashMap<usize, u32>,
}

const BUTTONS: [MouseButton; 3] = [MouseButton::Left, MouseButton::Right, MouseButton::Middle];

impl Sampler {
    /// `initial_keys` primes the key state so keys held at start are not reported.
    pub(crate) fn new(
        keys_mode: KeyCapture,
        titles: bool,
        initial_keys: &[(bool, bool); 256],
    ) -> Self {
        let mut keys_down = [false; 256];
        for (d, k) in keys_down.iter_mut().zip(initial_keys) {
            *d = k.0;
        }
        Self {
            keys_mode,
            titles,
            last_pos: None,
            buttons_down: [false; 3],
            keys_down,
            last_fg: None,
            last_focus_ts: i64::MIN,
            window_ids: std::collections::HashMap::new(),
        }
    }

    /// Process one snapshot. While `paused` the internal state follows the OS
    /// (so nothing held during the pause fires afterwards) but nothing is recorded.
    pub(crate) fn tick(
        &mut self,
        s: &Snapshot,
        paused: bool,
        title_of: impl Fn(usize) -> String,
        out: &mut RawInput,
    ) {
        let (ox, oy) = s.origin;
        let ts = s.ts;
        if let Some((x, y)) = s.cursor {
            let p = (scaled(x - ox, s.scale), scaled(y - oy, s.scale));
            if !paused && self.last_pos != Some(p) && out.cursor.len() < limits::MAX_CURSOR_SAMPLES
            {
                out.cursor.push(Stamped { ts, ev: p });
                self.last_pos = Some(p);
            }
        }
        let (cx, cy) = self.last_pos.unwrap_or_default();
        for (i, &down) in s.buttons.iter().enumerate() {
            if down != self.buttons_down[i] {
                self.buttons_down[i] = down;
                if !paused && out.clicks.len() < limits::MAX_EVENTS {
                    let state = if down {
                        ButtonState::Down
                    } else {
                        ButtonState::Up
                    };
                    out.clicks.push(Stamped {
                        ts,
                        ev: (cx, cy, BUTTONS[i], state),
                    });
                }
            }
        }
        if self.keys_mode != KeyCapture::Off {
            for vk in 0x08u16..=0xFE {
                if is_ignored_vk(vk) {
                    continue;
                }
                let (down, pressed_since_last) = s.keys[vk as usize];
                let was = self.keys_down[vk as usize];
                self.keys_down[vk as usize] = down;
                let pressed = (down && !was) || (!down && !was && pressed_since_last);
                if pressed
                    && !paused
                    && out.keys.len() < limits::MAX_EVENTS
                    && self.keys_mode.accepts(vk, s.mods)
                {
                    out.keys.push(Stamped {
                        ts,
                        ev: (vk, s.mods),
                    });
                }
            }
        }
        if paused {
            return;
        }
        if let Some((h, r)) = s.foreground {
            let rel = Rect {
                x: scaled(r.x - ox, s.scale),
                y: scaled(r.y - oy, s.scale),
                width: scaled(r.width, s.scale),
                height: scaled(r.height, s.scale),
            };
            let changed_window = self.last_fg.is_none_or(|(lh, _)| lh != h);
            let moved = self.last_fg.is_some_and(|(lh, lr)| lh == h && lr != rel);
            if (changed_window || (moved && ts - self.last_focus_ts >= 100 * HNS_PER_MS))
                && out.focus.len() < limits::MAX_FOCUS
            {
                let next = self.window_ids.len() as u32 + 1;
                let id = *self.window_ids.entry(h).or_insert(next);
                let title = if self.titles {
                    title_of(h)
                } else {
                    String::new()
                };
                out.focus.push(Stamped {
                    ts,
                    ev: (id, title, rel),
                });
                self.last_fg = Some((h, rel));
                self.last_focus_ts = ts;
            }
        }
    }
}

fn poll(keys: bool, origin: (i32, i32), scale: f64) -> Snapshot {
    let ts = clock::now_hns();
    let mut pt = POINT::default();
    // SAFETY: valid out param.
    let cursor = unsafe { GetCursorPos(&mut pt) }.ok().map(|()| (pt.x, pt.y));
    let buttons = [
        key_down(VK_LBUTTON.0).0,
        key_down(VK_RBUTTON.0).0,
        key_down(VK_MBUTTON.0).0,
    ];
    let mut k = [(false, false); 256];
    if keys {
        for vk in 0x08u16..=0xFE {
            k[vk as usize] = key_down(vk);
        }
    }
    // SAFETY: plain query.
    let fg = unsafe { GetForegroundWindow() };
    let foreground = (!fg.is_invalid())
        .then(|| window_rect(fg).map(|r| (fg.0 as usize, r)))
        .flatten();
    Snapshot {
        ts,
        origin,
        cursor,
        buttons,
        keys: k,
        mods: if keys {
            modifiers()
        } else {
            Modifiers::default()
        },
        foreground,
        scale,
    }
}

fn run(origin: SourceOrigin, opts: InputOptions, stop: &AtomicBool) -> RawInput {
    let mut out = RawInput::default();
    let mut ticker = Ticker::new(opts.hz);
    let mut sidecar = opts.sidecar.as_deref().and_then(SidecarWriter::open);
    let keys = opts.keys != KeyCapture::Off;
    let mut last_origin = origin.origin().unwrap_or((0, 0));
    let mut sampler = Sampler::new(
        opts.keys,
        opts.titles,
        &poll(keys, last_origin, opts.scale).keys,
    );
    let title_of = |h: usize| window_title(HWND(h as *mut _));

    while !stop.load(Ordering::Acquire) {
        out.ticks += 1;
        if let Some(o) = origin.origin() {
            last_origin = o;
        }
        let paused = opts.timeline.as_ref().is_some_and(|t| t.is_paused());
        let snap = poll(keys, last_origin, opts.scale);
        sampler.tick(&snap, paused, title_of, &mut out);

        if !paused
            && let Some(w) = sidecar.as_mut()
            && out.ticks % (u64::from(opts.hz.max(1)) * 2) == 0
            && w.flush(&out).is_err()
        {
            sidecar = None; // disk trouble: keep recording in memory
        }
        ticker.wait();
    }
    if let Some(w) = sidecar.as_mut() {
        let _ = w.flush(&out);
    }
    out
}
/// Human readable label for a virtual-key code (for the key overlay).
pub fn vk_label(vk: u16) -> String {
    let fixed = match vk {
        0x08 => "Backspace",
        0x09 => "Tab",
        0x0D => "Enter",
        0x13 => "Pause",
        0x14 => "CapsLock",
        0x1B => "Esc",
        0x20 => "Space",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x23 => "End",
        0x24 => "Home",
        0x25 => "←",
        0x26 => "↑",
        0x27 => "→",
        0x28 => "↓",
        0x2C => "PrintScreen",
        0x2D => "Insert",
        0x2E => "Delete",
        0x5D => "Menu",
        0x6A => "Num*",
        0x6B => "Num+",
        0x6D => "Num-",
        0x6E => "Num.",
        0x6F => "Num/",
        0x90 => "NumLock",
        0x91 => "ScrollLock",
        _ => "",
    };
    if !fixed.is_empty() {
        return fixed.to_owned();
    }
    match vk {
        0x30..=0x39 | 0x41..=0x5A => return char::from(vk as u8).to_string(),
        0x60..=0x69 => return format!("Num{}", vk - 0x60),
        0x70..=0x87 => return format!("F{}", vk - 0x6F),
        _ => {}
    }
    // SAFETY: plain conversions with a valid buffer.
    unsafe {
        let sc = MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC);
        if sc != 0 {
            let mut buf = [0u16; 64];
            let n = GetKeyNameTextW((sc << 16) as i32, &mut buf);
            if n > 0 {
                return String::from_utf16_lossy(&buf[..n as usize]);
            }
        }
    }
    format!("VK{vk:02X}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(vk_label(0x41), "A");
        assert_eq!(vk_label(0x31), "1");
        assert_eq!(vk_label(0x0D), "Enter");
        assert_eq!(vk_label(0x70), "F1");
        assert_eq!(vk_label(0x87), "F24");
    }

    fn snap(ts: i64) -> Snapshot {
        Snapshot {
            ts,
            origin: (0, 0),
            cursor: Some((10, 10)),
            buttons: [false; 3],
            keys: [(false, false); 256],
            mods: Modifiers::default(),
            foreground: None,
            scale: 1.0,
        }
    }

    #[test]
    fn a_scaled_canvas_scales_cursor_and_focus_coordinates() {
        let mut s = snap(1);
        s.origin = (-100, 0);
        s.cursor = Some((900, 500));
        s.scale = 0.8;
        s.foreground = Some((
            1,
            Rect {
                x: 100,
                y: 0,
                width: 500,
                height: 250,
            },
        ));
        let mut sampler = Sampler::new(KeyCapture::Off, false, &[(false, false); 256]);
        let mut out = RawInput::default();
        sampler.tick(&s, false, |_| String::new(), &mut out);
        assert_eq!(out.cursor[0].ev, (800, 400));
        let r = out.focus[0].ev.2;
        assert_eq!((r.x, r.y, r.width, r.height), (160, 0, 400, 200));
    }

    #[test]
    fn shortcuts_mode_ignores_altgr_and_plain_typing() {
        let m = |ctrl, alt, win| Modifiers {
            ctrl,
            alt,
            win,
            shift: false,
        };
        let s = KeyCapture::Shortcuts;
        assert!(s.accepts(0x53, m(true, false, false)), "Ctrl+S");
        assert!(s.accepts(0x09, m(false, true, false)), "Alt+Tab");
        assert!(s.accepts(0x45, m(true, true, true)), "Win chord");
        assert!(
            !s.accepts(0x45, m(true, true, false)),
            "AltGr+E types a character"
        );
        assert!(!s.accepts(0x41, m(false, false, false)), "plain A");
        assert!(s.accepts(0x74, m(false, false, false)), "F5");
        assert!(!KeyCapture::Off.accepts(0x74, m(true, false, false)));
    }

    #[test]
    fn sampler_records_key_downs_once_and_low_bit_taps() {
        let mut out = RawInput::default();
        let mut s = Sampler::new(KeyCapture::All, false, &[(false, false); 256]);
        let mut a = snap(1);
        a.keys[0x41] = (true, true);
        s.tick(&a, false, |_| String::new(), &mut out);
        let mut held = snap(2);
        held.keys[0x41] = (true, false);
        s.tick(&held, false, |_| String::new(), &mut out);
        assert_eq!(out.keys.len(), 1, "held key is one press");
        // Tap entirely between two polls: only the "pressed since last" bit is set.
        let mut tap = snap(3);
        tap.keys[0x42] = (false, true);
        s.tick(&tap, false, |_| String::new(), &mut out);
        assert_eq!(
            out.keys.iter().map(|k| k.ev.0).collect::<Vec<_>>(),
            vec![0x41, 0x42]
        );
    }

    #[test]
    fn nothing_is_recorded_while_paused() {
        let mut out = RawInput::default();
        let mut s = Sampler::new(KeyCapture::All, false, &[(false, false); 256]);
        let mut p = snap(1);
        p.keys[0x41] = (true, true);
        p.buttons[0] = true;
        p.cursor = Some((50, 60));
        p.foreground = Some((
            7,
            Rect {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
        ));
        s.tick(&p, true, |_| "Secret".into(), &mut out);
        assert!(
            out.cursor.is_empty()
                && out.clicks.is_empty()
                && out.keys.is_empty()
                && out.focus.is_empty()
        );
        // After resume, keys/buttons still held from the pause are not reported.
        let mut r = p;
        r.ts = 2;
        s.tick(&r, false, |_| String::new(), &mut out);
        assert!(out.keys.is_empty() && out.clicks.is_empty());
        assert_eq!(out.cursor.len(), 1);
    }

    #[test]
    fn focus_uses_opaque_ids_and_titles_only_when_opted_in() {
        let mut out = RawInput::default();
        let mut s = Sampler::new(KeyCapture::Off, false, &[(false, false); 256]);
        let r = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        for (ts, h) in [(1, 0xAAAA), (2, 0xBBBB), (3, 0xAAAA)] {
            let mut f = snap(ts);
            f.foreground = Some((h, r));
            s.tick(&f, false, |_| "Bank - Statement".into(), &mut out);
        }
        let ids: Vec<u32> = out.focus.iter().map(|f| f.ev.0).collect();
        assert_eq!(ids, vec![1, 2, 1], "stable per-session ids, not handles");
        assert!(
            out.focus.iter().all(|f| f.ev.1.is_empty()),
            "no titles by default"
        );
        let mut out2 = RawInput::default();
        let mut s2 = Sampler::new(KeyCapture::Off, true, &[(false, false); 256]);
        let mut f = snap(1);
        f.foreground = Some((0xAAAA, r));
        s2.tick(&f, false, |_| "Editor".into(), &mut out2);
        assert_eq!(out2.focus[0].ev.1, "Editor");
    }

    #[test]
    fn a_panicked_input_thread_is_reported() {
        let h = thread::spawn(|| -> RawInput { panic!("boom") });
        let raw = collect(Some(h));
        assert!(raw.failed);
        assert!(!collect(Some(thread::spawn(RawInput::default))).failed);
    }

    #[test]
    fn sidecar_reader_skips_oversized_lines() {
        let side = crate::test_tmp_dir().join(format!("side-long-{}.jsonl", std::process::id()));
        let long = format!(
            "{{\"k\":\"c\",\"ts\":1,\"x\":1,\"y\":2{}}}\n",
            " ".repeat(MAX_SIDECAR_LINE)
        );
        let ok = "{\"k\":\"c\",\"ts\":2,\"x\":3,\"y\":4}\n";
        std::fs::write(&side, format!("{long}{ok}")).unwrap();
        let back = read_sidecar(&side);
        assert_eq!(back.cursor.len(), 1);
        assert_eq!(back.cursor[0].ev, (3, 4));
        let _ = std::fs::remove_file(&side);
    }

    #[test]
    fn sidecar_never_contains_keys_or_titles() {
        let side = crate::test_tmp_dir().join(format!("side-priv-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&side);
        let mut raw = RawInput::default();
        raw.keys.push(Stamped {
            ts: 1,
            ev: (0x41, Modifiers::default()),
        });
        raw.focus.push(Stamped {
            ts: 1,
            ev: (3, "Secret Title".into(), Rect::default()),
        });
        raw.cursor.push(Stamped { ts: 1, ev: (1, 2) });
        let mut w = SidecarWriter::open(&side).unwrap();
        w.flush(&raw).unwrap();
        let text = std::fs::read_to_string(&side).unwrap();
        assert!(
            !text.contains("Secret") && !text.contains("\"k\":\"k\""),
            "{text}"
        );
        let back = read_sidecar(&side);
        assert_eq!(back.focus.len(), 1);
        assert_eq!(back.focus[0].ev.0, 3);
        assert!(back.keys.is_empty());
        let _ = std::fs::remove_file(&side);
    }

    #[test]
    fn tracks_are_rebased_on_t0() {
        let mut raw = RawInput::default();
        let ms = HNS_PER_MS;
        raw.cursor.push(Stamped {
            ts: 1_000 * ms,
            ev: (1, 1),
        });
        raw.cursor.push(Stamped {
            ts: 2_500 * ms,
            ev: (5, 6),
        });
        raw.clicks.push(Stamped {
            ts: 1_500 * ms,
            ev: (0, 0, MouseButton::Left, ButtonState::Down),
        });
        raw.clicks.push(Stamped {
            ts: 2_600 * ms,
            ev: (5, 6, MouseButton::Left, ButtonState::Down),
        });
        raw.keys.push(Stamped {
            ts: 3_000 * ms,
            ev: (0x41, Modifiers::default()),
        });
        let t = raw.into_tracks(2_000 * ms, &[]);
        assert_eq!(
            t.cursor,
            vec![
                CursorSample {
                    t_ms: 0,
                    x: 1,
                    y: 1
                },
                CursorSample {
                    t_ms: 500,
                    x: 5,
                    y: 6
                }
            ]
        );
        assert_eq!(t.clicks.len(), 1);
        assert_eq!(t.clicks[0].t_ms, 600);
        assert_eq!(t.keys[0].t_ms, 1_000);
        assert_eq!(t.keys[0].label, "A");
    }

    #[test]
    #[ignore = "needs interactive desktop"]
    fn recorder_runs_and_stops() {
        let side = crate::test_tmp_dir().join(format!("input-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&side);
        let mut rec = InputRecorder::start(
            SourceOrigin::Fixed(Rect::default()),
            InputOptions {
                hz: 120,
                keys: KeyCapture::Off,
                sidecar: Some(side.clone()),
                titles: false,
                timeline: None,
                scale: 1.0,
            },
        )
        .unwrap();
        thread::sleep(Duration::from_millis(250));
        let raw = rec.stop();
        // The sidecar holds at least the initial cursor sample and focus.
        let back = read_sidecar(&side);
        assert_eq!(back.cursor.len(), raw.cursor.len());
        assert_eq!(back.focus.len(), raw.focus.len());
        let _ = std::fs::remove_file(&side);
        // ~30 ticks expected at 120 Hz; allow wide tolerance for loaded CI machines.
        assert!(raw.ticks >= 10 && raw.ticks <= 60, "ticks = {}", raw.ticks);
    }
}
