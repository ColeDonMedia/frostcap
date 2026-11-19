//! GIF output: one global palette sampled from the first frames (own
//! deterministic median-cut), a 32 768-entry nearest-colour lookup table, 4x4
//! ordered dithering, identical-frame merging, `gif` crate (MIT/Apache) LZW.
//!
//! The palette is built once, after a short warm-up window of frames, instead of
//! per frame; every frame then costs one dither + table lookup per pixel.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use crate::error::{RenderError, Result};

const BINS: usize = 1 << 15; // 5 bits per channel
/// Frames buffered before the palette is built.
const WARMUP: usize = 12;
/// Every n-th warm-up frame contributes to the palette histogram.
const SAMPLE_EVERY: usize = 3;

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

/// Ordered-dithered RGB (3 bytes per pixel) of an RGBA frame.
fn dither(rgba: &[u8], w: usize, h: usize) -> Vec<u8> {
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
    px
}

/// Colour histogram (5 bits per channel) of dithered frames.
pub struct Histogram {
    count: Vec<u32>,
    sum: Vec<[u64; 3]>,
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

impl Histogram {
    /// Empty histogram.
    pub fn new() -> Self {
        Self {
            count: vec![0; BINS],
            sum: vec![[0; 3]; BINS],
        }
    }

    /// Add a `w x h` RGBA frame.
    pub fn add_frame(&mut self, rgba: &[u8], w: usize, h: usize) {
        for p in dither(rgba, w, h).as_chunks::<3>().0 {
            let i = bin(p[0], p[1], p[2]);
            self.count[i] += 1;
            for c in 0..3 {
                self.sum[i][c] += p[c] as u64;
            }
        }
    }
}

/// A palette plus the table that maps any 5-bit colour to its nearest entry.
pub struct Palette {
    /// RGB triplets (at least one entry).
    pub rgb: Vec<u8>,
    lut: Vec<u8>,
}

impl Palette {
    /// Median-cut `hist` down to `max_colors` entries (deterministic).
    pub fn build(hist: &Histogram, max_colors: usize) -> Self {
        let (count, sum) = (&hist.count, &hist.sum);
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
        let mut rgb = Vec::with_capacity(boxes.len() * 3);
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
                rgb.push((v / b.count.max(1)) as u8);
            }
        }
        if rgb.is_empty() {
            rgb = vec![0, 0, 0];
        }
        // Nearest palette entry for every 5-bit colour (bin centre), so frames
        // that contain colours the sample frames lacked still map sensibly.
        let n_pal = rgb.len() / 3;
        let mut lut = vec![0u8; BINS];
        for (i, slot) in lut.iter_mut().enumerate() {
            let c = coord(i).map(|v| (v as i32) * 8 + 4);
            let mut best = (i32::MAX, 0usize);
            for j in 0..n_pal {
                let d: i32 = (0..3)
                    .map(|ch| {
                        let e = rgb[j * 3 + ch] as i32 - c[ch];
                        e * e
                    })
                    .sum();
                if d < best.0 {
                    best = (d, j);
                }
            }
            *slot = best.1 as u8;
        }
        Self { rgb, lut }
    }

    /// Palette indices of a `w x h` RGBA frame (dithered, table lookup).
    pub fn apply(&self, rgba: &[u8], w: usize, h: usize) -> Vec<u8> {
        dither(rgba, w, h)
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| self.lut[bin(p[0], p[1], p[2])])
            .collect()
    }
}

/// Median-cut quantizer for one frame. Returns (palette RGB, indices).
pub fn quantize(rgba: &[u8], w: usize, h: usize, max_colors: usize) -> (Vec<u8>, Vec<u8>) {
    let mut hist = Histogram::new();
    hist.add_frame(rgba, w, h);
    let pal = Palette::build(&hist, max_colors);
    let idx = pal.apply(rgba, w, h);
    (pal.rgb, idx)
}

