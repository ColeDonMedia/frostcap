//! Per-frame scene description: everything the GPU pass needs, in output pixels.
//! Pure CPU code (deterministic for a given project + time), unit tested.

use crate::math::{self, RectF};
use crate::model::{AnnKind, Frame, RenderProject, WebcamShape};

/// Maximum simultaneous click ripples / cursor draws passed to the shader.
pub const MAX_RIPPLES: usize = 8;
/// Main cursor + up to 3 motion-blur ghosts.
pub const MAX_CURSORS: usize = 4;
/// Maximum annotations drawn in one frame.
pub const MAX_ANNS: usize = 8;
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
    /// Border width (inside the rect), 0 = none.
    pub border: f64,
    /// Border colour (sRGB 0..1 RGBA).
    pub border_color: [f64; 4],
    /// Flip horizontally.
    pub mirror: bool,
    /// brightness, contrast, saturation, shadow strength.
    pub fx: [f64; 4],
    /// Chroma key.
    pub key: Option<crate::model::Key>,
    /// Replacement kind: 0 none, 1 colour, 2 gradient, 3 image, 4 blur.
    pub bg_kind: f64,
    /// Colour, or first gradient stop.
    pub bg_c1: [f64; 4],
    /// Last gradient stop.
    pub bg_c2: [f64; 4],
    /// Gradient angle (deg).
    pub bg_angle: f64,
    /// Blur amount.
    pub bg_blur: f64,
}

