//! Capture source enumeration (monitors, windows, regions) and resolution.
//!
//! `list_sources` returns both the detailed lists (`monitors`, `windows`) and a
//! UI-shaped flat list (`sources`: `{id, kind, name, width, height, thumbnail,
//! source}`). `start_recording` accepts either the tagged [`CaptureSource`] or the
//! flat `id` string (see [`CaptureSource::from_id`]).

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow,
};
use windows_capture::monitor::Monitor;
use windows_capture::window::Window;

use crate::error::{Error, Result};
use crate::project::Rect;
use crate::screen::ScreenTarget;

/// `MONITORINFOF_PRIMARY` (winuser.h).
const MONITORINFOF_PRIMARY: u32 = 1;

/// A monitor that can be recorded.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    /// Stable id (GDI device name, e.g. `\\.\DISPLAY1`).
    pub id: String,
    /// Friendly name ("DELL U2720Q"), falls back to the device name.
    pub name: String,
    /// Bounds on the virtual desktop in physical pixels (may be negative).
    pub bounds: Rect,
    /// Refresh rate in Hz (0 if unknown).
    pub refresh_rate: u32,
    /// DPI scale (1.0 = 96 DPI).
    pub scale_factor: f64,
    /// Primary monitor.
    pub primary: bool,
}

/// A window that can be recorded.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    /// Window handle as an integer.
    pub hwnd: u64,
    /// Owning process id (validated again at record time).
    pub pid: u32,
    /// Title.
    pub title: String,
    /// Executable name (e.g. `Code.exe`), empty if unavailable.
    pub process_name: String,
    /// Visible bounds on the virtual desktop (physical px, DWM extended frame).
    pub bounds: Rect,
}

/// Kind of a flat source item.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SourceItemKind {
    /// A monitor.
    Display,
    /// A window.
    Window,
    /// Every monitor as one canvas.
    AllDisplays,
    /// A region of a monitor.
    Region,
}

/// UI-shaped source entry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceItem {
    /// Flat id, e.g. `display:\\.\DISPLAY1` or `window:65812`.
    pub id: String,
    /// Kind.
    pub kind: SourceItemKind,
    /// Display name.
    pub name: String,
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
    /// `data:image/png;base64,...` preview (max 320 px wide), if captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
    /// The tagged source to pass back to `start_recording`.
    pub source: CaptureSource,
}

/// Result of [`list_sources`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SourceList {
    /// Monitors, primary first.
    pub monitors: Vec<MonitorInfo>,
    /// Capturable top-level windows (Frostcap's own windows excluded).
    pub windows: Vec<WindowInfo>,
    /// Flat UI list (displays first, then windows).
    pub sources: Vec<SourceItem>,
}

/// Normalized rectangle, all values `0..=1` relative to a monitor.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NormRect {
    /// Left.
    pub x: f64,
    /// Top.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl NormRect {
    /// Convert to monitor pixels (even size, clamped inside the monitor, min 16 px).
    pub fn to_pixels(&self, w: i32, h: i32) -> Result<Rect> {
        let vals = [self.x, self.y, self.width, self.height];
        if vals.iter().any(|v| !v.is_finite() || *v < 0.0 || *v > 1.0) {
            return Err(Error::Invalid("region values must be within 0..1".into()));
        }
        let x = ((self.x * f64::from(w)).round() as i32).clamp(0, w);
        let y = ((self.y * f64::from(h)).round() as i32).clamp(0, h);
        let rw = ((self.width * f64::from(w)).round() as i32).min(w - x) & !1;
        let rh = ((self.height * f64::from(h)).round() as i32).min(h - y) & !1;
        if rw < 16 || rh < 16 {
            return Err(Error::Invalid("region is too small (min 16x16 px)".into()));
        }
        Ok(Rect {
            x,
            y,
            width: rw,
            height: rh,
        })
    }
}

