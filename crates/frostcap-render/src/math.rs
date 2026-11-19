//! Pure easing / layout / transform math shared by the compositor.
//!
//! Every function here mirrors `ui/src/lib/render.ts` (the editor preview) so the
//! exported video matches what the user saw. No GPU, no I/O: unit tested below.

use crate::model::{Easing, Frame, Gradient, RenderProject, ZoomSegment};

/// Easing curve, identical to `ease()` in `render.ts`. `k` is clamped to 0..1.
/// `Spring` may overshoot 1 (that is intended, the preview does it too).
pub fn ease(e: Easing, k: f64) -> f64 {
    let k = k.clamp(0.0, 1.0);
    match e {
        Easing::Linear => k,
        // UI "easeOut": cubic.
        Easing::EaseOutCubic => 1.0 - (1.0 - k).powi(3),
        // Core "easeOutQuint".
        Easing::EaseOutQuint => 1.0 - (1.0 - k).powi(5),
        Easing::Spring => {
            if k >= 1.0 {
                1.0
            } else {
                1.0 - (-6.5 * k).exp() * (9.0 * k).cos()
            }
        }
        Easing::EaseInOutCubic => {
            if k < 0.5 {
                4.0 * k * k * k
            } else {
                1.0 - (-2.0 * k + 2.0).powi(3) / 2.0
            }
        }
    }
}

/// Active zoom segment at source time `t` and its eased amount (0 = none).
/// Mirrors `zoomAt()`: the first segment with `start <= t <= end` wins and the
/// ramp length is capped at half the segment.
pub fn zoom_at(segs: &[ZoomSegment], t: f64) -> Option<(&ZoomSegment, f64)> {
    for s in segs {
        if t < s.start_ms || t > s.end_ms {
            continue;
        }
        let len = s.end_ms - s.start_ms;
        let tr = s.transition_ms.min(len / 2.0);
        let mut k = 1.0;
        if tr > 0.0 {
            if t < s.start_ms + tr {
                k = (t - s.start_ms) / tr;
            } else if t > s.end_ms - tr {
                k = (s.end_ms - t) / tr;
            }
        }
        return Some((s, ease(s.easing, k)));
    }
    None
}

/// Axis-aligned rectangle (x, y, w, h) in output pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectF {
    /// Left.
    pub x: f64,
    /// Top.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl RectF {
    /// Right edge.
    pub fn x1(&self) -> f64 {
        self.x + self.w
    }
    /// Bottom edge.
    pub fn y1(&self) -> f64 {
        self.y + self.h
    }
}

/// Unzoomed layout of the captured screen inside the output frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// Window rect including the title bar.
    pub window: RectF,
    /// Height of the title bar (0 when `frame` is none).
    pub bar_h: f64,
    /// Video (content) rect.
    pub content: RectF,
    /// Corner radius (output px, already clamped like `roundRect`).
    pub radius: f64,
    /// Style unit: output height / 1080.
    pub unit: f64,
}

/// Title bar height for a frame style at `unit` (output height / 1080).
pub fn bar_height(frame: Frame, unit: f64) -> f64 {
    match frame {
        Frame::None => 0.0,
        Frame::Minimal => 22.0 * unit,
        Frame::Windows | Frame::MacOs => 34.0 * unit,
    }
}

/// Mirrors `contentRect()` / step 2 of `renderFrame()`.
pub fn layout(w: f64, h: f64, src_w: f64, src_h: f64, p: &RenderProject) -> Layout {
    let st = &p.style;
    let unit = h / 1080.0;
    let pad = (st.padding / 100.0) * w.min(h * 1.78);
    let bar_h = bar_height(st.frame, unit);
    let avail_w = w - pad * 2.0;
    let avail_h = h - pad * 2.0;
    let aspect = if src_h > 0.0 {
        src_w / src_h
    } else {
        16.0 / 9.0
    };
    let mut cw = avail_w;
    let mut ch = cw / aspect;
    if ch + bar_h > avail_h {
        ch = avail_h - bar_h;
        cw = ch * aspect;
    }
    let rx = (w - cw) / 2.0;
    let ry = (h - ch - bar_h) / 2.0;
    let wh = ch + bar_h;
    let radius = (st.radius * unit).min(cw / 2.0).min(wh / 2.0).max(0.0);
    Layout {
        window: RectF {
            x: rx,
            y: ry,
            w: cw,
            h: wh,
        },
        bar_h,
        content: RectF {
            x: rx,
            y: ry + bar_h,
            w: cw,
            h: ch,
        },
        radius,
        unit,
    }
}

