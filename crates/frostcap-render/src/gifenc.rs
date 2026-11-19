//! GIF output: per-frame median-cut palette (own implementation, deterministic),
//! 4x4 ordered dithering, identical-frame merging, `gif` crate (MIT/Apache) LZW.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use crate::error::{RenderError, Result};

const BINS: usize = 1 << 15; // 5 bits per channel

const BAYER4: [[f32; 4]; 4] = [
    [0.0, 8.0, 2.0, 10.0],
    [12.0, 4.0, 14.0, 6.0],
    [3.0, 11.0, 1.0, 9.0],
    [15.0, 7.0, 13.0, 5.0],
];

#[inline]
fn bin(r: u8, g: u8, b: u8) -> usize {
    ((r as usize >> 3) << 10) | ((g as usize >> 3) << 5) | (b as usize >> 3)
}

#[derive(Clone, Copy)]
struct Bx {
    lo: [u8; 3],
    hi: [u8; 3],
    count: u64,
}

/// Median-cut quantizer over a 5-bit histogram. Returns (palette RGB, indices).
pub fn quantize(rgba: &[u8], w: usize, h: usize, max_colors: usize) -> (Vec<u8>, Vec<u8>) {
    // Dither first (so the palette covers the dithered colours).
    let mut px = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let o = (y * w + x) * 4;
            let d = (BAYER4[y & 3][x & 3] - 7.5) * 0.5;
            for c in 0..3 {
                px.push((rgba[o + c] as f32 + d).round().clamp(0.0, 255.0) as u8);
            }
        }
    }
    let mut count = vec![0u32; BINS];
    let mut sum = vec![[0u64; 3]; BINS];
    for p in px.as_chunks::<3>().0 {
        let i = bin(p[0], p[1], p[2]);
        count[i] += 1;
        for c in 0..3 {
            sum[i][c] += p[c] as u64;
        }
    }
    let coord = |i: usize| [(i >> 10) as u8, ((i >> 5) & 31) as u8, (i & 31) as u8];
    let box_stats = |lo: [u8; 3], hi: [u8; 3]| -> u64 {
        let mut n = 0u64;
        for r in lo[0]..=hi[0] {
            for g in lo[1]..=hi[1] {
                for b in lo[2]..=hi[2] {
                    n += count[((r as usize) << 10) | ((g as usize) << 5) | b as usize] as u64;
                }
            }
        }
        n
    };
    // Shrink a box to the bins actually used.
    let shrink = |lo: [u8; 3], hi: [u8; 3]| -> Option<Bx> {
        let mut nlo = [31u8; 3];
        let mut nhi = [0u8; 3];
        let mut n = 0u64;
        for r in lo[0]..=hi[0] {
            for g in lo[1]..=hi[1] {
                for b in lo[2]..=hi[2] {
                    let k = count[((r as usize) << 10) | ((g as usize) << 5) | b as usize];
                    if k > 0 {
                        n += k as u64;
                        for (c, v) in [r, g, b].into_iter().enumerate() {
                            nlo[c] = nlo[c].min(v);
                            nhi[c] = nhi[c].max(v);
                        }
                    }
                }
            }
        }
        (n > 0).then_some(Bx {
            lo: nlo,
            hi: nhi,
            count: n,
        })
    };
    let mut boxes: Vec<Bx> = shrink([0; 3], [31; 3]).into_iter().collect();
    while boxes.len() < max_colors {
        // Split the box with the largest count * longest side.
        let Some((bi, axis)) = boxes
            .iter()
            .enumerate()
            .filter_map(|(i, b)| {
                let (axis, len) = (0..3)
                    .map(|c| (c, b.hi[c] - b.lo[c]))
                    .max_by_key(|&(c, l)| (l, 2 - c))?;
                (len > 0).then_some((i, axis, b.count * len as u64))
            })
            .max_by_key(|&(i, _, score)| (score, usize::MAX - i))
            .map(|(i, a, _)| (i, a))
        else {
            break;
        };
        let b = boxes[bi];
        // Median along `axis`.
        let mut acc = 0u64;
        let mut cut = b.lo[axis];
        for v in b.lo[axis]..b.hi[axis] {
            let mut lo = b.lo;
            let mut hi = b.hi;
            lo[axis] = v;
            hi[axis] = v;
            acc += box_stats(lo, hi);
            cut = v;
            if acc * 2 >= b.count {
                break;
            }
        }
        let mut hi_a = b.hi;
        hi_a[axis] = cut;
        let mut lo_b = b.lo;
        lo_b[axis] = cut + 1;
        let a = shrink(b.lo, hi_a);
        let c = shrink(lo_b, b.hi);
        boxes.swap_remove(bi);
        boxes.extend(a);
        boxes.extend(c);
    }
    boxes.sort_by_key(|b| (b.lo, b.hi));
    let mut palette = Vec::with_capacity(boxes.len() * 3);
    for b in &boxes {
        let mut s = [0u64; 3];
        for r in b.lo[0]..=b.hi[0] {
            for g in b.lo[1]..=b.hi[1] {
                for bb in b.lo[2]..=b.hi[2] {
                    let i = ((r as usize) << 10) | ((g as usize) << 5) | bb as usize;
                    for c in 0..3 {
                        s[c] += sum[i][c];
                    }
                }
            }
        }
        for v in s {
            palette.push((v / b.count.max(1)) as u8);
        }
    }
    // Nearest palette entry per used bin (bin centre).
    let mut lut = vec![u8::MAX; BINS];
    let n_pal = palette.len() / 3;
    for (i, k) in count.iter().enumerate() {
        if *k == 0 {
            continue;
        }
        let c = coord(i).map(|v| (v as i32) * 8 + 4);
        let mut best = (i32::MAX, 0usize);
        for j in 0..n_pal {
            let d: i32 = (0..3)
                .map(|ch| {
                    let e = palette[j * 3 + ch] as i32 - c[ch];
                    e * e
                })
                .sum();
            if d < best.0 {
                best = (d, j);
            }
        }
        lut[i] = best.1 as u8;
    }
    let idx = px
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| lut[bin(p[0], p[1], p[2])])
        .collect();
    if palette.is_empty() {
        palette = vec![0, 0, 0];
    }
    (palette, idx)
}

