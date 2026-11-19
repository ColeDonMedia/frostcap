//! Output timeline: maps shared-clock timestamps (100 ns QPC units) to the
//! recording's output timeline, which starts at the first video frame (`t0`) and
//! excludes paused intervals. Pure and thread-safe; used by every track.

use std::sync::Mutex;

/// One pause, on the shared clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    /// Pause start (hns, shared clock).
    pub start: i64,
    /// Pause end (hns, shared clock).
    pub end: i64,
}

#[derive(Debug, Default)]
struct State {
    t0: Option<i64>,
    paused_since: Option<i64>,
    gaps: Vec<Gap>,
    ended_at: Option<i64>,
}

/// Shared output timeline.
#[derive(Debug, Default)]
pub struct Timeline {
    s: Mutex<State>,
}

impl Timeline {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.s
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Set `t0` if not set yet; returns the effective `t0`.
    pub fn anchor(&self, ts: i64) -> i64 {
        *self.lock().t0.get_or_insert(ts)
    }

    /// `t0`, once the first frame arrived.
    pub fn t0(&self) -> Option<i64> {
        self.lock().t0
    }

    /// Begin a pause at `now`. Returns false if already paused.
    pub fn pause(&self, now: i64) -> bool {
        let mut s = self.lock();
        if s.paused_since.is_some() {
            return false;
        }
        s.paused_since = Some(now);
        true
    }

    /// End the current pause at `now`. Returns false if not paused.
    pub fn resume(&self, now: i64) -> bool {
        let mut s = self.lock();
        match s.paused_since.take() {
            Some(start) => {
                s.gaps.push(Gap {
                    start,
                    end: now.max(start),
                });
                true
            }
            None => false,
        }
    }

    /// End the timeline at `now` (or at the open pause start): every track stops
    /// accepting data at the same instant. Idempotent (the first end wins).
    pub fn end(&self, now: i64) {
        let mut s = self.lock();
        if s.ended_at.is_none() {
            s.ended_at = Some(s.paused_since.map_or(now, |p| p.min(now)));
        }
    }

    /// Whether a pause is in progress.
    pub fn is_paused(&self) -> bool {
        self.lock().paused_since.is_some()
    }

    /// Completed pauses.
    pub fn gaps(&self) -> Vec<Gap> {
        self.lock().gaps.clone()
    }

    /// Map a shared-clock timestamp to the output timeline (hns since t0, pauses
    /// removed). `None` before t0 or inside a pause.
    pub fn map(&self, ts: i64) -> Option<i64> {
        let s = self.lock();
        if s.ended_at.is_some_and(|e| ts >= e) {
            return None;
        }
        map_with(s.t0?, &s.gaps, s.paused_since, ts)
    }

    /// Output-timeline position of `now`, clamped to the pause start while paused.
    pub fn position(&self, now: i64) -> Option<i64> {
        let s = self.lock();
        let t0 = s.t0?;
        let mut ts = s.paused_since.map_or(now, |p| p.min(now));
        if let Some(e) = s.ended_at {
            ts = ts.min(e);
        }
        map_with(t0, &s.gaps, None, ts)
    }

    /// Snapshot `(t0, gaps)` for offline conversion (e.g. input tracks).
    pub fn snapshot(&self) -> Option<(i64, Vec<Gap>)> {
        let s = self.lock();
        let mut gaps = s.gaps.clone();
        if let Some(p) = s.paused_since {
            gaps.push(Gap {
                start: p,
                end: i64::MAX,
            });
        }
        Some((s.t0?, gaps))
    }
}

/// Pure mapping used by [`Timeline::map`] and offline conversion.
pub fn map_with(t0: i64, gaps: &[Gap], paused_since: Option<i64>, ts: i64) -> Option<i64> {
    if ts < t0 || paused_since.is_some_and(|p| ts >= p) {
        return None;
    }
    let mut removed = 0i64;
    for g in gaps {
        if ts >= g.end {
            removed += (g.end - g.start.max(t0)).max(0);
        } else if ts >= g.start {
            return None;
        }
    }
    Some(ts - t0 - removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn before_anchor_is_none() {
        let t = Timeline::default();
        assert_eq!(t.map(5), None);
        t.anchor(100);
        t.anchor(200); // ignored
        assert_eq!(t.t0(), Some(100));
        assert_eq!(t.map(50), None);
        assert_eq!(t.map(150), Some(50));
    }

    #[test]
    fn pauses_are_removed() {
        let t = Timeline::default();
        t.anchor(0);
        assert!(t.pause(1_000));
        assert!(!t.pause(1_100));
        assert_eq!(t.map(1_500), None);
        assert_eq!(t.position(1_500), Some(1_000));
        assert!(t.resume(3_000));
        assert!(!t.resume(3_100));
        assert_eq!(t.map(999), Some(999));
        assert_eq!(t.map(2_000), None);
        assert_eq!(t.map(3_000), Some(1_000));
        assert_eq!(t.map(4_000), Some(2_000));
        assert!(t.pause(5_000));
        assert!(t.resume(5_500));
        assert_eq!(t.map(6_000), Some(3_500));
        assert_eq!(t.gaps().len(), 2);
    }

    #[test]
    fn end_stops_every_track_at_the_same_instant() {
        let t = Timeline::default();
        t.anchor(0);
        t.end(5_000);
        assert_eq!(t.map(4_999), Some(4_999));
        assert_eq!(t.map(5_000), None);
        assert_eq!(t.position(9_000), Some(5_000));
        // Ending while paused ends at the pause start.
        let t = Timeline::default();
        t.anchor(0);
        t.pause(2_000);
        t.end(3_000);
        assert_eq!(t.position(9_000), Some(2_000));
        assert_eq!(t.map(2_500), None);
    }

    #[test]
    fn gaps_before_t0_remove_nothing() {
        let before = [Gap {
            start: -100,
            end: -50,
        }];
        assert_eq!(map_with(0, &before, None, 10), Some(10));
        // A gap straddling t0 only removes its part after t0.
        let straddle = [Gap {
            start: -100,
            end: 40,
        }];
        assert_eq!(map_with(0, &straddle, None, 50), Some(10));
    }

    #[test]
    fn snapshot_includes_open_pause() {
        let t = Timeline::default();
        t.anchor(10);
        t.pause(20);
        let (t0, gaps) = t.snapshot().unwrap();
        assert_eq!(t0, 10);
        assert_eq!(map_with(t0, &gaps, None, 30), None);
        assert_eq!(map_with(t0, &gaps, None, 15), Some(5));
    }
}
