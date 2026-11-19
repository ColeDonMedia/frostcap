//! Interactive region picker: a dimmed, topmost, layered Win32 overlay covering
//! the whole virtual desktop. Drag to select; Esc or right-click cancels. The
//! result is clamped to the monitor under the selection centre and returned both
//! normalized to that monitor (for [`crate::sources::CaptureSource::Region`]) and
//! in absolute physical pixels.

use std::cell::RefCell;

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BLACK_BRUSH, BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, FrameRect,
    GetStockObject, HBRUSH, InvalidateRect, MONITOR_DEFAULTTONEAREST, MonitorFromPoint,
    PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetMessageW, GetSystemMetrics, IDC_CROSS, LWA_ALPHA, LoadCursorW, MSG, PostQuitMessage,
    RegisterClassW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SW_SHOW, SetForegroundWindow, SetLayeredWindowAttributes, ShowWindow, TranslateMessage,
    WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONDOWN,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::w;

use crate::error::{Error, Result};
use crate::project::Rect;
use crate::sources::{NormRect, list_monitors};

/// Result of [`pick_region`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RegionSelection {
    /// Monitor device name.
    pub monitor_id: String,
    /// Rectangle normalized to that monitor.
    pub rect: NormRect,
    /// Absolute rectangle on the virtual desktop (physical px).
    pub screen_rect: Rect,
}

#[derive(Default)]
struct PickState {
    origin: (i32, i32),
    start: Option<POINT>,
    current: POINT,
    done: Option<Option<RECT>>,
}

thread_local! {
    static STATE: RefCell<PickState> = RefCell::new(PickState::default());
}

fn lparam_point(l: LPARAM) -> POINT {
    POINT {
        x: (l.0 & 0xFFFF) as i16 as i32,
        y: ((l.0 >> 16) & 0xFFFF) as i16 as i32,
    }
}

fn sel_rect(a: POINT, b: POINT) -> RECT {
    RECT {
        left: a.x.min(b.x),
        top: a.y.min(b.y),
        right: a.x.max(b.x),
        bottom: a.y.max(b.y),
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: standard window procedure; only touches thread-local state.
    unsafe {
        match msg {
            WM_LBUTTONDOWN => {
                let p = lparam_point(lp);
                STATE.with_borrow_mut(|s| {
                    s.start = Some(p);
                    s.current = p;
                });
                SetCapture(hwnd);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let p = lparam_point(lp);
                let dragging = STATE.with_borrow_mut(|s| {
                    s.current = p;
                    s.start.is_some()
                });
                if dragging {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let _ = ReleaseCapture();
                let p = lparam_point(lp);
                STATE.with_borrow_mut(|s| {
                    let r = s.start.map(|a| sel_rect(a, p));
                    s.done = Some(r.filter(|r| r.right - r.left >= 16 && r.bottom - r.top >= 16));
                });
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_RBUTTONDOWN => {
                STATE.with_borrow_mut(|s| s.done = Some(None));
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_KEYDOWN if wp.0 == usize::from(VK_ESCAPE.0) => {
                STATE.with_borrow_mut(|s| s.done = Some(None));
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let full = RECT {
                    left: 0,
                    top: 0,
                    right: 1 << 15,
                    bottom: 1 << 15,
                };
                FillRect(hdc, &full, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                let sel = STATE.with_borrow(|s| s.start.map(|a| sel_rect(a, s.current)));
                if let Some(r) = sel {
                    let white = CreateSolidBrush(COLORREF(0x00FF_FFFF));
                    for i in 0..3 {
                        let rr = RECT {
                            left: r.left - i,
                            top: r.top - i,
                            right: r.right + i,
                            bottom: r.bottom + i,
                        };
                        FrameRect(hdc, &rr, white);
                    }
                    let _ = DeleteObject(white.into());
                }
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn run_overlay() -> Result<Option<RECT>> {
    // SAFETY: plain Win32 window creation + message loop on this thread.
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let class = w!("FrostcapRegionPicker");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_CROSS)?,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc); // fails harmlessly if already registered
        let (x, y) = (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
        );
        let (w, h) = (
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        );
        STATE.with_borrow_mut(|s| {
            *s = PickState {
                origin: (x, y),
                ..Default::default()
            }
        });
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!("Select region"),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 110, LWA_ALPHA)?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let (origin, done) = STATE.with_borrow(|s| (s.origin, s.done));
        Ok(done.flatten().map(|r| RECT {
            left: r.left + origin.0,
            top: r.top + origin.1,
            right: r.right + origin.0,
            bottom: r.bottom + origin.1,
        }))
    }
}

/// Convert an absolute rectangle to a selection on the monitor under its centre.
pub fn selection_from_screen_rect(abs: Rect) -> Result<RegionSelection> {
    let center = POINT {
        x: abs.x + abs.width / 2,
        y: abs.y + abs.height / 2,
    };
    // SAFETY: plain query.
    let hmon = unsafe { MonitorFromPoint(center, MONITOR_DEFAULTTONEAREST) };
    let monitors = list_monitors()?;
    let m = monitors
        .iter()
        .find(|m| {
            let b = m.bounds;
            center.x >= b.x
                && center.x < b.x + b.width
                && center.y >= b.y
                && center.y < b.y + b.height
        })
        .or_else(|| monitors.first())
        .ok_or_else(|| Error::SourceNotFound(format!("no monitor near selection ({hmon:?})")))?;
    let b = m.bounds;
    let l = abs.x.clamp(b.x, b.x + b.width);
    let t = abs.y.clamp(b.y, b.y + b.height);
    let r = (abs.x + abs.width).clamp(b.x, b.x + b.width);
    let btm = (abs.y + abs.height).clamp(b.y, b.y + b.height);
    let (bw, bh) = (f64::from(b.width.max(1)), f64::from(b.height.max(1)));
    Ok(RegionSelection {
        monitor_id: m.id.clone(),
        rect: NormRect {
            x: f64::from(l - b.x) / bw,
            y: f64::from(t - b.y) / bh,
            width: f64::from(r - l) / bw,
            height: f64::from(btm - t) / bh,
        },
        screen_rect: Rect {
            x: l,
            y: t,
            width: r - l,
            height: btm - t,
        },
    })
}

/// Show the overlay and wait for the user. `Ok(None)` when cancelled.
pub fn pick_region() -> Result<Option<RegionSelection>> {
    let abs = std::thread::Builder::new()
        .name("frostcap-region".into())
        .spawn(run_overlay)
        .map_err(|e| Error::Invalid(e.to_string()))?
        .join()
        .map_err(|_| Error::Invalid("region picker panicked".into()))??;
    abs.map(|r| {
        selection_from_screen_rect(Rect {
            x: r.left,
            y: r.top,
            width: r.right - r.left,
            height: r.bottom - r.top,
        })
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs interactive desktop"]
    fn selection_is_normalized_to_its_monitor() {
        let m = &list_monitors().unwrap()[0];
        let b = m.bounds;
        let s = selection_from_screen_rect(Rect {
            x: b.x + b.width / 4,
            y: b.y + b.height / 4,
            width: b.width / 2,
            height: b.height / 2,
        })
        .unwrap();
        assert_eq!(s.monitor_id, m.id);
        assert!((s.rect.x - 0.25).abs() < 0.01 && (s.rect.width - 0.5).abs() < 0.01);
        // Overhanging selections are clamped to the monitor.
        let s = selection_from_screen_rect(Rect {
            x: b.x + b.width - 100,
            y: b.y + 10,
            width: 150,
            height: 100,
        })
        .unwrap();
        assert!(s.rect.x + s.rect.width <= 1.0 + 1e-9);
    }
}