/// What to record (sent by the UI).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CaptureSource {
    /// The primary monitor.
    PrimaryMonitor,
    /// A monitor by [`MonitorInfo::id`].
    Monitor {
        /// Device name.
        id: String,
    },
    /// A window by [`WindowInfo::hwnd`]; `pid`/`title` (if given) must still match.
    Window {
        /// Window handle.
        hwnd: u64,
        /// Expected owning process id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
    },
    /// Every monitor, recorded as one canvas (the union of their virtual-desktop bounds).
    AllDisplays,
    /// A rectangle of a monitor.
    #[serde(rename_all = "camelCase")]
    Region {
        /// Monitor device name.
        monitor_id: String,
        /// Rectangle normalized to that monitor.
        rect: NormRect,
    },
}

impl CaptureSource {
    /// Parse a flat id from [`SourceItem::id`].
    pub fn from_id(id: &str) -> Result<Self> {
        if let Some(m) = id.strip_prefix("display:") {
            return Ok(CaptureSource::Monitor { id: m.to_owned() });
        }
        if let Some(h) = id.strip_prefix("window:") {
            let hwnd = h
                .parse()
                .map_err(|_| Error::Invalid(format!("bad window id {id:?}")))?;
            return Ok(CaptureSource::Window { hwnd, pid: None });
        }
        if id == "all-displays" {
            return Ok(CaptureSource::AllDisplays);
        }
        if id == "primary" {
            return Ok(CaptureSource::PrimaryMonitor);
        }
        Err(Error::Invalid(format!("unknown source id {id:?}")))
    }
}

pub(crate) fn rect_from(r: RECT) -> Rect {
    Rect {
        x: r.left,
        y: r.top,
        width: r.right - r.left,
        height: r.bottom - r.top,
    }
}

pub(crate) fn monitor_rect(m: &Monitor) -> Option<(Rect, bool)> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: handle comes from EnumDisplayMonitors; info is a valid out param.
    let ok = unsafe { GetMonitorInfoW(HMONITOR(m.as_raw_hmonitor()), &mut info) }.as_bool();
    ok.then(|| {
        (
            rect_from(info.rcMonitor),
            info.dwFlags & MONITORINFOF_PRIMARY != 0,
        )
    })
}

/// Visible window bounds (DWM extended frame, without the invisible resize border).
pub(crate) fn window_rect(hwnd: HWND) -> Option<Rect> {
    let mut r = RECT::default();
    // SAFETY: valid out buffer of the documented size.
    let dwm = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut r).cast(),
            size_of::<RECT>() as u32,
        )
    };
    if dwm.is_err() {
        // SAFETY: valid out param.
        unsafe { GetWindowRect(hwnd, &mut r) }.ok()?;
    }
    Some(rect_from(r))
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: valid out buffer.
    let r = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            size_of::<u32>() as u32,
        )
    };
    r.is_ok() && cloaked != 0
}

fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: valid out param.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

fn scale_of(m: &Monitor) -> f64 {
    let (mut x, mut y) = (0u32, 0u32);
    // SAFETY: valid monitor handle and out params.
    match unsafe {
        GetDpiForMonitor(
            HMONITOR(m.as_raw_hmonitor()),
            MDT_EFFECTIVE_DPI,
            &mut x,
            &mut y,
        )
    } {
        Ok(()) if x > 0 => f64::from(x) / 96.0,
        _ => 1.0,
    }
}

fn monitor_name(m: &Monitor) -> String {
    m.name()
        .ok()
        .filter(|n| !n.is_empty())
        .or_else(|| m.device_name().ok())
        .unwrap_or_default()
}

/// Enumerate monitors (primary first).
pub fn list_monitors() -> Result<Vec<MonitorInfo>> {
    let monitors = Monitor::enumerate().map_err(|e| Error::Capture(e.to_string()))?;
    let mut out: Vec<MonitorInfo> = monitors
        .iter()
        .filter_map(|m| {
            let (bounds, primary) = monitor_rect(m)?;
            let id = m.device_name().ok()?;
            Some(MonitorInfo {
                name: monitor_name(m),
                id,
                bounds,
                refresh_rate: m.refresh_rate().unwrap_or(0),
                scale_factor: scale_of(m),
                primary,
            })
        })
        .collect();
    out.sort_by_key(|m| (!m.primary, m.bounds.x, m.bounds.y));
    Ok(out)
}

