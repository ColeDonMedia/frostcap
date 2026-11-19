//! Background image preparation (pure CPU, deterministic): cover/contain fit
//! into the output rectangle, bilinear resampling (box pre-shrink for big
//! sources) and a Gaussian blur approximated by three box passes. The result is
//! a premultiplied RGBA8 image of exactly the output size that the compositor
//! draws 1:1 over the gradient.

use crate::model::BgFit;

/// Decoded image: straight-alpha RGBA8, rows tightly packed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
    /// `w * h * 4` bytes.
    pub data: Vec<u8>,
}

/// Blur sigma in output pixels for `blur` px at 1080p on an output `out_h` high.
pub fn blur_sigma_px(blur: f64, out_h: u32) -> f64 {
    if blur.is_finite() && blur > 0.0 {
        blur * f64::from(out_h) / 1080.0
    } else {
        0.0
    }
}

/// Destination rectangle `(x, y, w, h)` of an `iw x ih` image in an `ow x oh`
/// output (centred; `Cover` overflows, `Contain` leaves bars).
pub fn fit_rect(iw: f64, ih: f64, ow: f64, oh: f64, fit: BgFit) -> (f64, f64, f64, f64) {
    let k = match fit {
        BgFit::Cover => (ow / iw).max(oh / ih),
        BgFit::Contain => (ow / iw).min(oh / ih),
    };
    let (w, h) = (iw * k, ih * k);
    ((ow - w) / 2.0, (oh - h) / 2.0, w, h)
}