/// Streaming GIF writer.
pub struct GifWriter {
    enc: gif::Encoder<BufWriter<File>>,
    w: u16,
    h: u16,
    palette: Option<Palette>,
    hist: Histogram,
    /// Frames waiting for the palette (RGBA, start ms).
    warmup: Vec<(Vec<u8>, f64)>,
    seen: usize,
    pending: Option<(Vec<u8>, u64)>,
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
            palette: None,
            hist: Histogram::new(),
            warmup: Vec::new(),
            seen: 0,
            pending: None,
            pending_cs: 0,
        })
    }

    /// Add an RGBA frame that starts at `t_ms` (output clock). The first
    /// [`WARMUP`] frames are held back until the shared palette exists.
    pub fn push(&mut self, rgba: &[u8], t_ms: f64) -> Result<()> {
        if self.palette.is_none() {
            if self.seen.is_multiple_of(SAMPLE_EVERY) {
                self.hist.add_frame(rgba, self.w as usize, self.h as usize);
            }
            self.seen += 1;
            self.warmup.push((rgba.to_vec(), t_ms));
            if self.warmup.len() >= WARMUP {
                self.start_streaming()?;
            }
            return Ok(());
        }
        self.emit(rgba, t_ms)
    }

    fn start_streaming(&mut self) -> Result<()> {
        self.palette = Some(Palette::build(&self.hist, 256));
        for (f, t) in std::mem::take(&mut self.warmup) {
            self.emit(&f, t)?;
        }
        Ok(())
    }

    fn emit(&mut self, rgba: &[u8], t_ms: f64) -> Result<()> {
        let Some(pal) = &self.palette else {
            return Ok(());
        };
        let idx = pal.apply(rgba, self.w as usize, self.h as usize);
        let cs = (t_ms / 10.0).round() as u64;
        if let Some((i, _)) = &self.pending
            && *i == idx
        {
            return Ok(()); // identical: extend the pending frame
        }
        self.flush(cs)?;
        self.pending = Some((idx, cs));
        self.pending_cs = cs;
        Ok(())
    }

    fn flush(&mut self, end_cs: u64) -> Result<()> {
        if let Some((idx, start)) = self.pending.take() {
            let pal = self
                .palette
                .as_ref()
                .map_or_else(|| vec![0, 0, 0], |p| p.rgb.clone());
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
        if self.palette.is_none() && !self.warmup.is_empty() {
            self.start_streaming()?;
        }
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

    fn gradient_frame(w: usize, h: usize, shift: usize, blue: bool) -> Vec<u8> {
        let mut v = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let t = ((x + shift) * 255 / w) as u8;
                let u = (y * 255 / h) as u8;
                v.extend(if blue {
                    [u / 4, t / 2, 255 - t / 3, 255]
                } else {
                    [255 - t / 3, u / 2, t / 4, 255]
                });
            }
        }
        v
    }

    fn mean_error(pal: &[u8], idx: &[u8], rgba: &[u8]) -> f64 {
        let n = idx.len();
        (0..n)
            .map(|i| {
                let p = idx[i] as usize * 3;
                (0..3)
                    .map(|c| (f64::from(pal[p + c]) - f64::from(rgba[i * 4 + c])).abs())
                    .sum::<f64>()
            })
            .sum::<f64>()
            / (n * 3) as f64
    }

    #[test]
    fn one_palette_from_sampled_frames_serves_every_frame() {
        let (w, h) = (96, 64);
        let red = gradient_frame(w, h, 0, false);
        let blue = gradient_frame(w, h, 7, true);
        let mut hist = Histogram::new();
        hist.add_frame(&red, w, h);
        hist.add_frame(&blue, w, h);
        let pal = Palette::build(&hist, 256);
        assert!(pal.rgb.len() / 3 <= 256 && pal.rgb.len() >= 3);
        let again = Palette::build(&hist, 256);
        assert_eq!(pal.rgb, again.rgb, "deterministic");
        for frame in [&red, &blue] {
            let idx = pal.apply(frame, w, h);
            assert_eq!(idx.len(), w * h);
            assert!(mean_error(&pal.rgb, &idx, frame) < 9.0);
        }
        // The shared palette is the same table for both frames (one global colour table).
        let only_red = {
            let mut hr = Histogram::new();
            hr.add_frame(&red, w, h);
            Palette::build(&hr, 256)
        };
        let blue_with_red_palette = only_red.apply(&blue, w, h);
        let blue_with_both = pal.apply(&blue, w, h);
        assert!(
            mean_error(&pal.rgb, &blue_with_both, &blue)
                < mean_error(&only_red.rgb, &blue_with_red_palette, &blue),
            "sampling several frames beats the first frame alone"
        );
    }

    #[test]
    fn writer_uses_a_shared_palette_and_keeps_timing() {
        let dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")
            })
            .join("test-tmp");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("gif-shared-{}.gif", std::process::id()));
        let (w, h) = (64usize, 48usize);
        let mut g = GifWriter::create(&path, w as u32, h as u32).unwrap();
        let frames: Vec<Vec<u8>> = (0..30)
            .map(|i| gradient_frame(w, h, i * 2, i % 2 == 1))
            .collect();
        for (i, f) in frames.iter().enumerate() {
            g.push(f, i as f64 * 100.0).unwrap();
        }
        g.finish(3000.0).unwrap();
        let mut opts = gif::DecodeOptions::new();
        opts.set_color_output(gif::ColorOutput::Indexed);
        let mut dec = opts.read_info(std::fs::File::open(&path).unwrap()).unwrap();
        let mut count = 0;
        let mut total_cs = 0u32;
        let mut first_pal: Option<Vec<u8>> = None;
        while let Some(f) = dec.read_next_frame().unwrap() {
            count += 1;
            total_cs += u32::from(f.delay);
            let pal = f
                .palette
                .clone()
                .or_else(|| dec.global_palette().map(<[u8]>::to_vec))
                .unwrap();
            match &first_pal {
                None => first_pal = Some(pal),
                Some(p) => assert_eq!(*p, pal, "every frame uses the same palette"),
            }
        }
        assert_eq!(count, 30, "all frames (warm-up buffer flushed in order)");
        assert_eq!(total_cs, 300, "30 frames x 100 ms");
        let _ = std::fs::remove_file(&path);
        // Fewer frames than the warm-up window still produce a valid file.
        let short = dir.join(format!("gif-short-{}.gif", std::process::id()));
        let mut g = GifWriter::create(&short, w as u32, h as u32).unwrap();
        g.push(&frames[0], 0.0).unwrap();
        g.push(&frames[1], 100.0).unwrap();
        g.finish(250.0).unwrap();
        let mut dec = gif::DecodeOptions::new()
            .read_info(std::fs::File::open(&short).unwrap())
            .unwrap();
        let mut n = 0;
        while dec.read_next_frame().unwrap().is_some() {
            n += 1;
        }
        assert_eq!(n, 2);
        let _ = std::fs::remove_file(&short);
    }
}