/// Enumerate capturable windows (visible, titled, not minimized/cloaked, not ours).
pub fn list_windows() -> Result<Vec<WindowInfo>> {
    // SAFETY: trivial.
    let own_pid = unsafe { GetCurrentProcessId() };
    let windows = Window::enumerate().map_err(|e| Error::Capture(e.to_string()))?;
    Ok(windows
        .iter()
        .filter_map(|w| {
            let hwnd = HWND(w.as_raw_hwnd());
            // SAFETY: plain queries on a window handle.
            if unsafe { IsIconic(hwnd) }.as_bool() || is_cloaked(hwnd) {
                return None;
            }
            let pid = window_pid(hwnd);
            if pid == own_pid {
                return None;
            }
            let title = w.title().ok().filter(|t| !t.trim().is_empty())?;
            let bounds = window_rect(hwnd).filter(|r| r.width > 1 && r.height > 1)?;
            Some(WindowInfo {
                hwnd: hwnd.0 as usize as u64,
                pid,
                title,
                process_name: w.process_name().unwrap_or_default(),
                bounds,
            })
        })
        .collect())
}

/// The flat "All displays" entry for these monitor bounds, if there are 2+ monitors
/// whose union can be laid out (see [`crate::canvas::layout`]).
pub(crate) fn all_displays_item(bounds: &[Rect]) -> Option<SourceItem> {
    if bounds.len() < 2 {
        return None;
    }
    let l = crate::canvas::layout(bounds).ok()?;
    Some(SourceItem {
        id: "all-displays".into(),
        kind: SourceItemKind::AllDisplays,
        name: "All displays".into(),
        width: l.bounds.width as u32,
        height: l.bounds.height as u32,
        thumbnail: None,
        source: CaptureSource::AllDisplays,
    })
}

/// Enumerate monitors and windows. With `thumbnails`, each flat item gets a
/// small PNG preview captured with a single WGC frame (bounded total time).
pub fn list_sources(thumbnails: bool) -> Result<SourceList> {
    let monitors = list_monitors()?;
    let windows = list_windows()?;
    let mut sources: Vec<SourceItem> = monitors
        .iter()
        .map(|m| SourceItem {
            id: format!("display:{}", m.id),
            kind: SourceItemKind::Display,
            name: m.name.clone(),
            width: m.bounds.width.max(0) as u32,
            height: m.bounds.height.max(0) as u32,
            thumbnail: None,
            source: CaptureSource::Monitor { id: m.id.clone() },
        })
        .chain(all_displays_item(
            &monitors.iter().map(|m| m.bounds).collect::<Vec<_>>(),
        ))
        .chain(windows.iter().map(|w| SourceItem {
            id: format!("window:{}", w.hwnd),
            kind: SourceItemKind::Window,
            name: w.title.clone(),
            width: w.bounds.width.max(0) as u32,
            height: w.bounds.height.max(0) as u32,
            thumbnail: None,
            source: CaptureSource::Window {
                hwnd: w.hwnd,
                pid: Some(w.pid),
            },
        }))
        .collect();
    if thumbnails {
        let targets: Vec<Option<ScreenTarget>> = sources
            .iter()
            .map(|s| {
                resolve(&s.source)
                    .ok()
                    .map(|r| r.target)
                    .filter(|t| !matches!(t, ScreenTarget::AllDisplays))
            })
            .collect();
        let thumbs = crate::thumb::capture_thumbnails(&targets, 320);
        for (s, t) in sources.iter_mut().zip(thumbs) {
            s.thumbnail = t;
        }
    }
    Ok(SourceList {
        monitors,
        windows,
        sources,
    })
}

/// A source resolved to a live capture target.
#[derive(Clone, Debug)]
pub(crate) struct ResolvedSource {
    pub kind: crate::project::SourceKind,
    pub target: ScreenTarget,
    pub name: String,
    /// Bounds of the monitor/window on the virtual desktop.
    pub bounds: Rect,
    /// Crop inside the source (region capture), source pixels.
    pub crop: Option<Rect>,
    /// Monitors of an all-displays capture and their canvas layout.
    pub all: Option<(Vec<crate::screen::MonitorSrc>, crate::canvas::Layout)>,
}

