//! Global hotkeys via `RegisterHotKey` on a dedicated message thread (no plugin).
//!
//! Defaults are deliberately uncommon chords so other apps keep theirs:
//! * Ctrl+Alt+Shift+R: start / stop recording
//! * Ctrl+Alt+Shift+P: pause / resume recording
//!
//! Bindings are configurable ([`Bindings`], chord strings like `"Ctrl+Shift+F9"`).

use std::fmt;
use std::str::FromStr;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_HOTKEY, WM_QUIT, WM_USER,
};

use crate::error::{Error, Result};

/// What a hotkey asks for.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HotkeyAction {
    /// Start / stop.
    StartStop,
    /// Pause / resume.
    PauseResume,
}

impl HotkeyAction {
    const ALL: [HotkeyAction; 2] = [HotkeyAction::StartStop, HotkeyAction::PauseResume];

    /// RegisterHotKey id.
    pub fn id(self) -> i32 {
        match self {
            HotkeyAction::StartStop => 0x0046_4301,
            HotkeyAction::PauseResume => 0x0046_4302,
        }
    }

    /// Action for a WM_HOTKEY id.
    pub fn from_id(id: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.id() == id)
    }
}

/// A key chord: modifiers (MOD_* bits) + virtual key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord {
    /// MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_WIN bits.
    pub mods: u32,
    /// Virtual-key code (A-Z, 0-9, F1-F24).
    pub vk: u32,
}

impl FromStr for Chord {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        let bad = || {
            Error::Invalid(format!(
                "invalid hotkey {s:?}: use two or more modifiers, e.g. Ctrl+Alt+Shift+R (Ctrl+Alt alone is AltGr)"
            ))
        };
        let parts: Vec<String> = s
            .split('+')
            .map(|p| p.trim().to_ascii_uppercase())
            .collect();
        let (key, mods) = parts.split_last().ok_or_else(bad)?;
        let mut m = 0u32;
        for p in mods {
            let bit = match p.as_str() {
                "CTRL" | "CONTROL" => MOD_CONTROL.0,
                "ALT" => MOD_ALT.0,
                "SHIFT" => MOD_SHIFT.0,
                "WIN" | "SUPER" => MOD_WIN.0,
                _ => return Err(bad()),
            };
            if m & bit != 0 {
                return Err(bad());
            }
            m |= bit;
        }
        // Require Ctrl or Win so plain typing / Alt menus are never hijacked, at least
        // two modifiers, and never Ctrl+Alt alone (that is AltGr on many layouts).
        if m & (MOD_CONTROL.0 | MOD_WIN.0) == 0
            || m.count_ones() < 2
            || m == (MOD_CONTROL.0 | MOD_ALT.0)
        {
            return Err(bad());
        }
        let vk = match key.as_bytes() {
            [c] if c.is_ascii_uppercase() || c.is_ascii_digit() => u32::from(*c),
            [b'F', rest @ ..] if !rest.is_empty() => match std::str::from_utf8(rest)
                .ok()
                .and_then(|n| n.parse::<u32>().ok())
            {
                Some(n @ 1..=24) => 0x6F + n,
                _ => return Err(bad()),
            },
            _ => return Err(bad()),
        };
        Ok(Chord { mods: m, vk })
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (bit, name) in [
            (MOD_CONTROL.0, "Ctrl"),
            (MOD_ALT.0, "Alt"),
            (MOD_SHIFT.0, "Shift"),
            (MOD_WIN.0, "Win"),
        ] {
            if self.mods & bit != 0 {
                write!(f, "{name}+")?;
            }
        }
        match self.vk {
            0x70..=0x87 => write!(f, "F{}", self.vk - 0x6F),
            v => write!(f, "{}", char::from_u32(v).unwrap_or('?')),
        }
    }
}

impl Serialize for Chord {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Chord {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Hotkey configuration.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Bindings {
    /// Start / stop.
    pub start_stop: Chord,
    /// Pause / resume.
    pub pause_resume: Chord,
}

impl Default for Bindings {
    fn default() -> Self {
        Self {
            start_stop: Chord {
                mods: MOD_CONTROL.0 | MOD_ALT.0 | MOD_SHIFT.0,
                vk: u32::from(b'R'),
            },
            pause_resume: Chord {
                mods: MOD_CONTROL.0 | MOD_ALT.0 | MOD_SHIFT.0,
                vk: u32::from(b'P'),
            },
        }
    }
}

impl Bindings {
    /// Reject duplicate chords.
    pub fn validate(&self) -> Result<()> {
        if self.start_stop == self.pause_resume {
            return Err(Error::Invalid(
                "start/stop and pause/resume need different hotkeys".into(),
            ));
        }
        Ok(())
    }

    /// Chord bound to `a`.
    pub fn chord(&self, a: HotkeyAction) -> Chord {
        match a {
            HotkeyAction::StartStop => self.start_stop,
            HotkeyAction::PauseResume => self.pause_resume,
        }
    }
}

/// Call the handler, containing panics so the message thread keeps running.
/// Returns false if the handler panicked.
fn dispatch(on_press: &impl Fn(HotkeyAction), a: HotkeyAction) -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_press(a))).is_ok()
}

/// Registered hotkeys; dropping unregisters them and ends the thread.
pub struct Hotkeys {
    thread_id: u32,
    handle: Option<JoinHandle<()>>,
}