/// Streaming GIF writer.
pub struct GifWriter {
    enc: gif::Encoder<BufWriter<File>>,
    w: u16,
    h: u16,
    pending: Option<(Vec<u8>, Vec<u8>, u64)>,
    /// Start time (centiseconds) of the pending frame.
    pending_cs: u64,
}

impl GifWriter {
    /// Create `path` (looping forever).
    pub fn create(path: &Path, w: u32, h: u32) -> Result<Self> {
        let f = File::create(path).map_err(|e| RenderError::io(path, e))?;
        let gerr = |e: gif::EncodingError| RenderError::Media(format!("gif: {e}"));
        let mut enc =
            gif::Encoder::new(BufWriter::new(f), w as u16, h as u16, &[]).map_err(gerr)?;
        enc.set_repeat(gif::Repeat::Infinite).map_err(gerr)?;
        Ok(Self {
            enc,
            w: w as u16,
            h: h as u16,
            pending: None,
            pending_cs: 0,
        })
    }

    /// Add an RGBA frame that starts at `t_ms` (output clock).
    pub fn push(&mut self, rgba: &[u8], t_ms: f64) -> Result<()> {
        let (pal, idx) = quantize(rgba, self.w as usize, self.h as usize, 256);
        let cs = (t_ms / 10.0).round() as u64;
        if let Some((p, i, _)) = &self.pending
            && *p == pal
            && *i == idx
        {
            return Ok(()); // identical: extend the pending frame
        }
        self.flush(cs)?;
        self.pending = Some((pal, idx, cs));
        self.pending_cs = cs;
        Ok(())
    }

    fn flush(&mut self, end_cs: u64) -> Result<()> {
        if let Some((pal, idx, start)) = self.pending.take() {
            let frame = gif::Frame {
                width: self.w,
                height: self.h,
                buffer: std::borrow::Cow::Owned(idx),
                palette: Some(pal),
                delay: (end_cs.saturating_sub(start)).clamp(2, u16::MAX as u64) as u16,
                ..Default::default()
            };
            self.enc
                .write_frame(&frame)
                .map_err(|e| RenderError::Media(format!("gif: {e}")))?;
        }
        Ok(())
    }

    /// Write the last frame (ending at `end_ms`) and the trailer.
    pub fn finish(mut self, end_ms: f64) -> Result<()> {
        let end = ((end_ms / 10.0).round() as u64).max(self.pending_cs + 2);
        self.flush(end)?;
        let mut inner = self
            .enc
            .into_inner()
            .map_err(|e| RenderError::Media(format!("gif: {e}")))?;
        std::io::Write::flush(&mut inner).map_err(|e| RenderError::Media(format!("gif: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantizes_few_colors_exactly() {
        let mut rgba = Vec::new();
        for i in 0..64 {
            let c = if i % 2 == 0 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            };
            rgba.extend(c);
        }
        let (pal, idx) = quantize(&rgba, 8, 8, 256);
        assert!(pal.len() / 3 <= 8);
        let first = &pal[idx[0] as usize * 3..idx[0] as usize * 3 + 3];
        assert!(first[0] > 240 && first[2] < 15);
        let second = &pal[idx[1] as usize * 3..idx[1] as usize * 3 + 3];
        assert!(second[2] > 240 && second[0] < 15);
    }

    #[test]
    fn gradient_fits_256_and_is_deterministic() {
        let (w, h) = (128, 32);
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                rgba.extend([(x * 2) as u8, (y * 8) as u8, 128, 255]);
            }
        }
        let a = quantize(&rgba, w, h, 256);
        let b = quantize(&rgba, w, h, 256);
        assert_eq!(a, b);
        assert!(a.0.len() / 3 <= 256);
        // Mean error stays small.
        let err: f64 = (0..w * h)
            .map(|i| {
                let p = a.1[i] as usize * 3;
                (0..3)
                    .map(|c| (a.0[p + c] as f64 - rgba[i * 4 + c] as f64).abs())
                    .sum::<f64>()
            })
            .sum::<f64>()
            / (w * h * 3) as f64;
        assert!(err < 6.0, "{err}");
    }
}