fn find_monitor(id: &str) -> Result<Monitor> {
    Monitor::enumerate()
        .map_err(|e| Error::Capture(e.to_string()))?
        .into_iter()
        .find(|m| m.device_name().is_ok_and(|n| n == id))
        .ok_or_else(|| Error::SourceNotFound(format!("monitor {id}")))
}

/// Raw monitor handle of the monitor with GDI device name `id`.
pub fn monitor_handle(id: &str) -> Option<usize> {
    find_monitor(id).ok().map(|m| m.as_raw_hmonitor() as usize)
}

fn resolve_monitor(m: &Monitor, crop: Option<Rect>) -> Result<ResolvedSource> {
    use crate::project::SourceKind;
    let (bounds, _) = monitor_rect(m).ok_or_else(|| Error::SourceNotFound("monitor".into()))?;
    Ok(ResolvedSource {
        kind: if crop.is_some() {
            SourceKind::Region
        } else {
            SourceKind::Monitor
        },
        target: ScreenTarget::Monitor(m.as_raw_hmonitor() as usize),
        name: monitor_name(m),
        bounds,
        crop,
        all: None,
    })
}

pub(crate) fn resolve(source: &CaptureSource) -> Result<ResolvedSource> {
    match source {
        CaptureSource::AllDisplays => {
            let mut monitors = list_monitors()?;
            // Smoke-test hook: record only the first N monitors (3 monitors exceed the encoder limit).
            if let Some(n) = std::env::var("FROSTCAP_ALL_DISPLAYS_COUNT")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
            {
                monitors.truncate(n.max(1));
            }
            let bounds: Vec<Rect> = monitors.iter().map(|m| m.bounds).collect();
            let layout = crate::canvas::layout(&bounds)?;
            let srcs = monitors
                .iter()
                .map(|m| {
                    monitor_handle(&m.id)
                        .map(|handle| crate::screen::MonitorSrc {
                            handle,
                            bounds: m.bounds,
                        })
                        .ok_or_else(|| Error::SourceNotFound(format!("monitor {}", m.id)))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(ResolvedSource {
                kind: crate::project::SourceKind::Monitor,
                target: ScreenTarget::AllDisplays,
                name: "All displays".into(),
                bounds: layout.bounds,
                crop: None,
                all: Some((srcs, layout)),
            })
        }
        CaptureSource::PrimaryMonitor => {
            let m = Monitor::primary().map_err(|e| Error::SourceNotFound(e.to_string()))?;
            resolve_monitor(&m, None)
        }
        CaptureSource::Monitor { id } => resolve_monitor(&find_monitor(id)?, None),
        CaptureSource::Region { monitor_id, rect } => {
            let m = find_monitor(monitor_id)?;
            let (b, _) =
                monitor_rect(&m).ok_or_else(|| Error::SourceNotFound(monitor_id.clone()))?;
            let crop = rect.to_pixels(b.width, b.height)?;
            resolve_monitor(&m, Some(crop))
        }
        CaptureSource::Window { hwnd, pid } => {
            let h = HWND(*hwnd as usize as *mut std::ffi::c_void);
            // SAFETY: IsWindow accepts any value.
            if !unsafe { IsWindow(Some(h)) }.as_bool() {
                return Err(Error::SourceNotFound(format!(
                    "window {hwnd} no longer exists"
                )));
            }
            let actual_pid = window_pid(h);
            // SAFETY: trivial.
            if actual_pid == unsafe { GetCurrentProcessId() } {
                return Err(Error::Invalid("cannot record Frostcap's own window".into()));
            }
            if pid.is_some_and(|p| p != actual_pid) {
                return Err(Error::SourceNotFound(format!(
                    "window {hwnd} now belongs to another process"
                )));
            }
            // SAFETY: plain query.
            if unsafe { IsIconic(h) }.as_bool() {
                return Err(Error::SourceNotFound(
                    "the window is minimized; restore it first".into(),
                ));
            }
            let window = Window::from_raw_hwnd(h.0);
            let bounds =
                window_rect(h).ok_or_else(|| Error::SourceNotFound(format!("window {hwnd}")))?;
            Ok(ResolvedSource {
                kind: crate::project::SourceKind::Window,
                target: ScreenTarget::Window(h.0 as usize),
                name: window.title().unwrap_or_default(),
                bounds,
                crop: None,
                all: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_roundtrip() {
        assert_eq!(
            CaptureSource::from_id("display:\\\\.\\DISPLAY1").unwrap(),
            CaptureSource::Monitor {
                id: "\\\\.\\DISPLAY1".into()
            }
        );
        assert_eq!(
            CaptureSource::from_id("window:42").unwrap(),
            CaptureSource::Window {
                hwnd: 42,
                pid: None
            }
        );
        assert!(CaptureSource::from_id("nope").is_err());
    }

    #[test]
    fn all_displays_source_ids_and_wire_format() {
        assert_eq!(
            CaptureSource::from_id("all-displays").unwrap(),
            CaptureSource::AllDisplays
        );
        let s = serde_json::to_string(&CaptureSource::AllDisplays).unwrap();
        assert_eq!(s, r#"{"kind":"allDisplays"}"#);
        let k = serde_json::to_string(&SourceItemKind::AllDisplays).unwrap();
        assert_eq!(k, "\"allDisplays\"");
        let item = all_displays_item(&[
            Rect {
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
            },
            Rect {
                x: -1920,
                y: 0,
                width: 1920,
                height: 1080,
            },
        ])
        .unwrap();
        assert_eq!(
            (item.id.as_str(), item.width, item.height),
            ("all-displays", 4480, 1440)
        );
        assert!(
            all_displays_item(&[Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 1
            }])
            .is_none()
        );
    }

    #[test]
    fn region_to_pixels() {
        let r = NormRect {
            x: 0.25,
            y: 0.5,
            width: 0.5,
            height: 0.5,
        }
        .to_pixels(1920, 1080)
        .unwrap();
        assert_eq!(
            r,
            Rect {
                x: 480,
                y: 540,
                width: 960,
                height: 540
            }
        );
        assert!(
            NormRect {
                x: 0.0,
                y: 0.0,
                width: 0.001,
                height: 0.5
            }
            .to_pixels(1920, 1080)
            .is_err()
        );
        assert!(
            NormRect {
                x: f64::NAN,
                y: 0.0,
                width: 0.5,
                height: 0.5
            }
            .to_pixels(1920, 1080)
            .is_err()
        );
        // Clamped at the right edge and forced even.
        let r = NormRect {
            x: 0.9,
            y: 0.0,
            width: 0.5,
            height: 0.333,
        }
        .to_pixels(1001, 999)
        .unwrap();
        assert!(r.x + r.width <= 1001 && r.width % 2 == 0 && r.height % 2 == 0);
    }

    #[test]
    fn tagged_wire_format() {
        let s = serde_json::to_string(&CaptureSource::Region {
            monitor_id: "\\\\.\\DISPLAY2".into(),
            rect: NormRect {
                x: 0.1,
                y: 0.1,
                width: 0.5,
                height: 0.5,
            },
        })
        .unwrap();
        assert!(s.contains("\"kind\":\"region\"") && s.contains("\"monitorId\""));
    }

    #[test]
    #[ignore = "needs interactive desktop"]
    fn enumerates_live() {
        let list = list_sources(false).unwrap();
        assert!(!list.monitors.is_empty());
        assert!(list.monitors[0].primary);
        // Displays and windows, plus the "All displays" entry when there are 2+ monitors.
        let bounds: Vec<Rect> = list.monitors.iter().map(|m| m.bounds).collect();
        let all = usize::from(all_displays_item(&bounds).is_some());
        assert_eq!(
            list.sources.len(),
            list.monitors.len() + list.windows.len() + all
        );
    }
}
