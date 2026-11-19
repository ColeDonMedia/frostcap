//! Shared monotonic clock.
//!
//! Windows Graphics Capture stamps frames with `SystemRelativeTime`, which is the
//! QueryPerformanceCounter clock expressed in 100 ns units. Every side track
//! (input, audio) is stamped on the same clock, so syncing them to the video is a
//! subtraction of the first frame's timestamp.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, Ordering};

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// Ticks of the shared clock are 100 ns ("hns"), like WinRT `TimeSpan`.
pub const HNS_PER_SEC: i64 = 10_000_000;
/// 100 ns ticks per millisecond.
pub const HNS_PER_MS: i64 = 10_000;

fn qpc_freq() -> i64 {
    static FREQ: OnceLock<i64> = OnceLock::new();
    *FREQ.get_or_init(|| {
        let mut f = 0i64;
        // SAFETY: valid out pointer; cannot fail on XP+.
        let _ = unsafe { QueryPerformanceFrequency(&mut f) };
        f.max(1)
    })
}

/// Current QPC time in 100 ns units (same base as WGC `SystemRelativeTime`).
pub fn now_hns() -> i64 {
    let mut c = 0i64;
    // SAFETY: valid out pointer.
    let _ = unsafe { QueryPerformanceCounter(&mut c) };
    qpc_to_hns(c)
}

/// Convert raw QPC ticks to 100 ns units.
pub fn qpc_to_hns(ticks: i64) -> i64 {
    ((i128::from(ticks) * i128::from(HNS_PER_SEC)) / i128::from(qpc_freq())) as i64
}

/// Video clock anchors, written by the capture thread.
#[derive(Debug)]
pub struct VideoClock {
    first: AtomicI64,
    last: AtomicI64,
}

impl Default for VideoClock {
    fn default() -> Self {
        Self {
            first: AtomicI64::new(i64::MIN),
            last: AtomicI64::new(i64::MIN),
        }
    }
}

impl VideoClock {
    /// Register a frame timestamp (hns).
    pub fn on_frame(&self, ts: i64) {
        let _ = self
            .first
            .compare_exchange(i64::MIN, ts, Ordering::AcqRel, Ordering::Acquire);
        self.last.store(ts, Ordering::Release);
    }
    /// Timestamp of the first frame (hns), if any arrived.
    pub fn first(&self) -> Option<i64> {
        Some(self.first.load(Ordering::Acquire)).filter(|&v| v != i64::MIN)
    }
    /// Timestamp of the latest frame (hns), if any arrived.
    pub fn last(&self) -> Option<i64> {
        Some(self.last.load(Ordering::Acquire)).filter(|&v| v != i64::MIN)
    }
}

/// Convert an absolute hns timestamp to ms on the video clock (None if before t0).
pub fn to_video_ms(ts: i64, t0: i64) -> Option<u64> {
    (ts >= t0).then(|| ((ts - t0) / HNS_PER_MS) as u64)
}
