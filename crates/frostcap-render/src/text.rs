//! Minimal text rasterizer for captions and the key overlay.
//!
//! Glyph outlines come from our own TrueType reader ([`crate::font`]) and are
//! filled with `ab_glyph_rasterizer` (Apache-2.0/MIT). The font is loaded at
//! runtime from the Windows font folder (Segoe UI Semibold = the preview's
//! `600 "Segoe UI"`); no font is bundled. Output is a premultiplied RGBA8
//! sprite placed in output pixels.

use std::path::PathBuf;

use ab_glyph_rasterizer::{Rasterizer, point};

use crate::font::{Pt, TtFont};

/// Premultiplied RGBA sprite at an integer output position.
#[derive(Clone, Debug, PartialEq)]
pub struct Overlay {
    /// Left (px).
    pub x: i32,
    /// Top (px).
    pub y: i32,
    /// Width (px).
    pub w: u32,
    /// Height (px).
    pub h: u32,
    /// Premultiplied RGBA8, `w * h * 4`.
    pub rgba: Vec<u8>,
}

/// A text annotation to rasterize into the annotation layer.
#[derive(Clone, Debug, PartialEq)]
pub struct TextItem {
    /// Text (wrapped to the box width, centred in the box).
    pub text: String,
    /// sRGB colour 0..1.
    pub color: [f32; 3],
    /// Opacity 0..1 (already faded).
    pub alpha: f32,
    /// Font size in output px.
    pub font_px: f32,
    /// Box (x0, y0, x1, y1) in output px.
    pub rect: [f32; 4],
}

/// A loaded font.
pub struct TextRenderer {
    font: TtFont,
}

/// Font files tried in order (Segoe UI Semibold first).
const FONT_CANDIDATES: &[&str] = &["seguisb.ttf", "segoeuib.ttf", "segoeui.ttf", "arial.ttf"];