impl Hotkeys {
    /// Register `bindings` and call `on_press` for each press (on the hotkey
    /// thread). Returns the chords that could not be registered (owned by another app).
    pub fn start(
        bindings: &Bindings,
        on_press: impl Fn(HotkeyAction) + Send + 'static,
    ) -> Result<(Self, Vec<String>)> {
        bindings.validate()?;
        let bindings = *bindings;
        let (tx, rx) = mpsc::channel::<(u32, Vec<String>)>();
        let handle = thread::Builder::new()
            .name("frostcap-hotkeys".into())
            .spawn(move || {
                let mut msg = MSG::default();
                // SAFETY: create this thread's message queue before announcing it.
                unsafe {
                    let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
                }
                let mut failed = Vec::new();
                let mut ok = Vec::new();
                for a in HotkeyAction::ALL {
                    let c = bindings.chord(a);
                    let mods = HOT_KEY_MODIFIERS(c.mods | MOD_NOREPEAT.0);
                    // SAFETY: thread-level hotkey (no window).
                    if unsafe { RegisterHotKey(None, a.id(), mods, c.vk) }.is_ok() {
                        ok.push(a);
                    } else {
                        failed.push(c.to_string());
                    }
                }
                // SAFETY: trivial.
                let _ = tx.send((unsafe { GetCurrentThreadId() }, failed));
                // SAFETY: standard message loop; exits on WM_QUIT.
                while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                    if msg.message == WM_HOTKEY
                        && let Some(a) = HotkeyAction::from_id(msg.wParam.0 as i32)
                    {
                        let _ = dispatch(&on_press, a);
                    }
                }
                for a in ok {
                    // SAFETY: unregister what we registered.
                    let _ = unsafe { UnregisterHotKey(None, a.id()) };
                }
            })
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let (thread_id, failed) = rx
            .recv()
            .map_err(|_| Error::Invalid("hotkey thread died".into()))?;
        Ok((
            Self {
                thread_id,
                handle: Some(handle),
            },
            failed,
        ))
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        // SAFETY: posting WM_QUIT to our own thread.
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_uncommon_chords() {
        let d = Bindings::default();
        assert_eq!(d.start_stop.to_string(), "Ctrl+Alt+Shift+R");
        assert_eq!(d.pause_resume.to_string(), "Ctrl+Alt+Shift+P");
    }

    #[test]
    fn chords_parse_and_format() {
        let c: Chord = "ctrl + shift + F9".parse().unwrap();
        assert_eq!(c.to_string(), "Ctrl+Shift+F9");
        assert_eq!(c.vk, 0x78);
        let w: Chord = "Win+Alt+R".parse().unwrap();
        assert_eq!(w.to_string(), "Alt+Win+R");
        for bad in [
            "R",
            "Shift+R",
            "Alt+R",
            "Ctrl+",
            "Ctrl+Shift+Esc",
            "Ctrl+Ctrl+R",
            "Ctrl+Hyper+R",
            "",
        ] {
            assert!(bad.parse::<Chord>().is_err(), "{bad:?} must be rejected");
        }
        let json = serde_json::to_string(&Bindings::default()).unwrap();
        assert!(
            json.contains("\"startStop\":\"Ctrl+Alt+Shift+R\""),
            "{json}"
        );
        let back: Bindings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Bindings::default());
    }

    #[test]
    fn chords_need_two_modifiers_and_never_plain_ctrl_alt() {
        for bad in [
            "Ctrl+R",
            "Win+R",
            "Ctrl+F9",
            "Ctrl+Alt+R",
            "Alt+Ctrl+9",
            "ctrl+alt+F5",
        ] {
            assert!(bad.parse::<Chord>().is_err(), "{bad:?} must be rejected");
        }
        for ok in [
            "Ctrl+Shift+R",
            "Ctrl+Win+R",
            "Ctrl+Alt+Shift+R",
            "Win+Alt+R",
            "Ctrl+Alt+Win+F9",
        ] {
            assert!(ok.parse::<Chord>().is_ok(), "{ok:?} must be accepted");
        }
    }

    #[test]
    fn bindings_must_differ() {
        let c: Chord = "Ctrl+Alt+Shift+R".parse().unwrap();
        assert!(
            Bindings {
                start_stop: c,
                pause_resume: c
            }
            .validate()
            .is_err()
        );
        assert!(Bindings::default().validate().is_ok());
    }

    #[test]
    fn a_panicking_handler_does_not_kill_the_hotkey_thread() {
        let calls = std::sync::atomic::AtomicU32::new(0);
        let handler = |a: HotkeyAction| {
            calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if a == HotkeyAction::StartStop {
                panic!("handler bug");
            }
        };
        assert!(!dispatch(&handler, HotkeyAction::StartStop));
        assert!(dispatch(&handler, HotkeyAction::PauseResume));
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[test]
    fn ids_map_to_actions() {
        assert_eq!(
            HotkeyAction::from_id(HotkeyAction::StartStop.id()),
            Some(HotkeyAction::StartStop)
        );
        assert_eq!(HotkeyAction::from_id(999), None);
    }

    #[test]
    #[ignore = "needs interactive desktop"]
    fn registers_and_stops() {
        let (hk, failed) = Hotkeys::start(&Bindings::default(), |_| {}).unwrap();
        // Another app may own a chord; registration failures are reported, not fatal.
        assert!(failed.len() <= 2);
        drop(hk); // must not hang
    }
}