/// Zoom/pan transform: `screen = (p - t) * s + (W/2, H/2)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomXf {
    /// Scale.
    pub s: f64,
    /// Focus point (canvas space) mapped to the frame centre.
    pub tx: f64,
    /// Focus point y.
    pub ty: f64,
    /// Output width.
    pub w: f64,
    /// Output height.
    pub h: f64,
}

impl ZoomXf {
    /// Identity transform for a `w x h` frame.
    pub fn identity(w: f64, h: f64) -> Self {
        Self {
            s: 1.0,
            tx: w / 2.0,
            ty: h / 2.0,
            w,
            h,
        }
    }
    /// Map a canvas point to output pixels.
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (
            (x - self.tx) * self.s + self.w / 2.0,
            (y - self.ty) * self.s + self.h / 2.0,
        )
    }
    /// Map a rect.
    pub fn rect(&self, r: RectF) -> RectF {
        let (x, y) = self.apply(r.x, r.y);
        RectF {
            x,
            y,
            w: r.w * self.s,
            h: r.h * self.s,
        }
    }
}

/// Step 3 of `renderFrame()`: the zoom transform at source time `t`.
pub fn zoom_transform(w: f64, h: f64, lay: &Layout, p: &RenderProject, t: f64) -> ZoomXf {
    match zoom_at(&p.zooms, t) {
        Some((seg, e)) if e > 0.0 => {
            let s = 1.0 + (seg.scale - 1.0) * e;
            let fx = lay.content.x + seg.cx * lay.content.w;
            let fy = lay.content.y + seg.cy * lay.content.h;
            let min_x = w / (2.0 * s);
            let min_y = h / (2.0 * s);
            let em = e.min(1.0);
            // Same as Math.min(W - minX, Math.max(minX, ...)) (no panic when min > max).
            let tx = (w / 2.0 + (fx - w / 2.0) * em).max(min_x).min(w - min_x);
            let ty = (h / 2.0 + (fy - h / 2.0) * em).max(min_y).min(h - min_y);
            ZoomXf { s, tx, ty, w, h }
        }
        _ => ZoomXf::identity(w, h),
    }
}

/// Linear-interpolated normalized cursor position (`cursorAt()` in the editor store).
pub fn cursor_at(samples: &[(f64, f64, f64)], t: f64) -> (f64, f64) {
    let Some(first) = samples.first() else {
        return (0.5, 0.5);
    };
    let last = samples[samples.len() - 1];
    if t <= first.0 {
        return (first.1, first.2);
    }
    if t >= last.0 {
        return (last.1, last.2);
    }
    let i = samples.partition_point(|s| s.0 <= t);
    let a = samples[i - 1];
    let b = samples[i];
    let k = (t - a.0) / (b.0 - a.0).max(1.0);
    (a.1 + (b.1 - a.1) * k, a.2 + (b.2 - a.2) * k)
}

/// Smoothed cursor position: average of 7 samples over `[t - win, t]` with
/// `win = smoothing * 220 ms` (the preview's box filter).
pub fn smoothed_cursor(samples: &[(f64, f64, f64)], smoothing: f64, t: f64) -> (f64, f64) {
    let win = smoothing * 220.0;
    if win < 5.0 {
        return cursor_at(samples, t);
    }
    let n = 7;
    let (mut sx, mut sy) = (0.0, 0.0);
    for i in 0..n {
        let q = cursor_at(samples, t - win + (i as f64 / (n - 1) as f64) * win);
        sx += q.0;
        sy += q.1;
    }
    (sx / n as f64, sy / n as f64)
}

/// Endpoints of a CSS-angle linear gradient over a `w x h` box (canvas semantics
/// used by the preview: 0deg = bottom-to-top, 90deg = left-to-right).
pub fn gradient_line(g: &Gradient, w: f64, h: f64) -> (f64, f64, f64, f64) {
    let ang = g.angle_deg.to_radians();
    let dx = ang.sin();
    let dy = -ang.cos();
    let half = ((w * dx).abs() + (h * dy).abs()) / 2.0;
    (
        w / 2.0 - dx * half,
        h / 2.0 - dy * half,
        w / 2.0 + dx * half,
        h / 2.0 + dy * half,
    )
}