/// Float coverage canvas, composited "source-over" in premultiplied space.
struct Canvas {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            px: vec![[0.0; 4]; w * h],
        }
    }
    fn blend(&mut self, x: usize, y: usize, c: [f32; 4], cov: f32) {
        let a = c[3] * cov;
        if a <= 0.0 {
            return;
        }
        let d = &mut self.px[y * self.w + x];
        for i in 0..3 {
            d[i] = c[i] * a + d[i] * (1.0 - a);
        }
        d[3] = a + d[3] * (1.0 - a);
    }
    /// Anti-aliased rounded rect fill.
    fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: [f32; 4]) {
        let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        for py in 0..self.h {
            for px in 0..self.w {
                let qx = (px as f32 + 0.5 - cx).abs() - (w / 2.0 - r);
                let qy = (py as f32 + 0.5 - cy).abs() - (h / 2.0 - r);
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                let d = outside + qx.max(qy).min(0.0) - r;
                let cov = (0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(px, py, c, cov);
                }
            }
        }
    }
    fn to_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.px.len() * 4);
        for p in &self.px {
            for v in p {
                out.push((v.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
        }
        out
    }
}

/// Three box blurs ~ Gaussian(sigma) on a single-channel mask.
fn blur(mask: &mut [f32], w: usize, h: usize, sigma: f32) {
    let r = ((sigma * (12.0f32 / 3.0 + 1.0).sqrt() / 2.0).round() as usize).max(1);
    let mut tmp = vec![0.0f32; mask.len()];
    for _ in 0..3 {
        for y in 0..h {
            let row = &mask[y * w..(y + 1) * w];
            let mut acc: f32 = row[..(r + 1).min(w)].iter().sum();
            for x in 0..w {
                tmp[y * w + x] = acc / (2 * r + 1) as f32;
                if x + r + 1 < w {
                    acc += row[x + r + 1];
                }
                if x >= r {
                    acc -= row[x - r];
                }
            }
        }
        for x in 0..w {
            let mut acc = 0.0;
            for y in 0..(r + 1).min(h) {
                acc += tmp[y * w + x];
            }
            for y in 0..h {
                mask[y * w + x] = acc / (2 * r + 1) as f32;
                if y + r + 1 < h {
                    acc += tmp[(y + r + 1) * w + x];
                }
                if y >= r {
                    acc -= tmp[(y - r) * w + x];
                }
            }
        }
    }
}

/// Rendered characters per caption cue.
const MAX_CUE_CHARS: usize = 500;

impl TextRenderer {
    /// Load the first available font from `%WINDIR%\Fonts`.
    pub fn load_system() -> Option<Self> {
        let dir = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("Fonts");
        FONT_CANDIDATES
            .iter()
            .find_map(|f| std::fs::read(dir.join(f)).ok().and_then(Self::from_bytes))
    }

    /// Use a TrueType font from memory.
    pub fn from_bytes(data: Vec<u8>) -> Option<Self> {
        TtFont::parse(data).map(|font| Self { font })
    }

    /// Pixels per font unit for a CSS font size (em) in px.
    fn scale(&self, em_px: f32) -> f32 {
        em_px / self.font.units_per_em()
    }

    /// Advance width of `s` (like `measureText().width`, without kerning).
    pub fn measure(&self, s: &str, em_px: f32) -> f32 {
        let k = self.scale(em_px);
        s.chars()
            .map(|c| self.font.advance(self.font.glyph_index(c)) * k)
            .sum()
    }

    /// Word wrap to `max_w`, at most 3 lines, the same algorithm as the editor
    /// preview (`wrapCaption` in `ui/src/lib/render.ts`): at most 500 chars are
    /// laid out; line width = sum of word widths + space widths (no kerning); a
    /// word wider than the line is hard-broken (never ellipsized) after flushing
    /// the current line; linear time; stops as soon as 3 lines are full.
    pub fn wrap(&self, text: &str, em_px: f32, max_w: f32) -> Vec<String> {
        const MAX_LINES: usize = 3;
        let capped: String = text.chars().take(MAX_CUE_CHARS).collect();
        let k = self.scale(em_px);
        let cw = |c: char| self.font.advance(self.font.glyph_index(c)) * k;
        let space = cw(' ');
        let mut out: Vec<String> = Vec::new();
        let mut line = String::new();
        let mut line_w = 0.0f32;
        // `true` when the 3-line limit is reached.
        let push = |out: &mut Vec<String>, s: String| {
            out.push(s);
            out.len() >= MAX_LINES
        };
        for word in capped.split_whitespace() {
            let w: f32 = word.chars().map(cw).sum();
            if w > max_w {
                if !line.is_empty() && push(&mut out, std::mem::take(&mut line)) {
                    return out;
                }
                let (mut chunk, mut chunk_w) = (String::new(), 0.0f32);
                for ch in word.chars() {
                    let c = cw(ch);
                    if !chunk.is_empty() && chunk_w + c > max_w {
                        if push(&mut out, std::mem::take(&mut chunk)) {
                            return out;
                        }
                        chunk_w = 0.0;
                    }
                    chunk.push(ch);
                    chunk_w += c;
                }
                line = chunk;
                line_w = chunk_w;
            } else if !line.is_empty() && line_w + space + w > max_w {
                if push(&mut out, std::mem::replace(&mut line, word.to_owned())) {
                    return out;
                }
                line_w = w;
            } else {
                line_w = if line.is_empty() {
                    w
                } else {
                    line_w + space + w
                };
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(word);
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
        out
    }

    /// Rasterize `s` centred at (`cx`, `cy`) (`textBaseline = middle`) into a mask.
    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        &self,
        mask: &mut [f32],
        w: usize,
        h: usize,
        s: &str,
        em_px: f32,
        cx: f32,
        cy: f32,
    ) {
        let k = self.scale(em_px);
        // "middle" = centre of the em box; ascent/descent normalized to 1 em.
        let (asc, desc) = (self.font.ascent(), self.font.descent());
        let baseline = cy + em_px * (asc / (asc - desc)) - em_px / 2.0;
        let mut x = cx - self.measure(s, em_px) / 2.0;
        for c in s.chars() {
            let gid = self.font.glyph_index(c);
            let contours = self.font.outline(gid);
            let origin = x;
            x += self.font.advance(gid) * k;
            let pts: Vec<(f32, f32)> = contours
                .iter()
                .flatten()
                .map(|p| (origin + p.0 * k, baseline - p.1 * k))
                .collect();
            if pts.is_empty() {
                continue;
            }
            let x0 = pts.iter().map(|p| p.0).fold(f32::MAX, f32::min).floor();
            let y0 = pts.iter().map(|p| p.1).fold(f32::MAX, f32::min).floor();
            let x1 = pts.iter().map(|p| p.0).fold(f32::MIN, f32::max).ceil();
            let y1 = pts.iter().map(|p| p.1).fold(f32::MIN, f32::max).ceil();
            // Glyphs that miss the canvas (or are absurdly large) are never rasterized.
            if x1 < 0.0
                || y1 < 0.0
                || x0 >= w as f32
                || y0 >= h as f32
                || (x1 - x0) > 8192.0
                || (y1 - y0) > 8192.0
            {
                continue;
            }
            let (gw, gh) = ((x1 - x0) as usize + 1, (y1 - y0) as usize + 1);
            let mut r = Rasterizer::new(gw, gh);
            let map = |p: &Pt| point(origin + p.0 * k - x0, baseline - p.1 * k - y0);
            for contour in &contours {
                draw_contour(&mut r, contour, &map);
            }
            r.for_each_pixel_2d(|gx, gy, a| {
                let (px, py) = (x0 as i32 + gx as i32, y0 as i32 + gy as i32);
                if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                    let m = &mut mask[py as usize * w + px as usize];
                    *m = (*m + a.abs().min(1.0)).min(1.0);
                }
            });
        }
    }

    /// Caption sprite, same layout as the preview (step 7 of `renderFrame`).
    /// Rasterize text annotations into one output-sized premultiplied RGBA8
    /// layer (`None` when nothing visible was drawn).
    pub fn annotation_layer(
        &self,
        items: &[TextItem],
        out_w: usize,
        out_h: usize,
    ) -> Option<Vec<u8>> {
        let mut layer = vec![0.0f32; out_w * out_h * 4];
        let mut any = false;
        for it in items {
            let text: String = it.text.chars().take(MAX_CUE_CHARS).collect();
            let box_w = (it.rect[2] - it.rect[0]).max(1.0);
            let lines = self.wrap(&text, it.font_px, box_w);
            if lines.is_empty() || it.alpha <= 0.0 {
                continue;
            }
            let lh = it.font_px * 1.3;
            let block_h = lines.len() as f32 * lh;
            let (cx, cy) = (
                (it.rect[0] + it.rect[2]) / 2.0,
                (it.rect[1] + it.rect[3]) / 2.0,
            );
            let mut mask = vec![0.0f32; out_w * out_h];
            for (i, l) in lines.iter().enumerate() {
                let ly = cy - block_h / 2.0 + lh / 2.0 + i as f32 * lh;
                self.draw_text(&mut mask, out_w, out_h, l, it.font_px, cx, ly);
            }
            for (i, m) in mask.iter().enumerate() {
                let a = m * it.alpha;
                if a > 0.0 {
                    any = true;
                    let d = &mut layer[i * 4..i * 4 + 4];
                    for c in 0..3 {
                        d[c] = it.color[c] * a + d[c] * (1.0 - a);
                    }
                    d[3] = a + d[3] * (1.0 - a);
                }
            }
        }
        any.then(|| {
            layer
                .iter()
                .map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8)
                .collect()
        })
    }

    /// Caption sprite.
    pub fn caption(
        &self,
        text: &str,
        out_w: f64,
        out_h: f64,
        font_1080: f64,
        top: bool,
        background: bool,
    ) -> Option<Overlay> {
        // At most 500 chars per cue are ever rendered (the wrap shows 3 lines anyway).
        let text: String = text.chars().take(MAX_CUE_CHARS).collect();
        let text = text.as_str();
        let unit = (out_h / 1080.0) as f32;
        let fs = font_1080 as f32 * unit;
        let (wf, hf) = (out_w as f32, out_h as f32);
        let lines = self.wrap(text, fs, wf * 0.8);
        if lines.is_empty() {
            return None;
        }
        let lh = fs * 1.3;
        let block_h = lines.len() as f32 * lh;
        let cy = if top {
            48.0 * unit + block_h / 2.0
        } else {
            hf - 48.0 * unit - block_h / 2.0
        };
        let max_line = lines
            .iter()
            .map(|l| self.measure(l, fs))
            .fold(0.0, f32::max);
        let margin = (12.0 * unit).ceil() + 2.0;
        let bw = max_line + fs * 1.2;
        let x0 = ((wf - bw) / 2.0 - margin).floor().max(0.0);
        let x1 = ((wf + bw) / 2.0 + margin).ceil().min(wf);
        let y0 = (cy - block_h / 2.0 - fs * 0.35 - margin).floor().max(0.0);
        let y1 = (cy + block_h / 2.0 + fs * 0.35 + margin).ceil().min(hf);
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        if w == 0 || h == 0 {
            return None;
        }
        let mut cv = Canvas::new(w, h);
        if background {
            cv.round_rect(
                wf / 2.0 - bw / 2.0 - x0,
                cy - block_h / 2.0 - fs * 0.35 - y0,
                bw,
                block_h + fs * 0.7,
                fs * 0.45,
                [8.0 / 255.0, 10.0 / 255.0, 18.0 / 255.0, 0.72],
            );
        }
        let mut mask = vec![0.0f32; w * h];
        for (i, l) in lines.iter().enumerate() {
            let ly = cy - block_h / 2.0 + lh / 2.0 + i as f32 * lh;
            self.draw_text(&mut mask, w, h, l, fs, wf / 2.0 - x0, ly - y0);
        }
        if !background {
            let mut sh = mask.clone();
            blur(&mut sh, w, h, 4.0 * unit);
            for (i, a) in sh.iter().enumerate() {
                cv.blend(i % w, i / w, [0.0, 0.0, 0.0, 0.8], *a);
            }
        }
        for (i, a) in mask.iter().enumerate() {
            cv.blend(i % w, i / w, [1.0, 1.0, 1.0, 1.0], *a);
        }
        Some(Overlay {
            x: x0 as i32,
            y: y0 as i32,
            w: w as u32,
            h: h as u32,
            rgba: cv.to_rgba8(),
        })
    }

    /// Key overlay pill, bottom centre; `bottom` is the y of its lower edge.
    pub fn keys(&self, text: &str, out_w: f64, out_h: f64, bottom: f64) -> Option<Overlay> {
        let unit = (out_h / 1080.0) as f32;
        let fs = 30.0 * unit;
        let tw = self.measure(text, fs);
        let (bw, bh) = (tw + fs * 1.4, fs * 1.9);
        let w = (bw.ceil() as usize + 2).min(out_w as usize);
        let h = bh.ceil() as usize + 2;
        if w == 0 {
            return None;
        }
        let mut cv = Canvas::new(w, h);
        cv.round_rect(
            1.0,
            1.0,
            bw,
            bh,
            bh / 2.0,
            [8.0 / 255.0, 10.0 / 255.0, 18.0 / 255.0, 0.78],
        );
        let mut mask = vec![0.0f32; w * h];
        self.draw_text(&mut mask, w, h, text, fs, 1.0 + bw / 2.0, 1.0 + bh / 2.0);
        for (i, a) in mask.iter().enumerate() {
            cv.blend(i % w, i / w, [1.0, 1.0, 1.0, 1.0], *a);
        }
        Some(Overlay {
            x: ((out_w as f32 - w as f32) / 2.0).round() as i32,
            y: (bottom as f32 - h as f32).round() as i32,
            w: w as u32,
            h: h as u32,
            rgba: cv.to_rgba8(),
        })
    }
}

