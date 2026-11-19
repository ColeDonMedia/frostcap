//! Auto-zoom segment generator.
//!
//! A pure function from recorded input (clicks, typing, focus changes) to a list of
//! [`ZoomSegment`]s the editor can show, tweak and render. No I/O, no randomness:
//! the same input always produces the same segments.
//!
//! ## Algorithm
//! 1. **Activities**: every mouse *down* is a point activity at the click position;
//!    every burst of typing (≥ `typing_min_keys` key-downs with gaps ≤
//!    `typing_gap_ms`) is a span activity located at the cursor position at the
//!    burst start (falling back to the centre of the focused window, then the canvas
//!    centre).
//! 2. **Clustering**: activities are merged in time order while the next one starts
//!    within `merge_gap_ms` of the cluster end, lies within `merge_radius` (fraction
//!    of the canvas diagonal) of the cluster centre, and no focus change to a
//!    *different* window happened in between.
//! 3. **Segments**: each cluster becomes `[start - lead_in, end + hold]` (at least
//!    `min_duration_ms`, clamped to the recording). The scale is the preset scale,
//!    reduced if needed so the whole cluster bounding box fits in the viewport. The
//!    centre is clamped so the zoomed viewport never leaves the canvas.
//! 4. **De-overlap**: overlapping neighbours are split at the midpoint of the
//!    overlap, so the output is sorted and non-overlapping; transitions are
//!    shortened to fit short segments.

use serde::{Deserialize, Serialize};

use crate::project::{
    ButtonState, ClickEvent, CursorSample, Easing, FocusChange, KeyEvent, NormPoint, Project,
    SegmentOrigin, Size, ZoomSegment,
};

/// User-facing auto-zoom presets.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum ZoomPreset {
    /// Mild zoom, long slow transitions, generous merging.
    Gentle,
    /// Default.
    #[default]
    Balanced,
    /// Strong zoom, quick transitions, short holds.
    Snappy,
}

/// Tunables behind a [`ZoomPreset`].
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ZoomParams {
    /// Target magnification.
    pub scale: f64,
    /// Smallest magnification worth emitting after fitting a cluster.
    pub min_scale: f64,
    /// Zoom starts this long before the first activity (ms).
    pub lead_in_ms: u64,
    /// Zoom is held this long after the last activity (ms).
    pub hold_ms: u64,
    /// Zoom-in / zoom-out transition length (ms).
    pub transition_ms: u64,
    /// Max idle gap between activities of the same cluster (ms).
    pub merge_gap_ms: u64,
    /// Max distance from cluster centre, as a fraction of the canvas diagonal.
    pub merge_radius: f64,
    /// Minimum segment length (ms).
    pub min_duration_ms: u64,
    /// Easing curve for transitions.
    pub easing: Easing,
    /// Minimum key-downs to count as a typing burst.
    pub typing_min_keys: usize,
    /// Max gap between key-downs within a burst (ms).
    pub typing_gap_ms: u64,
}

impl ZoomPreset {
    /// Parameters for this preset.
    pub fn params(self) -> ZoomParams {
        match self {
            ZoomPreset::Gentle => ZoomParams {
                scale: 1.5,
                min_scale: 1.2,
                lead_in_ms: 700,
                hold_ms: 1800,
                transition_ms: 900,
                merge_gap_ms: 3000,
                merge_radius: 0.30,
                min_duration_ms: 2500,
                easing: Easing::EaseInOutCubic,
                typing_min_keys: 4,
                typing_gap_ms: 1500,
            },
            ZoomPreset::Balanced => ZoomParams {
                scale: 1.8,
                min_scale: 1.25,
                lead_in_ms: 450,
                hold_ms: 1300,
                transition_ms: 650,
                merge_gap_ms: 2200,
                merge_radius: 0.22,
                min_duration_ms: 1800,
                easing: Easing::EaseInOutCubic,
                typing_min_keys: 3,
                typing_gap_ms: 1200,
            },
            ZoomPreset::Snappy => ZoomParams {
                scale: 2.2,
                min_scale: 1.3,
                lead_in_ms: 250,
                hold_ms: 900,
                transition_ms: 350,
                merge_gap_ms: 1400,
                merge_radius: 0.15,
                min_duration_ms: 1200,
                easing: Easing::EaseOutQuint,
                typing_min_keys: 3,
                typing_gap_ms: 900,
            },
        }
    }
}

