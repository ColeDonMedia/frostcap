//! Keep Frostcap's own windows out of recordings with
//! `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`.
//!
//! [`ExcludeGuard`] excludes a set of windows while it lives and restores each one
//! to exactly the affinity it had before. The Win32 calls sit behind
//! [`AffinityApi`] so the bookkeeping is unit tested with a fake.

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowDisplayAffinity, GetWindowThreadProcessId, IsWindowVisible,
    SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WINDOW_DISPLAY_AFFINITY,
};
use windows::core::BOOL;

/// `WDA_NONE`.
pub const AFFINITY_NONE: u32 = 0;
/// `WDA_EXCLUDEFROMCAPTURE`.
pub const AFFINITY_EXCLUDE: u32 = 0x11;

/// Reads and sets the display affinity of one window.
pub trait AffinityApi {
    /// Current affinity (`None` if it cannot be read).
    fn get(&mut self, hwnd: isize) -> Option<u32>;
    /// Set the affinity. Returns whether it worked.
    fn set(&mut self, hwnd: isize, affinity: u32) -> bool;
}

/// The real Win32 implementation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Win32Affinity;

impl AffinityApi for Win32Affinity {
    fn get(&mut self, hwnd: isize) -> Option<u32> {
        let mut a = 0u32;
        // SAFETY: valid out pointer; an invalid handle just fails.
        unsafe { GetWindowDisplayAffinity(HWND(hwnd as *mut _), &mut a) }
            .ok()
            .map(|()| a)
    }

    fn set(&mut self, hwnd: isize, affinity: u32) -> bool {
        // SAFETY: plain call; an invalid handle just fails.
        unsafe { SetWindowDisplayAffinity(HWND(hwnd as *mut _), WINDOW_DISPLAY_AFFINITY(affinity)) }
            .is_ok()
    }
}

const _: () = {
    assert!(WDA_NONE.0 == AFFINITY_NONE);
    assert!(WDA_EXCLUDEFROMCAPTURE.0 == AFFINITY_EXCLUDE);
};

/// Current affinity of `hwnd` (`None` if the query fails).
pub fn is_excluded(hwnd: isize) -> Option<bool> {
    Win32Affinity.get(hwnd).map(|a| a == AFFINITY_EXCLUDE)
}

/// Excludes windows from capture until dropped.
pub struct ExcludeGuard<A: AffinityApi = Win32Affinity> {
    api: A,
    /// `(window, affinity it had before)` for every window we changed.
    applied: Vec<(isize, u32)>,
}

impl<A: AffinityApi> ExcludeGuard<A> {
    /// Exclude every distinct non-null `hwnd` when `enabled`. A window that is already
    /// excluded is left alone (and not restored); failures are skipped.
    pub fn apply(mut api: A, hwnds: &[isize], enabled: bool) -> Self {
        let mut applied: Vec<(isize, u32)> = Vec::new();
        if enabled {
            for &h in hwnds {
                if h == 0 || applied.iter().any(|(a, _)| *a == h) {
                    continue;
                }
                let before = api.get(h).unwrap_or(AFFINITY_NONE);
                if before == AFFINITY_EXCLUDE {
                    continue;
                }
                if api.set(h, AFFINITY_EXCLUDE) {
                    applied.push((h, before));
                }
            }
        }
        Self { api, applied }
    }

    /// Windows that are currently excluded by this guard.
    pub fn applied(&self) -> Vec<isize> {
        self.applied.iter().map(|(h, _)| *h).collect()
    }
}

impl<A: AffinityApi> Drop for ExcludeGuard<A> {
    fn drop(&mut self) {
        for &(h, before) in self.applied.iter().rev() {
            self.api.set(h, before);
        }
    }
}

/// The windows to hide: `own` minus the window being recorded (excluding the very
/// window a user records would give a black video).
pub fn windows_to_exclude(own: &[isize], recorded_window: Option<isize>) -> Vec<isize> {
    own.iter()
        .copied()
        .filter(|h| Some(*h) != recorded_window)
        .collect()
}