/// Feed one TrueType contour (quadratic B-splines with implied on-curve
/// midpoints) to the rasterizer.
fn draw_contour(r: &mut Rasterizer, c: &[Pt], map: &impl Fn(&Pt) -> ab_glyph_rasterizer::Point) {
    let n = c.len();
    if n < 2 {
        return;
    }
    let mid = |a: &Pt, b: &Pt| ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0, true);
    // Start on an on-curve point (or the midpoint of two off-curve ones) and
    // walk the remaining points once, then close back to the start.
    let (start, seq): (Pt, Vec<Pt>) = match c.iter().position(|p| p.2) {
        Some(i) => (c[i], (1..n).map(|k| c[(i + k) % n]).collect()),
        None => (mid(&c[0], &c[1]), (1..=n).map(|k| c[k % n]).collect()),
    };
    let mut cur = start;
    let mut ctrl: Option<Pt> = None;
    for p in seq.into_iter().chain(std::iter::once(start)) {
        if p.2 {
            match ctrl.take() {
                Some(q) => r.draw_quad(map(&cur), map(&q), map(&p)),
                None => r.draw_line(map(&cur), map(&p)),
            }
            cur = p;
        } else if let Some(q) = ctrl {
            let m = mid(&q, &p);
            r.draw_quad(map(&cur), map(&q), map(&m));
            cur = m;
            ctrl = Some(p);
        } else {
            ctrl = Some(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Coverage area of a contour given in 0..10 units, rasterized at 10 px per
    /// unit (large enough that the rasterizer's quad flattening error is < 1 %),
    /// returned in units^2.
    fn area(c: &[Pt]) -> f32 {
        let mut r = Rasterizer::new(104, 104);
        draw_contour(&mut r, c, &|p: &Pt| {
            point(p.0 * 10.0 + 2.0, p.1 * 10.0 + 2.0)
        });
        let mut a = 0.0;
        r.for_each_pixel(|_, v| a += v.abs());
        a / 100.0
    }

    #[test]
    fn contour_quadratics_including_all_off_curve() {
        // On-curve square: area 100.
        let sq = [
            (0.0, 0.0, true),
            (10.0, 0.0, true),
            (10.0, 10.0, true),
            (0.0, 10.0, true),
        ];
        assert!((area(&sq) - 100.0).abs() < 0.5, "{}", area(&sq));
        // All off-curve corners: curve through edge midpoints, area 50 + 4 * (2/3 * 12.5).
        let off = sq.map(|p| (p.0, p.1, false));
        assert!((area(&off) - 83.33).abs() < 1.0, "{}", area(&off));
        // Mixed: on-curve start in the middle of the list.
        let mixed = [
            (10.0, 0.0, false),
            (10.0, 10.0, true),
            (0.0, 10.0, true),
            (0.0, 0.0, true),
        ];
        // Triangle (0,0)-(10,10)-(0,10) = 50, plus the parabola segment over the
        // chord (0,0)->(10,10) with control (10,0): 2/3 of triangle 50.
        assert!(
            (area(&mixed) - (50.0 + 2.0 / 3.0 * 50.0)).abs() < 1.0,
            "{}",
            area(&mixed)
        );
    }

    #[test]
    fn annotation_text_layer_is_boxed_premultiplied_and_scaled_by_alpha() {
        let Some(t) = TextRenderer::load_system() else {
            eprintln!("no system font; skipped");
            return;
        };
        let item = |text: &str, alpha: f32| TextItem {
            text: text.into(),
            color: [1.0, 1.0, 1.0],
            alpha,
            font_px: 40.0,
            rect: [100.0, 100.0, 300.0, 160.0],
        };
        let (w, h) = (400usize, 300usize);
        let layer = t.annotation_layer(&[item("Hello", 1.0)], w, h).unwrap();
        assert_eq!(layer.len(), w * h * 4);
        let px = |x: usize, y: usize| &layer[(y * w + x) * 4..(y * w + x) * 4 + 4];
        let inside = (100..300)
            .flat_map(|x| (100..160).map(move |y| (x, y)))
            .filter(|&(x, y)| px(x, y)[3] > 200)
            .count();
        assert!(inside > 100, "glyph pixels inside the box: {inside}");
        let outside = (0..w)
            .flat_map(|x| (0..h).map(move |y| (x, y)))
            .filter(|&(x, y)| !(90..310).contains(&x) || !(90..170).contains(&y))
            .filter(|&(x, y)| px(x, y)[3] != 0)
            .count();
        assert_eq!(outside, 0, "nothing far outside the box");
        let half = t.annotation_layer(&[item("Hello", 0.5)], w, h).unwrap();
        assert!(
            half.chunks(4).all(|p| p[3] <= 128 && p[0] <= p[3]),
            "premultiplied, alpha-scaled"
        );
        assert!(half.chunks(4).any(|p| p[3] > 100));
        // Long text wraps inside the box width; empty text draws nothing.
        let long = "word ".repeat(30);
        let wrapped = t.annotation_layer(&[item(&long, 1.0)], w, h).unwrap();
        let min_x = (0..w)
            .find(|&x| (0..h).any(|y| wrapped[(y * w + x) * 4 + 3] > 0))
            .unwrap();
        assert!(
            min_x >= 90,
            "wrapped text stays within the box width (min x {min_x})"
        );
        assert!(t.annotation_layer(&[item("   ", 1.0)], w, h).is_none());
        assert!(t.annotation_layer(&[], w, h).is_none());
    }

    #[test]
    fn caption_renders_with_system_font() {
        let Some(t) = TextRenderer::load_system() else {
            eprintln!("no system font; skipped");
            return;
        };
        let lines = t.wrap(
            "hello world this is a fairly long caption that should wrap onto lines",
            40.0,
            600.0,
        );
        assert!(lines.len() >= 2 && lines.len() <= 3);
        let o = t
            .caption("Hello captions", 1920.0, 1080.0, 40.0, false, true)
            .unwrap();
        assert!(o.y > 900 && (o.y as u32 + o.h) <= 1080);
        // White text pixels exist, and the box is dark.
        let white = o
            .rgba
            .chunks(4)
            .filter(|p| p[0] > 240 && p[3] > 240)
            .count();
        assert!(white > 50, "white {white}");
        let k = t.keys("Ctrl + S", 1920.0, 1080.0, 1000.0).unwrap();
        assert_eq!(k.y as u32 + k.h, 1000);
    }

    #[test]
    fn wrap_matches_the_editor_preview_algorithm() {
        let Some(t) = TextRenderer::load_system() else {
            eprintln!("no system font; skipped");
            return;
        };
        let (em, maxw) = (40.0f32, 600.0f32);
        // A long word after a short line: the line is flushed first (UI behaviour),
        // the word is hard-broken, and its last chunk starts the next line.
        let long = "M".repeat(60);
        let lines = t.wrap(&format!("hi {long}"), em, maxw);
        assert_eq!(lines[0], "hi");
        assert!(lines.len() == 3, "{lines:?}");
        // Chunks never exceed the line width and never lose characters.
        let joined: String = lines[1..].concat();
        assert!(long.starts_with(&joined));
        // 500-char cap: only the first 500 chars are ever laid out.
        let text = "ab ".repeat(1000);
        let shown: usize = t
            .wrap(&text, em, 100_000.0)
            .iter()
            .map(|l| l.chars().count())
            .sum();
        assert!(shown <= 500, "{shown}");
        // Normal wrapping: width = sum of word widths + spaces, <= 3 lines.
        let lines = t.wrap(
            "hello world this is a fairly long caption that should wrap onto lines",
            em,
            maxw,
        );
        assert!(lines.len() >= 2 && lines.len() <= 3);
        for l in &lines {
            assert!(t.measure(l, em) <= maxw + 0.01, "{l}");
        }
        assert!(t.wrap("   \n\t ", em, maxw).is_empty());
    }
    #[test]
    fn hostile_caption_text_is_bounded() {
        let Some(t) = TextRenderer::load_system() else {
            eprintln!("no system font; skipped");
            return;
        };
        // One enormous word: broken into lines that fit, at most 3 of them.
        let word = "W".repeat(5000);
        let lines = t.wrap(&word, 40.0, 600.0);
        assert!(
            !lines.is_empty() && lines.len() <= 3,
            "{} lines",
            lines.len()
        );
        for l in &lines {
            assert!(
                t.measure(l, 40.0) <= 600.0 + 1.0,
                "line too wide: {}",
                t.measure(l, 40.0)
            );
        }
        // A huge text is processed in (near) linear time, not quadratic.
        let big = "word ".repeat(400_000);
        let t0 = std::time::Instant::now();
        let lines = t.wrap(&big, 40.0, 600.0);
        assert!(lines.len() <= 3);
        assert!(
            t0.elapsed().as_secs_f64() < 2.0,
            "wrap took {:?}",
            t0.elapsed()
        );
        // Rendered chars per cue are capped at 500.
        let long = "ab ".repeat(2000);
        let o = t.caption(&long, 1920.0, 1080.0, 40.0, false, true).unwrap();
        let shown: usize = t
            .wrap(
                &long.chars().take(500).collect::<String>(),
                40.0 * 1.0,
                1920.0 * 0.8,
            )
            .iter()
            .map(|l| l.chars().filter(|c| !c.is_whitespace()).count())
            .sum();
        assert!(shown <= 500 && o.w > 0);
        // A huge font never allocates a glyph-sized raster: glyphs that miss the canvas are skipped.
        let mut mask = vec![0.0f32; 16 * 16];
        t.draw_text(&mut mask, 16, 16, "WWWW", 40.0, 1.0e7, 8.0);
        t.draw_text(&mut mask, 16, 16, "WWWW", 40.0, 8.0, -1.0e7);
        assert!(mask.iter().all(|m| *m == 0.0));
        t.draw_text(&mut mask, 16, 16, "W", 4.0e5, 8.0, 8.0); // bbox >> canvas: skipped, not allocated
    }
}