/// Borrowed input for [`generate`].
#[derive(Clone, Copy, Debug)]
pub struct AutoZoomInput<'a> {
    /// Canvas (video) size in pixels.
    pub canvas: Size,
    /// Recording duration in ms (segments are clamped to it; 0 = unbounded).
    pub duration_ms: u64,
    /// Mouse button transitions.
    pub clicks: &'a [ClickEvent],
    /// Key-down events.
    pub keys: &'a [KeyEvent],
    /// Cursor samples (sorted by time).
    pub cursor: &'a [CursorSample],
    /// Focus changes (sorted by time).
    pub focus: &'a [FocusChange],
}

impl<'a> AutoZoomInput<'a> {
    /// Build the input from a project's tracks.
    pub fn from_project(p: &'a Project) -> Self {
        Self {
            canvas: p.canvas,
            duration_ms: p.duration_ms,
            clicks: &p.tracks.clicks,
            keys: &p.tracks.keys,
            cursor: &p.tracks.cursor,
            focus: &p.tracks.focus,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Activity {
    start: u64,
    end: u64,
    x: f64,
    y: f64,
}

#[derive(Clone, Copy, Debug)]
struct Cluster {
    start: u64,
    end: u64,
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
}

impl Cluster {
    fn new(a: &Activity) -> Self {
        Self {
            start: a.start,
            end: a.end,
            min_x: a.x,
            min_y: a.y,
            max_x: a.x,
            max_y: a.y,
        }
    }
    fn center(&self) -> (f64, f64) {
        (
            (self.min_x + self.max_x) / 2.0,
            (self.min_y + self.max_y) / 2.0,
        )
    }
    fn add(&mut self, a: &Activity) {
        self.end = self.end.max(a.end);
        self.min_x = self.min_x.min(a.x);
        self.min_y = self.min_y.min(a.y);
        self.max_x = self.max_x.max(a.x);
        self.max_y = self.max_y.max(a.y);
    }
}

/// Whether a sorted boundary list has an entry in `(from, to]` (binary search).
fn crosses(sorted: &[u64], from: u64, to: u64) -> bool {
    let i = sorted.partition_point(|&b| b <= from);
    sorted.get(i).is_some_and(|&b| b <= to)
}

/// Generate zoom segments for a whole project with a preset.
pub fn generate_for_project(project: &Project, preset: ZoomPreset) -> Vec<ZoomSegment> {
    generate(&AutoZoomInput::from_project(project), &preset.params())
}

/// Generate auto-zoom segments. Output is sorted by `start_ms` and non-overlapping.
pub fn generate(input: &AutoZoomInput<'_>, params: &ZoomParams) -> Vec<ZoomSegment> {
    let w = f64::from(input.canvas.width.max(1));
    let h = f64::from(input.canvas.height.max(1));
    let diag = (w * w + h * h).sqrt();

    let mut activities = collect_activities(input, params, w, h);
    if activities.is_empty() {
        return Vec::new();
    }
    activities.sort_by_key(|a| a.start);

    // Focus boundaries: times where the foreground window *title* changed.
    let mut boundaries: Vec<u64> = input
        .focus
        .windows(2)
        .filter(|p| (p[0].window_id, &p[0].title) != (p[1].window_id, &p[1].title))
        .map(|p| p[1].t_ms)
        .collect();
    boundaries.sort_unstable();
    let crosses_boundary = |from: u64, to: u64| crosses(&boundaries, from, to);

    // Cluster.
    let mut clusters: Vec<Cluster> = Vec::new();
    for a in &activities {
        if let Some(c) = clusters.last_mut() {
            let (cx, cy) = c.center();
            let dist = ((a.x - cx).powi(2) + (a.y - cy).powi(2)).sqrt();
            let gap = a.start.saturating_sub(c.end);
            if gap <= params.merge_gap_ms
                && dist <= params.merge_radius * diag
                && !crosses_boundary(c.end, a.start)
            {
                c.add(a);
                continue;
            }
        }
        clusters.push(Cluster::new(a));
    }

    // Clusters -> segments.
    let mut segments: Vec<ZoomSegment> = Vec::with_capacity(clusters.len());
    for c in &clusters {
        // Fit the cluster bbox (+20% margin) inside the viewport.
        let bw = (c.max_x - c.min_x).max(1.0) * 1.2;
        let bh = (c.max_y - c.min_y).max(1.0) * 1.2;
        let scale = params.scale.min(w / bw).min(h / bh);
        if scale < params.min_scale {
            continue;
        }

        let start = c.start.saturating_sub(params.lead_in_ms);
        let mut end = (c.end + params.hold_ms).max(start + params.min_duration_ms);
        if input.duration_ms > 0 {
            end = end.min(input.duration_ms);
        }
        if end <= start {
            continue;
        }

        let (cx, cy) = c.center();
        segments.push(ZoomSegment {
            start_ms: start,
            end_ms: end,
            center: clamp_center(cx / w, cy / h, scale),
            scale: round3(scale),
            easing: params.easing,
            transition_ms: params.transition_ms,
            origin: SegmentOrigin::Auto,
        });
    }

    // De-overlap neighbours.
    for i in 1..segments.len() {
        let (prev_end, next_start) = (segments[i - 1].end_ms, segments[i].start_ms);
        if next_start < prev_end {
            let mid = next_start + (prev_end - next_start) / 2;
            segments[i - 1].end_ms = mid;
            segments[i].start_ms = mid;
        }
    }
    segments.retain(|s| s.end_ms > s.start_ms);
    for s in &mut segments {
        s.transition_ms = s.transition_ms.min((s.end_ms - s.start_ms) / 2);
    }
    segments
}

fn collect_activities(
    input: &AutoZoomInput<'_>,
    params: &ZoomParams,
    w: f64,
    h: f64,
) -> Vec<Activity> {
    let mut out: Vec<Activity> = input
        .clicks
        .iter()
        .filter(|c| c.state == ButtonState::Down)
        .map(|c| Activity {
            start: c.t_ms,
            end: c.t_ms,
            x: f64::from(c.x),
            y: f64::from(c.y),
        })
        .collect();

    // Typing bursts.
    let mut keys: Vec<&KeyEvent> = input.keys.iter().collect();
    keys.sort_by_key(|k| k.t_ms);
    let mut i = 0;
    while i < keys.len() {
        let mut j = i;
        while j + 1 < keys.len() && keys[j + 1].t_ms - keys[j].t_ms <= params.typing_gap_ms {
            j += 1;
        }
        if j + 1 - i >= params.typing_min_keys {
            let (start, end) = (keys[i].t_ms, keys[j].t_ms);
            let (x, y) = typing_position(input, start, w, h);
            out.push(Activity { start, end, x, y });
        }
        i = j + 1;
    }
    out
}

/// Where typing at time `t` most likely happens.
fn typing_position(input: &AutoZoomInput<'_>, t: u64, w: f64, h: f64) -> (f64, f64) {
    if let Some(s) = last_at_or_before(input.cursor, t, |s| s.t_ms) {
        return (f64::from(s.x), f64::from(s.y));
    }
    if let Some(f) = last_at_or_before(input.focus, t, |f| f.t_ms) {
        return (
            f64::from(f.rect.x) + f64::from(f.rect.width) / 2.0,
            f64::from(f.rect.y) + f64::from(f.rect.height) / 2.0,
        );
    }
    (w / 2.0, h / 2.0)
}

fn last_at_or_before<T>(items: &[T], t: u64, key: impl Fn(&T) -> u64) -> Option<&T> {
    let idx = items.partition_point(|it| key(it) <= t);
    if idx == 0 {
        items.first()
    } else {
        items.get(idx - 1)
    }
}

/// Clamp a normalized centre so a viewport of size `1/scale` stays inside 0..1.
fn clamp_center(x: f64, y: f64, scale: f64) -> NormPoint {
    let half = 0.5 / scale.max(1.0);
    NormPoint {
        x: round3(x.clamp(half, 1.0 - half)),
        y: round3(y.clamp(half, 1.0 - half)),
    }
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boundary_search_matches_the_linear_scan() {
        let b: Vec<u64> = vec![0, 5, 5, 10, 100, 101, 5000];
        for from in 0..=5100u64 {
            for to in [from, from + 1, from + 7, from + 150, 6000] {
                let want = b.iter().any(|&x| x > from && x <= to);
                assert_eq!(crosses(&b, from, to), want, "({from},{to})");
            }
        }
        assert!(!crosses(&[], 0, u64::MAX));
    }

    use crate::project::{Modifiers, MouseButton, Rect};

    const CANVAS: Size = Size {
        width: 1920,
        height: 1080,
    };

    fn click(t: u64, x: i32, y: i32) -> ClickEvent {
        ClickEvent {
            t_ms: t,
            x,
            y,
            button: MouseButton::Left,
            state: ButtonState::Down,
        }
    }
    fn up(t: u64, x: i32, y: i32) -> ClickEvent {
        ClickEvent {
            state: ButtonState::Up,
            ..click(t, x, y)
        }
    }
    fn key(t: u64) -> KeyEvent {
        KeyEvent {
            t_ms: t,
            vk: 0x41,
            label: "A".into(),
            modifiers: Modifiers::default(),
        }
    }
    fn focus(t: u64, title: &str) -> FocusChange {
        FocusChange {
            t_ms: t,
            window_id: 0,
            title: title.into(),
            rect: Rect {
                x: 100,
                y: 100,
                width: 800,
                height: 600,
            },
        }
    }
    fn input<'a>(
        clicks: &'a [ClickEvent],
        keys: &'a [KeyEvent],
        cursor: &'a [CursorSample],
        focus: &'a [FocusChange],
    ) -> AutoZoomInput<'a> {
        AutoZoomInput {
            canvas: CANVAS,
            duration_ms: 60_000,
            clicks,
            keys,
            cursor,
            focus,
        }
    }
    fn assert_sorted_non_overlapping(s: &[ZoomSegment]) {
        for w in s.windows(2) {
            assert!(w[0].end_ms <= w[1].start_ms, "overlap: {w:?}");
        }
        for seg in s {
            assert!(seg.end_ms > seg.start_ms);
            assert!(seg.transition_ms * 2 <= seg.end_ms - seg.start_ms);
        }
    }

    #[test]
    fn no_events_no_segments() {
        let s = generate(&input(&[], &[], &[], &[]), &ZoomPreset::Balanced.params());
        assert!(s.is_empty());
    }

    #[test]
    fn single_click_makes_one_segment_around_it() {
        let p = ZoomPreset::Balanced.params();
        let clicks = [click(10_000, 960, 540), up(10_080, 960, 540)];
        let s = generate(&input(&clicks, &[], &[], &[]), &p);
        assert_eq!(s.len(), 1);
        let seg = s[0];
        assert_eq!(seg.start_ms, 10_000 - p.lead_in_ms);
        assert!(seg.end_ms >= 10_000 + p.hold_ms);
        assert!(seg.end_ms - seg.start_ms >= p.min_duration_ms);
        assert!((seg.center.x - 0.5).abs() < 1e-9 && (seg.center.y - 0.5).abs() < 1e-9);
        assert!((seg.scale - p.scale).abs() < 1e-9);
        assert_eq!(seg.origin, SegmentOrigin::Auto);
    }

    #[test]
    fn mouse_up_alone_is_ignored() {
        let clicks = [up(5_000, 100, 100)];
        assert!(
            generate(
                &input(&clicks, &[], &[], &[]),
                &ZoomPreset::Balanced.params()
            )
            .is_empty()
        );
    }

    #[test]
    fn nearby_clicks_merge_far_ones_split() {
        let p = ZoomPreset::Balanced.params();
        // Two clicks close in time and space -> one segment.
        let clicks = [click(5_000, 500, 500), click(6_000, 560, 520)];
        assert_eq!(generate(&input(&clicks, &[], &[], &[]), &p).len(), 1);
        // Far apart in time -> two segments.
        let clicks = [click(5_000, 500, 500), click(20_000, 520, 500)];
        let s = generate(&input(&clicks, &[], &[], &[]), &p);
        assert_eq!(s.len(), 2);
        assert_sorted_non_overlapping(&s);
        // Close in time but opposite corners -> two segments.
        let clicks = [click(5_000, 100, 100), click(5_500, 1800, 1000)];
        let s = generate(&input(&clicks, &[], &[], &[]), &p);
        assert_eq!(s.len(), 2);
        assert_sorted_non_overlapping(&s);
    }

    #[test]
    fn center_is_clamped_inside_canvas() {
        let p = ZoomPreset::Snappy.params();
        let clicks = [click(3_000, 0, 0)];
        let s = generate(&input(&clicks, &[], &[], &[]), &p);
        let half = 0.5 / s[0].scale;
        assert!((s[0].center.x - round3(half)).abs() < 1e-3);
        assert!((s[0].center.y - round3(half)).abs() < 1e-3);
        let clicks = [click(3_000, 1920, 1080)];
        let s = generate(&input(&clicks, &[], &[], &[]), &p);
        assert!((s[0].center.x - round3(1.0 - half)).abs() < 1e-3);
    }

    #[test]
    fn typing_burst_zooms_at_cursor() {
        let p = ZoomPreset::Balanced.params();
        let keys: Vec<KeyEvent> = (0..6).map(|i| key(8_000 + i * 150)).collect();
        let cursor = [CursorSample {
            t_ms: 7_000,
            x: 480,
            y: 270,
        }];
        let s = generate(&input(&[], &keys, &cursor, &[]), &p);
        assert_eq!(s.len(), 1);
        assert!((s[0].center.x - 0.25).abs() < 0.03 + 0.5 / p.scale);
        assert_eq!(s[0].start_ms, 8_000 - p.lead_in_ms);
        assert!(s[0].end_ms >= 8_750 + p.hold_ms);
    }

    #[test]
    fn sparse_keys_are_not_a_burst() {
        let p = ZoomPreset::Balanced.params();
        let keys = [key(1_000), key(10_000), key(20_000)];
        assert!(generate(&input(&[], &keys, &[], &[]), &p).is_empty());
    }

    #[test]
    fn typing_without_cursor_uses_focused_window() {
        let p = ZoomPreset::Gentle.params();
        let keys: Vec<KeyEvent> = (0..5).map(|i| key(4_000 + i * 100)).collect();
        let f = [focus(0, "Editor")];
        let s = generate(&input(&[], &keys, &[], &f), &p);
        assert_eq!(s.len(), 1);
        // window centre = (500, 400) -> clamped to viewport
        let half = 0.5 / s[0].scale;
        assert!((s[0].center.x - (500.0 / 1920.0_f64).clamp(half, 1.0 - half)).abs() < 1e-3);
    }

    #[test]
    fn focus_change_splits_clusters() {
        let p = ZoomPreset::Gentle.params();
        let clicks = [click(5_000, 500, 500), click(6_000, 520, 500)];
        let f = [focus(0, "A"), focus(5_500, "B")];
        let s = generate(&input(&clicks, &[], &[], &f), &p);
        assert_eq!(s.len(), 2);
        assert_sorted_non_overlapping(&s);
        // Same title re-focus does not split.
        let f = [focus(0, "A"), focus(5_500, "A")];
        assert_eq!(generate(&input(&clicks, &[], &[], &f), &p).len(), 1);
    }

    #[test]
    fn clamped_to_duration_and_zero_start() {
        let p = ZoomPreset::Gentle.params();
        let clicks = [click(100, 900, 500), click(2_900, 950, 500)];
        let mut inp = input(&clicks, &[], &[], &[]);
        inp.duration_ms = 3_000;
        let s = generate(&inp, &p);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].start_ms, 0);
        assert_eq!(s[0].end_ms, 3_000);
        assert_sorted_non_overlapping(&s);
    }

    #[test]
    fn presets_are_ordered() {
        let (g, b, s) = (
            ZoomPreset::Gentle.params(),
            ZoomPreset::Balanced.params(),
            ZoomPreset::Snappy.params(),
        );
        assert!(g.scale < b.scale && b.scale < s.scale);
        assert!(g.transition_ms > b.transition_ms && b.transition_ms > s.transition_ms);
        assert!(g.merge_gap_ms > b.merge_gap_ms && b.merge_gap_ms > s.merge_gap_ms);
    }

    #[test]
    fn many_events_are_sorted_and_deterministic() {
        let clicks: Vec<ClickEvent> = (0..50)
            .map(|i| click(i * 700, ((i * 137) % 1900) as i32, ((i * 71) % 1070) as i32))
            .collect();
        for preset in [ZoomPreset::Gentle, ZoomPreset::Balanced, ZoomPreset::Snappy] {
            let a = generate(&input(&clicks, &[], &[], &[]), &preset.params());
            let b = generate(&input(&clicks, &[], &[], &[]), &preset.params());
            assert_eq!(a, b);
            assert!(!a.is_empty());
            assert_sorted_non_overlapping(&a);
            for seg in &a {
                assert!(
                    seg.scale >= preset.params().min_scale && seg.scale <= preset.params().scale
                );
                let half = 0.5 / seg.scale;
                assert!(seg.center.x >= half - 1e-3 && seg.center.x <= 1.0 - half + 1e-3);
            }
        }
    }
}