/// Evaluate the gradient at pixel `(x, y)`; returns sRGB-encoded RGBA 0..1.
/// Same interpolation as the shader (and as Canvas 2D: in sRGB space).
pub fn gradient_color(g: &Gradient, w: f64, h: f64, x: f64, y: f64) -> [f64; 4] {
    let (x0, y0, x1, y1) = gradient_line(g, w, h);
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len2 = (dx * dx + dy * dy).max(1e-9);
    let t = (((x - x0) * dx + (y - y0) * dy) / len2).clamp(0.0, 1.0);
    let n = g.stops.len();
    if n == 0 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    if n == 1 {
        return g.stops[0];
    }
    let f = t * (n - 1) as f64;
    let i = (f.floor() as usize).min(n - 2);
    let k = f - i as f64;
    let (a, b) = (g.stops[i], g.stops[i + 1]);
    [
        a[0] + (b[0] - a[0]) * k,
        a[1] + (b[1] - a[1]) * k,
        a[2] + (b[2] - a[2]) * k,
        a[3] + (b[3] - a[3]) * k,
    ]
}

/// Ripple radius / opacity for a click of age `age_ms` (None when not visible).
/// Returns (radius in canvas px before zoom, k 0..1).
pub fn ripple(age_ms: f64, unit: f64, cursor_size: f64) -> Option<(f64, f64)> {
    if !(0.0..=550.0).contains(&age_ms) {
        return None;
    }
    let k = age_ms / 550.0;
    Some((
        (8.0 + 34.0 * ease(Easing::EaseOutCubic, k)) * unit * cursor_size * 0.8,
        k,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RenderProject, Style};

    fn seg(start: f64, end: f64, easing: Easing) -> ZoomSegment {
        ZoomSegment {
            start_ms: start,
            end_ms: end,
            cx: 0.75,
            cy: 0.25,
            scale: 2.0,
            easing,
            transition_ms: 600.0,
        }
    }

    #[test]
    fn easing_endpoints_and_midpoints() {
        for e in [
            Easing::Linear,
            Easing::EaseInOutCubic,
            Easing::EaseOutCubic,
            Easing::EaseOutQuint,
            Easing::Spring,
        ] {
            assert!(ease(e, 0.0).abs() < 1e-12, "{e:?}");
            assert!((ease(e, 1.0) - 1.0).abs() < 1e-12, "{e:?}");
            assert_eq!(ease(e, -1.0), ease(e, 0.0));
            assert_eq!(ease(e, 2.0), ease(e, 1.0));
        }
        assert!((ease(Easing::EaseInOutCubic, 0.5) - 0.5).abs() < 1e-12);
        assert!((ease(Easing::EaseInOutCubic, 0.25) - 0.0625).abs() < 1e-12);
        assert!((ease(Easing::EaseOutCubic, 0.5) - 0.875).abs() < 1e-12);
        assert!((ease(Easing::Linear, 0.3) - 0.3).abs() < 1e-12);
        // Spring overshoots like the preview.
        let peak = (0..100)
            .map(|i| ease(Easing::Spring, i as f64 / 100.0))
            .fold(0.0, f64::max);
        assert!(peak > 1.0);
    }

    #[test]
    fn zoom_ramps_hold_and_inclusive_end() {
        let segs = [seg(1000.0, 4000.0, Easing::Linear)];
        assert!(zoom_at(&segs, 999.0).is_none());
        assert_eq!(zoom_at(&segs, 1000.0).unwrap().1, 0.0);
        assert!((zoom_at(&segs, 1300.0).unwrap().1 - 0.5).abs() < 1e-12);
        assert_eq!(zoom_at(&segs, 2500.0).unwrap().1, 1.0);
        assert!((zoom_at(&segs, 3700.0).unwrap().1 - 0.5).abs() < 1e-12);
        assert_eq!(zoom_at(&segs, 4000.0).unwrap().1, 0.0);
        assert!(zoom_at(&segs, 4000.1).is_none());
        // Ramp capped at half of a short segment.
        let short = [seg(0.0, 400.0, Easing::Linear)];
        assert!((zoom_at(&short, 100.0).unwrap().1 - 0.5).abs() < 1e-12);
    }

    fn project() -> RenderProject {
        RenderProject {
            canvas: (1600, 900),
            style: Style {
                padding: 8.0,
                radius: 16.0,
                frame: Frame::Minimal,
                ..Style::default()
            },
            // cy = 0.05 puts the focus above H/(2s), so the pan clamp kicks in.
            zooms: vec![ZoomSegment {
                cy: 0.05,
                ..seg(0.0, 3000.0, Easing::EaseInOutCubic)
            }],
            ..RenderProject::empty_for_tests()
        }
    }

    #[test]
    fn layout_matches_preview_formula() {
        let p = project();
        let l = layout(1920.0, 1080.0, 1600.0, 900.0, &p);
        let pad = 0.08 * 1920f64.min(1080.0 * 1.78);
        assert!((l.bar_h - 22.0).abs() < 1e-9);
        // Height-limited: ch = availH - barH.
        assert!((l.content.h - (1080.0 - 2.0 * pad - 22.0)).abs() < 1e-9);
        assert!((l.content.w - l.content.h * 16.0 / 9.0).abs() < 1e-9);
        assert!((l.window.x - (1920.0 - l.content.w) / 2.0).abs() < 1e-9);
        assert!((l.content.y - l.window.y - 22.0).abs() < 1e-9);
        assert!((l.radius - 16.0).abs() < 1e-9);
        // 9:16 is width-limited.
        let v = layout(1080.0, 1920.0, 1600.0, 900.0, &p);
        assert!((v.content.w - (1080.0 - 2.0 * 0.08 * 1080.0)).abs() < 1e-9);
    }

    #[test]
    fn zoom_transform_centres_focus_and_clamps() {
        let p = project();
        let l = layout(1920.0, 1080.0, 1600.0, 900.0, &p);
        let xf = zoom_transform(1920.0, 1080.0, &l, &p, 1500.0);
        assert!((xf.s - 2.0).abs() < 1e-12);
        let fx = l.content.x + 0.75 * l.content.w;
        // Focus x is inside the clamp window so it maps to the centre.
        let (sx, _) = xf.apply(fx, 0.0);
        assert!((sx - 960.0).abs() < 1e-9);
        // Focus y (0.05) would show outside the frame -> clamped to minY.
        assert!((xf.ty - 1080.0 / 4.0).abs() < 1e-9);
        // The zoomed frame never shows outside the canvas.
        let (x0, y0) = xf.apply(0.0, 0.0);
        let (x1, y1) = xf.apply(1920.0, 1080.0);
        assert!(x0 <= 1e-9 && y0 <= 1e-9 && x1 >= 1920.0 - 1e-9 && y1 >= 1080.0 - 1e-9);
        // Outside segments: identity.
        let id = zoom_transform(1920.0, 1080.0, &l, &p, 5000.0);
        assert_eq!(id, ZoomXf::identity(1920.0, 1080.0));
    }

    #[test]
    fn cursor_interpolation_and_smoothing() {
        let s = [(0.0, 0.0, 0.0), (100.0, 1.0, 0.5)];
        assert_eq!(cursor_at(&s, -5.0), (0.0, 0.0));
        assert_eq!(cursor_at(&s, 50.0), (0.5, 0.25));
        assert_eq!(cursor_at(&s, 500.0), (1.0, 0.5));
        assert_eq!(cursor_at(&[], 1.0), (0.5, 0.5));
        // Smoothing lags behind the raw position while moving.
        let raw = cursor_at(&s, 80.0);
        let sm = smoothed_cursor(&s, 0.6, 80.0);
        assert!(sm.0 < raw.0);
        // Standing still: identical.
        assert_eq!(smoothed_cursor(&s, 0.6, 1000.0), (1.0, 0.5));
        assert_eq!(smoothed_cursor(&s, 0.0, 80.0), raw);
    }

    #[test]
    fn gradient_line_and_color() {
        let g = Gradient {
            angle_deg: 90.0,
            stops: vec![[0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]],
        };
        let (x0, y0, x1, y1) = gradient_line(&g, 200.0, 100.0);
        assert!((x0 - 0.0).abs() < 1e-9 && (x1 - 200.0).abs() < 1e-9);
        assert!((y0 - 50.0).abs() < 1e-9 && (y1 - 50.0).abs() < 1e-9);
        let c = gradient_color(&g, 200.0, 100.0, 50.0, 10.0);
        assert!((c[0] - 0.25).abs() < 1e-9);
        // 180deg: top -> bottom.
        let g2 = Gradient {
            angle_deg: 180.0,
            ..g.clone()
        };
        assert!(gradient_color(&g2, 200.0, 100.0, 0.0, 0.0)[0] < 0.01);
        assert!(gradient_color(&g2, 200.0, 100.0, 0.0, 100.0)[0] > 0.99);
    }

    #[test]
    fn ripple_window() {
        assert!(ripple(-1.0, 1.0, 1.0).is_none());
        assert!(ripple(551.0, 1.0, 1.0).is_none());
        let (r0, k0) = ripple(0.0, 1.0, 1.0).unwrap();
        assert!((r0 - 6.4).abs() < 1e-9 && k0 == 0.0);
        let (r1, _) = ripple(550.0, 1.0, 1.0).unwrap();
        assert!((r1 - 42.0 * 0.8).abs() < 1e-9);
    }
}