/// Integer box-filter shrink by `k` in both axes (premultiplied f32 RGBA).
fn shrink(img: &RgbaImage, k: u32) -> Option<(u32, u32, Vec<f32>)> {
    let (nw, nh) = ((img.w / k).max(1), (img.h / k).max(1));
    let mut out: Vec<f32> = Vec::new();
    out.try_reserve_exact((nw * nh * 4) as usize).ok()?;
    out.resize((nw * nh * 4) as usize, 0.0);
    for y in 0..nh {
        for x in 0..nw {
            let mut acc = [0.0f32; 4];
            let mut n = 0.0;
            for sy in (y * k)..((y + 1) * k).min(img.h) {
                for sx in (x * k)..((x + 1) * k).min(img.w) {
                    let o = ((sy * img.w + sx) * 4) as usize;
                    let a = f32::from(img.data[o + 3]) / 255.0;
                    for c in 0..3 {
                        acc[c] += f32::from(img.data[o + c]) * a;
                    }
                    acc[3] += a * 255.0;
                    n += 1.0;
                }
            }
            let o = ((y * nw + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = acc[c] / n;
            }
        }
    }
    Some((nw, nh, out))
}

/// Premultiplied f32 RGBA (0..255 scale) from straight RGBA8.
fn premultiply(img: &RgbaImage) -> Option<Vec<f32>> {
    let mut v: Vec<f32> = Vec::new();
    v.try_reserve_exact(img.data.len()).ok()?;
    for p in img.data.as_chunks::<4>().0 {
        let a = f32::from(p[3]) / 255.0;
        v.extend([
            f32::from(p[0]) * a,
            f32::from(p[1]) * a,
            f32::from(p[2]) * a,
            f32::from(p[3]),
        ]);
    }
    Some(v)
}

/// Box widths (odd) for a 3-pass box blur approximating a Gaussian of `sigma`.
fn box_widths(sigma: f64) -> [usize; 3] {
    let n = 3.0;
    let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = ideal.floor() as i64;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wl = wl.max(1);
    let wu = wl + 2;
    let m = ((12.0 * sigma * sigma - n * (wl * wl) as f64 - 4.0 * n * wl as f64 - 3.0 * n)
        / (-4.0 * wl as f64 - 4.0))
        .round()
        .clamp(0.0, 3.0) as usize;
    let mut out = [wu as usize; 3];
    for w in out.iter_mut().take(m) {
        *w = wl as usize;
    }
    out
}

/// One horizontal (`stride = 4`) or vertical box pass with clamped edges.
fn box_pass(
    src: &[f32],
    dst: &mut [f32],
    len: usize,
    lines: usize,
    step: usize,
    line_step: usize,
    width: usize,
) {
    let r = (width / 2) as i64;
    let inv = 1.0 / width as f32;
    for l in 0..lines {
        let base = l * line_step;
        for c in 0..4 {
            let at = |i: i64| src[base + (i.clamp(0, len as i64 - 1) as usize) * step + c];
            let mut sum = 0.0f32;
            for i in -r..=r {
                sum += at(i);
            }
            for i in 0..len {
                dst[base + i * step + c] = sum * inv;
                sum += at(i as i64 + r + 1) - at(i as i64 - r);
            }
        }
    }
}

/// Gaussian-like blur of a premultiplied `w x h` buffer, in place.
fn blur(data: &mut [f32], w: usize, h: usize, sigma: f64) {
    if sigma < 0.3 || w == 0 || h == 0 {
        return;
    }
    let mut tmp = vec![0.0f32; data.len()];
    for width in box_widths(sigma) {
        if width <= 1 {
            continue;
        }
        box_pass(data, &mut tmp, w, h, 4, w * 4, width);
        box_pass(&tmp, data, h, w, w * 4, 4, width);
    }
}

/// Above this many output pixels a blurred background is composed and blurred at
/// half resolution (a blur hides the detail anyway) so the f32 work buffers stay
/// around 40 MB instead of gigabytes at 8K.
const HALF_RES_PIXELS: u64 = 2_500_000;

/// Largest output (pixels) a background is composed for: 8K UHD is 33 Mpx.
const MAX_OUTPUT_PIXELS: u64 = 70_000_000;

/// Divisor of the working resolution used for blurring an `ow x oh` output.
pub fn blur_working_scale(ow: u32, oh: u32) -> u32 {
    if u64::from(ow) * u64::from(oh) > HALF_RES_PIXELS {
        2
    } else {
        1
    }
}

/// Walk every output pixel of `img` fitted into `ow x oh` and hand its
/// premultiplied colour (0..255 scale) to `put(pixel index, rgba)`. Pixels the
/// image does not reach are not visited (the caller starts from zeros).
fn raster(
    img: &RgbaImage,
    ow: u32,
    oh: u32,
    fit: BgFit,
    put: &mut dyn FnMut(usize, [f32; 4]),
) -> Option<()> {
    let (dx, dy, dw, dh) = fit_rect(
        f64::from(img.w),
        f64::from(img.h),
        f64::from(ow),
        f64::from(oh),
        fit,
    );
    // Pre-shrink so the bilinear step never skips more than ~2 source pixels; the
    // f32 copy of the source is therefore at most ~2x the output size.
    let k = ((f64::from(img.w) / dw).min(f64::from(img.h) / dh).floor() as u32).max(1);
    let (sw, sh, src) = if k >= 2 {
        shrink(img, k)?
    } else {
        (img.w, img.h, premultiply(img)?)
    };
    let (fw, fh) = (f64::from(sw), f64::from(sh));
    for y in 0..oh {
        let cov_y = (f64::from(y + 1) - dy)
            .min(dy + dh - f64::from(y))
            .clamp(0.0, 1.0);
        if cov_y <= 0.0 {
            continue;
        }
        let v = ((f64::from(y) + 0.5 - dy) / dh * fh - 0.5).clamp(0.0, fh - 1.0);
        let (y0, fy) = (v.floor() as usize, (v - v.floor()) as f32);
        let y1 = (y0 + 1).min(sh as usize - 1);
        for x in 0..ow {
            let cov_x = (f64::from(x + 1) - dx)
                .min(dx + dw - f64::from(x))
                .clamp(0.0, 1.0);
            if cov_x <= 0.0 {
                continue;
            }
            let u = ((f64::from(x) + 0.5 - dx) / dw * fw - 0.5).clamp(0.0, fw - 1.0);
            let (x0, fx) = (u.floor() as usize, (u - u.floor()) as f32);
            let x1 = (x0 + 1).min(sw as usize - 1);
            let cov = (cov_x * cov_y) as f32;
            let mut px = [0.0f32; 4];
            for (c, slot) in px.iter_mut().enumerate() {
                let s = |xx: usize, yy: usize| src[(yy * sw as usize + xx) * 4 + c];
                let top = s(x0, y0) + (s(x1, y0) - s(x0, y0)) * fx;
                let bot = s(x0, y1) + (s(x1, y1) - s(x0, y1)) * fx;
                *slot = (top + (bot - top) * fy) * cov;
            }
            put((y * ow + x) as usize, px);
        }
    }
    Some(())
}

fn to_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// Fit `img` into an `ow x oh` output and blur it (`blur_sigma` output pixels).
/// Returns premultiplied RGBA8, transparent where the image does not reach, or
/// `None` when the memory for it cannot be reserved.
pub fn try_compose(
    img: &RgbaImage,
    ow: u32,
    oh: u32,
    fit: BgFit,
    blur_sigma: f64,
) -> Option<Vec<u8>> {
    let (ow, oh) = (ow.max(1), oh.max(1));
    if u64::from(ow) * u64::from(oh) > MAX_OUTPUT_PIXELS {
        return None;
    }
    let n = ow as usize * oh as usize * 4;
    if blur_sigma < 0.3 {
        // No blur: write bytes directly, no f32 buffer at all.
        let mut out: Vec<u8> = Vec::new();
        out.try_reserve_exact(n).ok()?;
        out.resize(n, 0);
        raster(img, ow, oh, fit, &mut |i, p| {
            for c in 0..4 {
                out[i * 4 + c] = to_u8(p[c]);
            }
        })?;
        return Some(out);
    }
    let s = blur_working_scale(ow, oh);
    let (bw, bh) = (ow.div_ceil(s), oh.div_ceil(s));
    let mut buf: Vec<f32> = Vec::new();
    buf.try_reserve_exact(bw as usize * bh as usize * 4).ok()?;
    buf.resize(bw as usize * bh as usize * 4, 0.0);
    raster(img, bw, bh, fit, &mut |i, p| {
        buf[i * 4..i * 4 + 4].copy_from_slice(&p)
    })?;
    blur(
        &mut buf,
        bw as usize,
        bh as usize,
        blur_sigma / f64::from(s),
    );
    let mut out: Vec<u8> = Vec::new();
    out.try_reserve_exact(n).ok()?;
    if s == 1 {
        out.extend(buf.iter().map(|v| to_u8(*v)));
        return Some(out);
    }
    // Bilinear upsample of the half-resolution result.
    for y in 0..oh {
        let fy = ((f64::from(y) + 0.5) / f64::from(s) - 0.5).clamp(0.0, f64::from(bh - 1));
        let (y0, ty) = (fy.floor() as usize, (fy - fy.floor()) as f32);
        let y1 = (y0 + 1).min(bh as usize - 1);
        for x in 0..ow {
            let fx = ((f64::from(x) + 0.5) / f64::from(s) - 0.5).clamp(0.0, f64::from(bw - 1));
            let (x0, tx) = (fx.floor() as usize, (fx - fx.floor()) as f32);
            let x1 = (x0 + 1).min(bw as usize - 1);
            for c in 0..4 {
                let g = |xx: usize, yy: usize| buf[(yy * bw as usize + xx) * 4 + c];
                let top = g(x0, y0) + (g(x1, y0) - g(x0, y0)) * tx;
                let bot = g(x0, y1) + (g(x1, y1) - g(x0, y1)) * tx;
                out.push(to_u8(top + (bot - top) * ty));
            }
        }
    }
    Some(out)
}

/// [`try_compose`] that returns an empty buffer (ignored by the compositor) when
/// memory runs out; exports use [`try_compose`] and fail cleanly instead.
pub fn compose(img: &RgbaImage, ow: u32, oh: u32, fit: BgFit, blur_sigma: f64) -> Vec<u8> {
    try_compose(img, ow, oh, fit, blur_sigma).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, c: [u8; 4]) -> RgbaImage {
        RgbaImage {
            w,
            h,
            data: c.repeat((w * h) as usize),
        }
    }

    fn px(d: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let o = ((y * w + x) * 4) as usize;
        [d[o], d[o + 1], d[o + 2], d[o + 3]]
    }

    #[test]
    fn fit_rect_cover_and_contain() {
        // 2:1 source into a square output.
        let (x, y, w, h) = fit_rect(200.0, 100.0, 100.0, 100.0, BgFit::Cover);
        assert_eq!((x, y, w, h), (-50.0, 0.0, 200.0, 100.0));
        let (x, y, w, h) = fit_rect(200.0, 100.0, 100.0, 100.0, BgFit::Contain);
        assert_eq!((x, y, w, h), (0.0, 25.0, 100.0, 50.0));
        // 1:2 source into a wide output.
        let (x, y, w, h) = fit_rect(100.0, 200.0, 200.0, 100.0, BgFit::Cover);
        assert_eq!((x, y, w, h), (0.0, -150.0, 200.0, 400.0));
        let (x, y, w, h) = fit_rect(100.0, 200.0, 200.0, 100.0, BgFit::Contain);
        assert_eq!((x, y, w, h), (75.0, 0.0, 50.0, 100.0));
    }

    #[test]
    fn cover_fills_and_contain_leaves_a_transparent_letterbox() {
        let red = solid(4, 4, [255, 0, 0, 255]);
        let cover = compose(&red, 8, 4, BgFit::Cover, 0.0);
        assert_eq!(cover.len(), 8 * 4 * 4);
        for x in 0..8 {
            assert_eq!(px(&cover, 8, x, 2), [255, 0, 0, 255], "cover x={x}");
        }
        let contain = compose(&red, 8, 4, BgFit::Contain, 0.0);
        assert_eq!(px(&contain, 8, 0, 2)[3], 0);
        assert_eq!(px(&contain, 8, 1, 2)[3], 0);
        assert_eq!(px(&contain, 8, 2, 2), [255, 0, 0, 255]);
        assert_eq!(px(&contain, 8, 5, 2), [255, 0, 0, 255]);
        assert_eq!(px(&contain, 8, 6, 2)[3], 0);
        assert_eq!(px(&contain, 8, 7, 2)[3], 0);
    }

    #[test]
    fn output_is_premultiplied() {
        let half = solid(2, 2, [200, 100, 0, 128]);
        let out = compose(&half, 2, 2, BgFit::Cover, 0.0);
        let p = px(&out, 2, 0, 0);
        assert_eq!(p[3], 128);
        assert!((i32::from(p[0]) - 100).abs() <= 1 && (i32::from(p[1]) - 50).abs() <= 1);
    }

    #[test]
    fn bilinear_resampling_blends_and_big_sources_are_shrunk_without_aliasing() {
        // Left half black, right half white, 2x1 stretched to 8x1: smooth ramp.
        let img = RgbaImage {
            w: 2,
            h: 1,
            data: vec![0, 0, 0, 255, 255, 255, 255, 255],
        };
        let out = compose(&img, 8, 1, BgFit::Cover, 0.0);
        let row: Vec<u8> = (0..8).map(|x| px(&out, 8, x, 0)[0]).collect();
        assert!(row.windows(2).all(|w| w[0] <= w[1]), "{row:?}");
        assert!(row[0] < 40 && row[7] > 215, "{row:?}");
        // Checkerboard of 1 px squares, 64x64 -> 8x8: every output pixel is ~mid grey.
        let mut data = Vec::new();
        for y in 0..64u32 {
            for x in 0..64u32 {
                let v = if (x + y) % 2 == 0 { 0 } else { 255 };
                data.extend([v, v, v, 255]);
            }
        }
        let out = compose(&RgbaImage { w: 64, h: 64, data }, 8, 8, BgFit::Cover, 0.0);
        for y in 0..8 {
            for x in 0..8 {
                let v = i32::from(px(&out, 8, x, y)[0]);
                assert!((v - 128).abs() <= 3, "aliased pixel {v} at {x},{y}");
            }
        }
    }

    #[test]
    fn blur_softens_edges_keeps_flat_colour_and_is_deterministic() {
        // Flat colour stays flat everywhere, including at the borders.
        let flat = compose(
            &solid(16, 16, [10, 120, 250, 255]),
            32,
            32,
            BgFit::Cover,
            6.0,
        );
        for (x, y) in [(0, 0), (31, 31), (0, 31), (16, 16)] {
            let p = px(&flat, 32, x, y);
            assert!(
                (i32::from(p[0]) - 10).abs() <= 1
                    && (i32::from(p[1]) - 120).abs() <= 1
                    && (i32::from(p[2]) - 250).abs() <= 1
                    && p[3] == 255,
                "{p:?} at {x},{y}"
            );
        }
        // A hard vertical edge: sharp without blur, a ramp with it.
        let mut data = Vec::new();
        for _y in 0..8 {
            for x in 0..32u32 {
                let v = if x < 16 { 0 } else { 255 };
                data.extend([v, v, v, 255]);
            }
        }
        let img = RgbaImage { w: 32, h: 8, data };
        let sharp = compose(&img, 32, 8, BgFit::Cover, 0.0);
        assert_eq!(
            (px(&sharp, 32, 15, 4)[0], px(&sharp, 32, 16, 4)[0]),
            (0, 255)
        );
        let soft = compose(&img, 32, 8, BgFit::Cover, 3.0);
        let (a, b) = (px(&soft, 32, 15, 4)[0], px(&soft, 32, 16, 4)[0]);
        assert!(a > 20 && a < 128 && b > 128 && b < 235, "{a} {b}");
        let col: Vec<u8> = (0..32).map(|x| px(&soft, 32, x, 4)[0]).collect();
        assert!(col.windows(2).all(|w| w[0] <= w[1]), "{col:?}");
        assert_eq!(soft, compose(&img, 32, 8, BgFit::Cover, 3.0));
    }

    #[test]
    fn big_blurred_outputs_use_a_reduced_working_resolution() {
        assert_eq!(blur_working_scale(1920, 1080), 1);
        assert_eq!(blur_working_scale(3840, 2160), 2);
        assert_eq!(blur_working_scale(7680, 4320), 2);
        // A flat 4K background with blur stays flat, full size, premultiplied.
        let out = try_compose(
            &solid(64, 36, [40, 90, 200, 255]),
            3840,
            2160,
            BgFit::Cover,
            30.0,
        )
        .unwrap();
        assert_eq!(out.len(), 3840 * 2160 * 4);
        for (x, y) in [(0u32, 0u32), (3839, 2159), (1920, 1080), (0, 2159)] {
            let p = px(&out, 3840, x, y);
            assert!(
                (i32::from(p[0]) - 40).abs() <= 1
                    && (i32::from(p[1]) - 90).abs() <= 1
                    && (i32::from(p[2]) - 200).abs() <= 1
                    && p[3] == 255,
                "{p:?} at {x},{y}"
            );
        }
        // A huge source is shrunk straight from the bytes; no full-size f32 copy.
        let huge = solid(6000, 4000, [255, 255, 255, 255]);
        let out = try_compose(&huge, 320, 180, BgFit::Cover, 0.0).unwrap();
        assert_eq!(px(&out, 320, 160, 90), [255, 255, 255, 255]);
        // An absurd output is refused (no allocation attempt that aborts the process).
        assert!(try_compose(&solid(2, 2, [0; 4]), 60_000, 60_000, BgFit::Cover, 0.0).is_none());
        assert!(compose(&solid(2, 2, [0; 4]), 60_000, 60_000, BgFit::Cover, 0.0).is_empty());
    }

    #[test]
    fn blur_sigma_scales_with_the_output_height() {
        assert_eq!(blur_sigma_px(20.0, 1080), 20.0);
        assert_eq!(blur_sigma_px(20.0, 540), 10.0);
        assert_eq!(blur_sigma_px(0.0, 2160), 0.0);
        assert!(blur_sigma_px(f64::NAN, 1080) == 0.0);
    }
}
