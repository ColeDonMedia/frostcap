//! Per-frame scene description: everything the GPU pass needs, in output pixels.
//! Pure CPU code (deterministic for a given project + time), unit tested.

use crate::math::{self, RectF};
use crate::model::{Frame, RenderProject, WebcamShape};

/// Maximum simultaneous click ripples / cursor draws passed to the shader.
pub const MAX_RIPPLES: usize = 8;
/// Main cursor + up to 3 motion-blur ghosts.
pub const MAX_CURSORS: usize = 4;
/// Maximum gradient stops.
pub const MAX_STOPS: usize = 8;

/// Gaussian drop shadow of a rounded rect (canvas `shadowBlur` semantics:
/// sigma = blur / 2, in device pixels, not affected by the zoom transform).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    /// Shadow-casting rect (already offset).
    pub rect: RectF,
    /// Corner radius.
    pub radius: f64,
    /// Gaussian sigma.
    pub sigma: f64,
    /// Opacity of the black shadow.
    pub alpha: f64,
}

/// Webcam bubble placement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Webcam {
    /// Bubble rect.
    pub rect: RectF,
    /// Corner radius (size/2 for a circle).
    pub radius: f64,
    /// Shadow sigma.
    pub shadow_sigma: f64,
}

/// Key overlay state.
#[derive(Clone, Debug, PartialEq)]
pub struct Keys {
    /// Text shown.
    pub text: String,
    /// Opacity 0..1.
    pub alpha: f64,
}

/// One output frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    /// Source time this frame shows (ms).
    pub t_src: f64,
    /// Output size.
    pub w: f64,
    /// Output height.
    pub h: f64,
    /// Gradient line (x0, y0, x1, y1).
    pub grad_line: (f64, f64, f64, f64),
    /// Gradient stops (sRGB 0..1).
    pub stops: Vec<[f64; 4]>,
    /// Window drop shadow.
    pub shadow: Option<Shadow>,
    /// Window rect (incl. bar) after zoom.
    pub window: RectF,
    /// Window corner radius after zoom.
    pub radius: f64,
    /// Bar height after zoom.
    pub bar_h: f64,
    /// Frame style.
    pub frame: Frame,
    /// Style unit after zoom (unit * s).
    pub unit_s: f64,
    /// Video rect after zoom.
    pub video: RectF,
    /// Mip level for sampling the video texture.
    pub video_lod: f64,
    /// Ripples (x, y, radius, k).
    pub ripples: Vec<[f64; 4]>,
    /// Ripple stroke width.
    pub ripple_lw: f64,
    /// Cursor draws (x, y, alpha), back to front.
    pub cursors: Vec<[f64; 3]>,
    /// Pixels per cursor-polygon unit.
    pub cursor_scale: f64,
    /// Webcam bubble.
    pub webcam: Option<Webcam>,
    /// Index of the active caption cue.
    pub caption: Option<usize>,
    /// Key overlay.
    pub keys: Option<Keys>,
}

/// Key overlay timing (ms).
const KEY_SHOW_MS: f64 = 1500.0;
const KEY_FADE_MS: f64 = 300.0;
const KEY_GROUP_GAP_MS: f64 = 800.0;

fn key_overlay(p: &RenderProject, t: f64) -> Option<Keys> {
    if !p.style.show_keys {
        return None;
    }
    let idx = p.keys.partition_point(|k| k.t <= t);
    if idx == 0 {
        return None;
    }
    let last = &p.keys[idx - 1];
    let age = t - last.t;
    if age > KEY_SHOW_MS {
        return None;
    }
    let mut start = idx - 1;
    while start > 0 && idx - start < 4 && p.keys[start].t - p.keys[start - 1].t < KEY_GROUP_GAP_MS {
        start -= 1;
    }
    let text = p.keys[start..idx]
        .iter()
        .map(|k| k.label.as_str())
        .collect::<Vec<_>>()
        .join("  ");
    let alpha = ((KEY_SHOW_MS - age) / KEY_FADE_MS).clamp(0.0, 1.0);
    Some(Keys { text, alpha })
}

