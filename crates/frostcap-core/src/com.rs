//! Process-wide COM multithreaded apartment.
//!
//! windows-rs caches WinRT activation factories in statics. If the MTA is torn
//! down (last `CoUninitialize`) those cached pointers dangle and the next WinRT
//! call crashes, so we pin the MTA for the process lifetime once.

use std::sync::OnceLock;

use windows::Win32::System::Com::CoIncrementMTAUsage;

/// Keep the MTA alive for the rest of the process. Cheap after the first call.
pub fn ensure_mta() {
    static PINNED: OnceLock<bool> = OnceLock::new();
    PINNED.get_or_init(|| {
        // SAFETY: the cookie is intentionally leaked (never decremented).
        unsafe { CoIncrementMTAUsage() }.is_ok()
    });
}