/// One visible annotation in output pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneAnn {
    /// Index into `RenderProject::annotations`.
    pub index: usize,
    /// Shape.
    pub kind: AnnKind,
    /// Rect (x0, y0, x1, y1) or arrow (x_start, y_start, x_end, y_end).
    pub geom: [f64; 4],
    /// Colour.
    pub color: [f64; 3],
    /// Opacity after fading.
    pub alpha: f64,
    /// Stroke width px.
    pub stroke: f64,
    /// Filled.
    pub fill: bool,
    /// Corner radius px.
    pub radius: f64,
    /// Blur / pixel amount 0..1.
    pub strength: f64,
    /// Spotlight dim.
    pub dim: f64,
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
    /// Device bezel thickness after zoom (0 for plain frames).
    pub bezel: f64,
    /// Laptop base height after zoom (0 otherwise).
    pub base_h: f64,
    /// Laptop base overhang per side after zoom.
    pub base_overhang: f64,
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
    /// Active annotations (at most [`MAX_ANNS`]).
    pub annotations: Vec<SceneAnn>,
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
        rect.h += lay.base_h * xf.s; // the laptop's base casts the same shadow
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
        let m = st.webcam_margin * h;
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
            border: st.webcam_border_width * unit,
            border_color: st.webcam_border_color,
            mirror: st.webcam_mirror,
            fx: [
                st.webcam_brightness,
                st.webcam_contrast,
                st.webcam_saturation,
                st.webcam_shadow,
            ],
            key: st.webcam_key,
            bg_kind: match &st.webcam_bg {
                crate::model::WebcamBg::None => 0.0,
                crate::model::WebcamBg::Color(_) => 1.0,
                crate::model::WebcamBg::Gradient(_) => 2.0,
                crate::model::WebcamBg::Image(_) => 3.0,
                crate::model::WebcamBg::Blur(_) => 4.0,
            },
            bg_c1: match &st.webcam_bg {
                crate::model::WebcamBg::Color(c) => *c,
                crate::model::WebcamBg::Gradient(g) => {
                    g.stops.first().copied().unwrap_or([0.0, 0.0, 0.0, 1.0])
                }
                _ => [0.0; 4],
            },
            bg_c2: match &st.webcam_bg {
                crate::model::WebcamBg::Gradient(g) => {
                    g.stops.last().copied().unwrap_or([0.0, 0.0, 0.0, 1.0])
                }
                _ => [0.0; 4],
            },
            bg_angle: match &st.webcam_bg {
                crate::model::WebcamBg::Gradient(g) => g.angle_deg,
                _ => 0.0,
            },
            bg_blur: match &st.webcam_bg {
                crate::model::WebcamBg::Blur(b) => *b,
                _ => 0.0,
            },
        }
    });

    let caption = if st.captions_enabled {
        p.captions.iter().position(|c| t >= c.start && t <= c.end)
    } else {
        None
    };

    let video = xf.rect(content);
    let annotations: Vec<SceneAnn> = p
        .annotations
        .iter()
        .enumerate()
        .filter(|(_, a)| t >= a.start && t <= a.end)
        .take(MAX_ANNS)
        .map(|(index, a)| {
            let k = if a.fade > 0.0 {
                ((t - a.start) / a.fade)
                    .min((a.end - t) / a.fade)
                    .clamp(0.0, 1.0)
            } else {
                1.0
            };
            let (px, py) = (video.x + a.x * video.w, video.y + a.y * video.h);
            let geom = if a.kind == AnnKind::Arrow {
                [px, py, video.x + a.x2 * video.w, video.y + a.y2 * video.h]
            } else {
                [px, py, px + a.w * video.w, py + a.h * video.h]
            };
            SceneAnn {
                index,
                kind: a.kind,
                geom,
                color: a.color,
                alpha: a.opacity * k,
                stroke: a.stroke * unit,
                fill: a.fill,
                radius: a.radius * unit,
                strength: a.strength,
                dim: a.dim,
            }
        })
        .collect();
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
        bezel: lay.bezel * xf.s,
        base_h: lay.base_h * xf.s,
        base_overhang: lay.base_overhang * xf.s,
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
        annotations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ann, Easing, KeyPress, ZoomSegment};

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
    fn webcam_margin_and_border_follow_the_style() {
        let mut p = RenderProject::empty_for_tests();
        p.style.webcam_enabled = true;
        p.style.webcam_size = 0.2;
        // Default margin is the old 28 px at 1080p, and scales with the height.
        let w = build(&p, 1920.0, 1080.0, 0.0, true).webcam.unwrap();
        assert!((1920.0 - w.rect.x1() - 28.0).abs() < 1e-9);
        assert!((1080.0 - w.rect.y1() - 28.0).abs() < 1e-9);
        assert_eq!(w.border, 0.0);
        let w = build(&p, 1280.0, 720.0, 0.0, true).webcam.unwrap();
        assert!((1280.0 - w.rect.x1() - 28.0 * 720.0 / 1080.0).abs() < 1e-9);
        p.style.webcam_margin = 0.1;
        p.style.webcam_border_width = 10.0;
        p.style.webcam_corner = (true, true);
        let w = build(&p, 1920.0, 1080.0, 0.0, true).webcam.unwrap();
        assert!((w.rect.x - 108.0).abs() < 1e-9 && (w.rect.y - 108.0).abs() < 1e-9);
        assert!((w.border - 10.0).abs() < 1e-9);
        // A margin of zero hugs the corner.
        p.style.webcam_margin = 0.0;
        let w = build(&p, 1920.0, 1080.0, 0.0, true).webcam.unwrap();
        assert_eq!((w.rect.x, w.rect.y), (0.0, 0.0));
    }

    #[test]
    fn laptop_shadow_covers_the_base_and_zoom_scales_the_device() {
        let mut p = RenderProject::empty_for_tests();
        p.style.frame = Frame::Laptop;
        let a = build(&p, 1920.0, 1080.0, 0.0, false);
        assert!(a.bezel > 0.0 && a.base_h > 0.0 && a.base_overhang > 0.0);
        let sh = a.shadow.unwrap();
        // Shadow rect = lid + base (offset down by the style's shadow shift).
        assert!((sh.rect.h - (a.window.h + a.base_h)).abs() < 1e-9);
        p.zooms = vec![ZoomSegment {
            start_ms: 0.0,
            end_ms: 2000.0,
            cx: 0.5,
            cy: 0.5,
            scale: 2.0,
            easing: Easing::Linear,
            transition_ms: 100.0,
        }];
        let z = build(&p, 1920.0, 1080.0, 1000.0, false);
        assert!((z.bezel / a.bezel - 2.0).abs() < 1e-9);
        assert!((z.base_h / a.base_h - 2.0).abs() < 1e-9);
        let mut ph = RenderProject::empty_for_tests();
        ph.style.frame = Frame::Phone;
        let s = build(&ph, 1920.0, 1080.0, 0.0, false);
        assert!(s.bezel > 0.0 && s.base_h == 0.0);
    }

    fn ann(kind: AnnKind, start: f64, end: f64) -> Ann {
        Ann {
            id: "a".into(),
            kind,
            start,
            end,
            x: 0.25,
            y: 0.5,
            w: 0.5,
            h: 0.25,
            x2: 0.75,
            y2: 0.9,
            color: [1.0, 0.0, 0.0],
            stroke: 10.0,
            fill: false,
            text: String::new(),
            font: 40.0,
            opacity: 0.8,
            strength: 0.5,
            dim: 0.6,
            radius: 12.0,
            fade: 200.0,
        }
    }

    #[test]
    fn annotations_are_timed_faded_and_follow_the_content_rect() {
        let mut p = RenderProject::empty_for_tests();
        p.annotations = vec![ann(AnnKind::Rect, 1000.0, 2000.0)];
        let at = |p: &RenderProject, t: f64| build(p, 1920.0, 1080.0, t, false);
        assert!(at(&p, 900.0).annotations.is_empty());
        assert!(at(&p, 2100.0).annotations.is_empty());
        let (a, b, c) = (at(&p, 1100.0), at(&p, 1500.0), at(&p, 1900.0));
        assert!(
            (a.annotations[0].alpha - 0.4).abs() < 1e-9,
            "fade in halfway"
        );
        assert!((b.annotations[0].alpha - 0.8).abs() < 1e-9);
        assert!(
            (c.annotations[0].alpha - 0.4).abs() < 1e-9,
            "fade out halfway"
        );
        // Geometry is the normalized rect inside the (unzoomed) video rect.
        let s = at(&p, 1500.0);
        let v = s.video;
        let g = s.annotations[0].geom;
        assert!(
            (g[0] - (v.x + 0.25 * v.w)).abs() < 1e-9 && (g[1] - (v.y + 0.5 * v.h)).abs() < 1e-9
        );
        assert!(
            (g[2] - (v.x + 0.75 * v.w)).abs() < 1e-9 && (g[3] - (v.y + 0.75 * v.h)).abs() < 1e-9
        );
        // Stroke and radius scale with the output size, not with the zoom.
        assert!((s.annotations[0].stroke - 10.0).abs() < 1e-9);
        let small = build(&p, 960.0, 540.0, 1500.0, false);
        assert!((small.annotations[0].stroke - 5.0).abs() < 1e-9);
        // Zoom 2x: the rect scales with the video, the stroke does not.
        p.zooms = vec![ZoomSegment {
            start_ms: 0.0,
            end_ms: 3000.0,
            cx: 0.5,
            cy: 0.5,
            scale: 2.0,
            easing: Easing::Linear,
            transition_ms: 0.0,
        }];
        let z = at(&p, 1500.0);
        let zg = z.annotations[0].geom;
        assert!(((zg[2] - zg[0]) / (g[2] - g[0]) - 2.0).abs() < 1e-9);
        assert!((z.annotations[0].stroke - 10.0).abs() < 1e-9);
        // Arrows keep their two endpoints in geom.
        p.zooms.clear();
        p.annotations = vec![ann(AnnKind::Arrow, 0.0, 3000.0)];
        let ar = at(&p, 1500.0).annotations[0].geom;
        assert!(
            (ar[0] - (v.x + 0.25 * v.w)).abs() < 1e-9 && (ar[3] - (v.y + 0.9 * v.h)).abs() < 1e-9
        );
        // No fade -> full opacity right at the start.
        p.annotations[0].fade = 0.0;
        assert!((at(&p, 0.0).annotations[0].alpha - 0.8).abs() < 1e-9);
    }

    #[test]
    fn at_most_eight_annotations_are_drawn_and_text_is_indexed() {
        let mut p = RenderProject::empty_for_tests();
        p.annotations = (0..12)
            .map(|_| ann(AnnKind::Highlight, 0.0, 5000.0))
            .collect();
        assert_eq!(
            build(&p, 1920.0, 1080.0, 100.0, false).annotations.len(),
            MAX_ANNS
        );
        p.annotations = vec![
            ann(AnnKind::Rect, 0.0, 10.0),
            ann(AnnKind::Text, 0.0, 5000.0),
        ];
        let s = build(&p, 1920.0, 1080.0, 100.0, false);
        assert_eq!(s.annotations.len(), 1);
        assert_eq!(
            s.annotations[0].index, 1,
            "index into RenderProject::annotations"
        );
        assert_eq!(s.annotations[0].kind, AnnKind::Text);
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