/// Build the scene for source time `t` at output size `w x h`.
/// `has_webcam`: a webcam stream is available (the bubble is skipped otherwise,
/// the editor's placeholder silhouette is never exported).
pub fn build(p: &RenderProject, w: f64, h: f64, t: f64, has_webcam: bool) -> Scene {
    let st = &p.style;
    let (src_w, src_h) = (p.canvas.0 as f64, p.canvas.1 as f64);
    let lay = math::layout(w, h, src_w, src_h, p);
    let unit = lay.unit;
    let xf = math::zoom_transform(w, h, &lay, p, t);

    let shadow = (st.shadow > 0.0).then(|| {
        let s = st.shadow / 100.0;
        let mut rect = xf.rect(lay.window);
        rect.y += s * 24.0 * unit;
        Shadow {
            rect,
            radius: lay.radius * xf.s,
            sigma: (s * 70.0 * unit / 2.0).max(0.01),
            alpha: 0.15 + s * 0.45,
        }
    });

    let content = lay.content;
    let to_canvas = |x: f64, y: f64| (content.x + x * content.w, content.y + y * content.h);

    let mut ripples = Vec::new();
    if st.click_ripple {
        for c in p.clicks.iter().rev() {
            if let Some((r, k)) = math::ripple(t - c.0, unit, st.cursor_size) {
                let (cx, cy) = to_canvas(c.1, c.2);
                let (sx, sy) = xf.apply(cx, cy);
                ripples.push([sx, sy, r * xf.s, k]);
                if ripples.len() == MAX_RIPPLES {
                    break;
                }
            }
        }
        // Oldest first, like the preview's array order.
        ripples.reverse();
    }

    let mut cursors = Vec::new();
    if p.draws_cursor() {
        let pos = |tt: f64| {
            let q = math::smoothed_cursor(&p.cursor, st.cursor_smoothing, tt);
            let (cx, cy) = to_canvas(q.0, q.1);
            xf.apply(cx, cy)
        };
        if st.motion_blur > 0.02 {
            for i in (1..=3).rev() {
                let q = pos(t - i as f64 * 18.0 * st.motion_blur * 2.0);
                cursors.push([q.0, q.1, 0.18 * st.motion_blur * (4 - i) as f64]);
            }
        }
        let q = pos(t);
        cursors.push([q.0, q.1, 1.0]);
    }

    let webcam = (st.webcam_enabled && has_webcam).then(|| {
        let sz = st.webcam_size * h;
        let m = 28.0 * unit;
        let (left, top) = st.webcam_corner;
        Webcam {
            rect: RectF {
                x: if left { m } else { w - m - sz },
                y: if top { m } else { h - m - sz },
                w: sz,
                h: sz,
            },
            radius: match st.webcam_shape {
                WebcamShape::Circle => sz / 2.0,
                WebcamShape::Rounded => sz * 0.22,
                WebcamShape::Square => 6.0 * unit,
            },
            shadow_sigma: 12.0 * unit,
        }
    });

    let caption = if st.captions_enabled {
        p.captions.iter().position(|c| t >= c.start && t <= c.end)
    } else {
        None
    };

    let video = xf.rect(content);
    let ratio = src_w / video.w.max(1.0);
    Scene {
        t_src: t,
        w,
        h,
        grad_line: math::gradient_line(&st.background, w, h),
        stops: st
            .background
            .stops
            .iter()
            .take(MAX_STOPS)
            .copied()
            .collect(),
        shadow,
        window: xf.rect(lay.window),
        radius: lay.radius * xf.s,
        bar_h: lay.bar_h * xf.s,
        frame: st.frame,
        unit_s: unit * xf.s,
        video,
        video_lod: ratio.log2().max(0.0),
        ripples,
        ripple_lw: 3.0 * unit * xf.s,
        cursors,
        cursor_scale: st.cursor_size * unit * xf.s,
        webcam,
        caption,
        keys: key_overlay(p, t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Easing, KeyPress, ZoomSegment};

    #[test]
    fn unzoomed_scene_has_padding_and_shadow() {
        let p = RenderProject::empty_for_tests();
        let s = build(&p, 1920.0, 1080.0, 0.0, false);
        // 8 % padding of min(1920, 1080*1.78) = 153.79 px.
        assert!(s.window.y > 150.0 && s.window.x > 150.0);
        let sh = s.shadow.unwrap();
        assert!((sh.alpha - 0.375).abs() < 1e-9);
        assert!((sh.rect.y - s.window.y - 12.0).abs() < 1e-9);
        assert_eq!(s.video_lod, 0.0_f64.max((1920.0 / s.video.w).log2()));
        assert!(s.cursors.is_empty() && s.webcam.is_none());
    }

    #[test]
    fn zoom_scales_window_and_lod() {
        let mut p = RenderProject::empty_for_tests();
        p.zooms = vec![ZoomSegment {
            start_ms: 0.0,
            end_ms: 2000.0,
            cx: 0.5,
            cy: 0.5,
            scale: 2.0,
            easing: Easing::Linear,
            transition_ms: 500.0,
        }];
        let a = build(&p, 1920.0, 1080.0, 3000.0, false);
        let b = build(&p, 1920.0, 1080.0, 1000.0, false);
        assert!((b.video.w / a.video.w - 2.0).abs() < 1e-9);
        assert!(b.video_lod < a.video_lod);
        // Shadow blur is device-space (unchanged by zoom), radius scales.
        assert_eq!(a.shadow.unwrap().sigma, b.shadow.unwrap().sigma);
        assert!((b.radius / a.radius - 2.0).abs() < 1e-9);
    }

    #[test]
    fn cursor_ghosts_ripples_keys() {
        let mut p = RenderProject::empty_for_tests();
        p.cursor = vec![(0.0, 0.1, 0.1), (1000.0, 0.9, 0.9)];
        p.clicks = vec![(400.0, 0.5, 0.5)];
        p.style.motion_blur = 0.5;
        p.style.show_keys = true;
        p.keys = vec![
            KeyPress {
                t: 100.0,
                label: "Ctrl + C".into(),
            },
            KeyPress {
                t: 300.0,
                label: "Ctrl + V".into(),
            },
        ];
        let s = build(&p, 1280.0, 720.0, 500.0, false);
        assert_eq!(s.cursors.len(), 4);
        assert_eq!(s.cursors[3][2], 1.0);
        assert!(s.cursors[0][2] < s.cursors[2][2]);
        assert_eq!(s.ripples.len(), 1);
        assert_eq!(s.keys.as_ref().unwrap().text, "Ctrl + C  Ctrl + V");
        assert!(build(&p, 1280.0, 720.0, 2000.0, false).keys.is_none());
        p.cursor_baked = true;
        assert!(build(&p, 1280.0, 720.0, 500.0, false).cursors.is_empty());
    }

    #[test]
    fn deterministic() {
        let mut p = RenderProject::empty_for_tests();
        p.cursor = vec![(0.0, 0.1, 0.1), (1000.0, 0.9, 0.9)];
        assert_eq!(
            build(&p, 1080.0, 1920.0, 777.0, true),
            build(&p, 1080.0, 1920.0, 777.0, true)
        );
    }
}