/// Every visible top-level window of this process (main window, overlays, toasts).
pub fn own_visible_windows() -> Vec<isize> {
    extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the address of the Vec passed below; hwnd is valid in the callback.
        unsafe {
            let out = &mut *(lparam.0 as *mut Vec<isize>);
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == GetCurrentProcessId() && IsWindowVisible(hwnd).as_bool() {
                out.push(hwnd.0 as isize);
            }
        }
        true.into()
    }
    let mut out: Vec<isize> = Vec::new();
    // SAFETY: the callback only runs during this call; `out` outlives it.
    let _ = unsafe { EnumWindows(Some(each), LPARAM((&raw mut out) as isize)) };
    out
}

/// Exclude Frostcap's own windows (app window, overlays) while recording.
pub fn exclude_own_windows(hwnds: &[isize], enabled: bool) -> ExcludeGuard {
    ExcludeGuard::apply(Win32Affinity, hwnds, enabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Fake {
        state: HashMap<isize, u32>,
        calls: Vec<(isize, u32)>,
        fail_set: Vec<isize>,
    }
    impl AffinityApi for &mut Fake {
        fn get(&mut self, hwnd: isize) -> Option<u32> {
            self.state.get(&hwnd).copied()
        }
        fn set(&mut self, hwnd: isize, affinity: u32) -> bool {
            self.calls.push((hwnd, affinity));
            if self.fail_set.contains(&hwnd) {
                return false;
            }
            self.state.insert(hwnd, affinity);
            true
        }
    }

    #[test]
    fn excludes_on_apply_and_restores_on_drop() {
        let mut f = Fake::default();
        {
            let g = ExcludeGuard::apply(&mut f, &[1, 2], true);
            assert_eq!(g.applied(), [1, 2]);
        }
        assert_eq!(
            f.calls,
            [
                (1, AFFINITY_EXCLUDE),
                (2, AFFINITY_EXCLUDE),
                (2, AFFINITY_NONE),
                (1, AFFINITY_NONE)
            ]
        );
    }

    #[test]
    fn restores_exactly_the_previous_affinity() {
        // WDA_MONITOR (1) was set by something else: it must come back as 1, not 0.
        let mut f = Fake::default();
        f.state.insert(7, 1);
        drop(ExcludeGuard::apply(&mut f, &[7], true));
        assert_eq!(f.state[&7], 1);
    }

    #[test]
    fn an_already_excluded_window_is_left_alone() {
        let mut f = Fake::default();
        f.state.insert(9, AFFINITY_EXCLUDE);
        drop(ExcludeGuard::apply(&mut f, &[9], true));
        assert!(f.calls.is_empty());
        assert_eq!(
            f.state[&9], AFFINITY_EXCLUDE,
            "not turned off behind its owner's back"
        );
    }

    #[test]
    fn disabled_setting_touches_nothing() {
        let mut f = Fake::default();
        drop(ExcludeGuard::apply(&mut f, &[1, 2], false));
        assert!(f.calls.is_empty());
    }

    #[test]
    fn only_successfully_excluded_windows_are_restored_and_duplicates_skipped() {
        let mut f = Fake {
            fail_set: vec![2],
            ..Default::default()
        };
        {
            let g = ExcludeGuard::apply(&mut f, &[1, 2, 1, 0], true);
            assert_eq!(g.applied(), [1]);
        }
        assert_eq!(
            f.calls,
            [
                (1, AFFINITY_EXCLUDE),
                (2, AFFINITY_EXCLUDE),
                (1, AFFINITY_NONE)
            ]
        );
    }

    #[test]
    fn the_recorded_window_is_never_excluded() {
        assert_eq!(windows_to_exclude(&[1, 2, 3], Some(2)), [1, 3]);
        assert_eq!(windows_to_exclude(&[1, 2], None), [1, 2]);
    }

    #[test]
    #[ignore = "needs a desktop"]
    fn this_process_windows_can_be_enumerated() {
        // A test process usually has no visible window; the call must simply work.
        let _ = own_visible_windows();
    }
}
