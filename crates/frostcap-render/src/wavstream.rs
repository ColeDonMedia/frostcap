//! Bounded-memory WAV decoding: PCM / float / RF64 read in chunks, downmixed to
//! stereo f32 and linearly resampled to 48 kHz on the fly. Nothing here ever
//! holds more than one chunk plus a two-frame resampler window, whatever the
//! file length (RF64 files over 4 GiB included).

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::audio::RATE;
use crate::error::{RenderError, Result};

/// Frames per decoded chunk (and the most any caller ever sees at once).
pub const CHUNK_FRAMES: usize = 1 << 16;
/// Bytes of header searched for `fmt ` / `data`.
const HEAD_BYTES: u64 = 64 * 1024;

/// What a caller allows: decoding stops after `max_out_frames` 48 kHz frames,
/// and a file whose format disagrees with the project's is refused.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Most 48 kHz frames delivered.
    pub max_out_frames: u64,
    /// Sample rate the project declares for this file.
    pub expect_rate: Option<u32>,
    /// Channel count the project declares for this file.
    pub expect_channels: Option<u16>,
}

impl Limits {
    /// No limits (tests, tools).
    pub const NONE: Self = Self {
        max_out_frames: u64::MAX,
        expect_rate: None,
        expect_channels: None,
    };
}

fn le16(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}
fn le32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// A WAV positioned at its first sample.
pub struct WavStream<R> {
    r: R,
    /// Sample rate (Hz).
    pub rate: u32,
    channels: u16,
    tag: u16,
    bits: u16,
    bps: usize,
    total: u64,
    read: u64,
    buf: Vec<u8>,
}

impl<R: Read + Seek> WavStream<R> {
    /// Parse the header (RIFF / RF64 / BW64, JUNK and unknown chunks skipped) and
    /// seek to the data. `file_len` bounds what the data chunk can really hold.
    pub fn from_reader(mut r: R, file_len: u64, name: &str) -> Result<Self> {
        let err = |m: &str| RenderError::Audio(format!("{name}: {m}"));
        let mut b = Vec::new();
        r.by_ref()
            .take(HEAD_BYTES.min(file_len))
            .read_to_end(&mut b)
            .map_err(|e| RenderError::Audio(format!("{name}: {e}")))?;
        let rf64 = b.len() >= 4 && matches!(&b[0..4], b"RF64" | b"BW64");
        if b.len() < 12 || !(rf64 || &b[0..4] == b"RIFF") || &b[8..12] != b"WAVE" {
            return Err(err("not a RIFF/WAVE file"));
        }
        let mut i = 12usize;
        let mut fmt: Option<(u16, u16, u32, u16)> = None;
        let mut ds64: Option<u64> = None;
        let mut data: Option<(u64, u64)> = None;
        while i + 8 <= b.len() {
            let id = &b[i..i + 4];
            if rf64 && i == 12 && id != b"ds64" {
                return Err(err("RF64 file without a ds64 chunk"));
            }
            let size32 = le32(&b, i + 4);
            let body = i + 8;
            if id == b"data" {
                let mut size = u64::from(size32);
                if size32 == u32::MAX
                    && let Some(d) = ds64
                {
                    size = d;
                }
                data = Some((body as u64, size));
                break;
            }
            let end = body.saturating_add(size32 as usize).min(b.len());
            let chunk = &b[body.min(b.len())..end];
            if id == b"ds64" {
                if chunk.len() < 28 {
                    return Err(err("ds64 chunk is too short"));
                }
                let mut d = [0u8; 8];
                d.copy_from_slice(&chunk[8..16]);
                ds64 = Some(u64::from_le_bytes(d));
            } else if id == b"fmt " && chunk.len() >= 16 {
                let mut tag = le16(chunk, 0);
                if tag == 0xFFFE && chunk.len() >= 26 {
                    tag = le16(chunk, 24);
                }
                fmt = Some((tag, le16(chunk, 2), le32(chunk, 4), le16(chunk, 14)));
            }
            i = body
                .saturating_add(size32 as usize)
                .saturating_add((size32 & 1) as usize);
        }
        let (tag, ch, rate, bits) = fmt.ok_or_else(|| err("missing fmt chunk before the data"))?;
        let (data_start, declared) = data.ok_or_else(|| err("missing data chunk"))?;
        if !(1..=32).contains(&ch)
            || !(8_000..=768_000).contains(&rate)
            || !matches!(bits, 8 | 16 | 24 | 32 | 64)
            || !matches!((tag, bits), (1, 8 | 16 | 24 | 32) | (3, 32 | 64))
        {
            return Err(err("invalid or unsupported format"));
        }
        let bps = (bits as usize).div_ceil(8);
        let frame = (bps * ch as usize) as u64;
        let avail = file_len.saturating_sub(data_start);
        let total = declared.min(avail) / frame;
        r.seek(SeekFrom::Start(data_start))
            .map_err(|e| RenderError::Audio(format!("{name}: {e}")))?;
        Ok(Self {
            r,
            rate,
            channels: ch,
            tag,
            bits,
            bps,
            total,
            read: 0,
            buf: Vec::new(),
        })
    }

    /// Frames the data chunk holds (after any [`Self::limit_frames`]).
    pub fn frames(&self) -> u64 {
        self.total
    }

    /// Channel count of the file.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Never deliver more than `n` frames.
    pub fn limit_frames(&mut self, n: u64) {
        self.total = self.total.min(n);
    }

    fn sample(&self, s: &[u8]) -> f32 {
        match (self.tag, self.bits) {
            (1, 8) => (f32::from(s[0]) - 128.0) / 128.0,
            (1, 16) => f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0,
            (1, 24) => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
            (1, 32) => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
            (3, 32) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            _ => {
                let mut d = [0u8; 8];
                d.copy_from_slice(&s[..8]);
                f64::from_le_bytes(d) as f32
            }
        }
    }

    /// Append up to `max` frames as interleaved stereo f32 (mono duplicated,
    /// extra channels dropped, non-finite samples zeroed). Returns the count.
    pub fn read_frames(&mut self, max: usize, out: &mut Vec<f32>) -> Result<usize> {
        let want = (self.total - self.read).min(max as u64) as usize;
        let frame = self.bps * self.channels as usize;
        self.buf.clear();
        self.buf
            .try_reserve(want * frame)
            .map_err(|_| RenderError::Audio("out of memory reading audio".into()))?;
        self.buf.resize(want * frame, 0);
        let mut got = 0;
        while got < self.buf.len() {
            match self.r.read(&mut self.buf[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) => return Err(RenderError::Audio(e.to_string())),
            }
        }
        let n = got / frame;
        out.try_reserve(n * 2)
            .map_err(|_| RenderError::Audio("out of memory reading audio".into()))?;
        let fix = |x: f32| if x.is_finite() { x } else { 0.0 };
        for f in 0..n {
            let o = f * frame;
            let l = self.sample(&self.buf[o..o + self.bps]);
            let r = if self.channels > 1 {
                self.sample(&self.buf[o + self.bps..o + 2 * self.bps])
            } else {
                l
            };
            out.push(fix(l));
            out.push(fix(r));
        }
        self.read += n as u64;
        if n < want {
            self.total = self.read; // the file ended early
        }
        Ok(n)
    }
}

/// Decode a reader to 48 kHz stereo, calling `f` with chunks of at most
/// [`CHUNK_FRAMES`] frames. Linear resampling identical to
/// [`crate::audio::resample`] on the whole file.
pub fn stream_48k_reader<R: Read + Seek>(
    r: R,
    file_len: u64,
    name: &str,
    f: &mut dyn FnMut(&[f32]),
) -> Result<()> {
    stream_48k_limited(r, file_len, name, Limits::NONE, f)
}

/// Pull-based 48 kHz stereo decoder: [`Pcm48k::next_chunk`] appends the next
/// chunk of at most `chunk` frames. Linear resampling identical to
/// [`crate::audio::resample`] on the whole file.
pub struct Pcm48k<R> {
    ws: WavStream<R>,
    chunk: usize,
    passthrough: bool,
    left: u64,
    step: f64,
    n_in: u64,
    n_out: u64,
    win: Vec<f32>,
    base: u64,
    k: u64,
}

impl<R: Read + Seek> Pcm48k<R> {
    /// Open and validate (format vs `lim`), ready to deliver the first chunk.
    pub fn new(r: R, file_len: u64, name: &str, lim: Limits) -> Result<Self> {
        let mut ws = WavStream::from_reader(r, file_len, name)?;
        if lim.expect_rate.is_some_and(|x| x != ws.rate)
            || lim.expect_channels.is_some_and(|x| x != ws.channels)
        {
            return Err(RenderError::Audio(format!(
                "{name}: format {} Hz x{} does not match the project ({:?} Hz x{:?})",
                ws.rate, ws.channels, lim.expect_rate, lim.expect_channels
            )));
        }
        // Input frames that can produce `max_out_frames` outputs (+ interpolation margin).
        let need_in =
            (u128::from(lim.max_out_frames) * u128::from(ws.rate)).div_ceil(u128::from(RATE)) + 2;
        ws.limit_frames(u64::try_from(need_in).unwrap_or(u64::MAX));
        let n_in = ws.frames();
        let passthrough = ws.rate == RATE || n_in < 2;
        let step = f64::from(ws.rate) / f64::from(RATE);
        let n_out = if passthrough {
            lim.max_out_frames
        } else {
            (n_in * u64::from(RATE) / u64::from(ws.rate)).min(lim.max_out_frames)
        };
        Ok(Self {
            ws,
            chunk: CHUNK_FRAMES,
            passthrough,
            left: lim.max_out_frames,
            step,
            n_in,
            n_out,
            win: Vec::new(),
            base: 0,
            k: 0,
        })
    }

    /// Frames per chunk (default [`CHUNK_FRAMES`]).
    pub fn set_chunk(&mut self, n: usize) {
        self.chunk = n.clamp(2, CHUNK_FRAMES);
    }

    /// Append the next chunk to `out`; `false` once the stream is finished.
    pub fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool> {
        if self.passthrough {
            let want = self.chunk.min(self.left.min(usize::MAX as u64) as usize);
            let n = self.ws.read_frames(want, out)?;
            self.left -= n as u64;
            return Ok(n > 0);
        }
        let start = out.len();
        let target = start + self.chunk * 2;
        loop {
            while self.k < self.n_out && out.len() < target {
                let pos = self.k as f64 * self.step;
                let i = (pos as u64).min(self.n_in - 2);
                if i + 1 >= self.base + (self.win.len() / 2) as u64 {
                    break;
                }
                let fr = (pos - i as f64) as f32;
                let (a, b) = (
                    ((i - self.base) * 2) as usize,
                    ((i + 1 - self.base) * 2) as usize,
                );
                for c in 0..2 {
                    out.push(self.win[a + c] + (self.win[b + c] - self.win[a + c]) * fr);
                }
                self.k += 1;
            }
            if out.len() >= target {
                return Ok(true);
            }
            if self.k >= self.n_out {
                return Ok(out.len() > start);
            }
            let keep_from =
                (((self.k as f64 * self.step) as u64).min(self.n_in - 2)).max(self.base);
            if keep_from > self.base {
                self.win.drain(..((keep_from - self.base) * 2) as usize);
                self.base = keep_from;
            }
            if self.ws.read_frames(self.chunk, &mut self.win)? == 0 {
                self.k = self.n_out;
                return Ok(out.len() > start);
            }
        }
    }
}

/// [`stream_48k_reader`] with [`Limits`].
pub fn stream_48k_limited<R: Read + Seek>(
    r: R,
    file_len: u64,
    name: &str,
    lim: Limits,
    f: &mut dyn FnMut(&[f32]),
) -> Result<()> {
    let mut p = Pcm48k::new(r, file_len, name, lim)?;
    let mut chunk: Vec<f32> = Vec::new();
    loop {
        chunk.clear();
        if !p.next_chunk(&mut chunk)? {
            return Ok(());
        }
        f(&chunk);
    }
}

/// [`stream_48k_limited`] on a file.
pub fn stream_48k(path: &Path, lim: Limits, f: &mut dyn FnMut(&[f32])) -> Result<()> {
    let file = std::fs::File::open(path).map_err(|e| RenderError::io(path, e))?;
    let len = file.metadata().map_err(|e| RenderError::io(path, e))?.len();
    stream_48k_limited(file, len, &path.display().to_string(), lim, f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn wav(tag: u16, bits: u16, ch: u16, rate: u32, data: &[u8]) -> Vec<u8> {
        let mut v = b"RIFF\0\0\0\0WAVEJUNK".to_vec();
        v.extend(28u32.to_le_bytes());
        v.extend([0u8; 28]);
        v.extend(b"fmt ");
        v.extend(16u32.to_le_bytes());
        v.extend(tag.to_le_bytes());
        v.extend(ch.to_le_bytes());
        v.extend(rate.to_le_bytes());
        v.extend((rate * u32::from(ch) * u32::from(bits) / 8).to_le_bytes());
        v.extend((ch * bits / 8).to_le_bytes());
        v.extend(bits.to_le_bytes());
        v.extend(b"data");
        v.extend((data.len() as u32).to_le_bytes());
        v.extend(data);
        v
    }

    fn collect(bytes: &[u8]) -> (Vec<f32>, usize) {
        let mut out = Vec::new();
        let mut biggest = 0;
        stream_48k_reader(
            Cursor::new(bytes.to_vec()),
            bytes.len() as u64,
            "t",
            &mut |c| {
                biggest = biggest.max(c.len() / 2);
                out.extend_from_slice(c);
            },
        )
        .unwrap();
        (out, biggest)
    }

    fn pcm16(frames: usize, ch: usize) -> Vec<u8> {
        (0..frames * ch)
            .flat_map(|i| (((i * 7919) % 60_000) as i32 - 30_000).to_le_bytes()[..2].to_vec())
            .collect()
    }

    #[test]
    fn chunked_output_equals_the_whole_file_path_for_every_rate_and_format() {
        for (rate, ch) in [
            (44_100u32, 2u16),
            (48_000, 2),
            (16_000, 1),
            (96_000, 2),
            (22_050, 1),
        ] {
            let data = pcm16(50_000, ch as usize);
            let bytes = wav(1, 16, ch, rate, &data);
            let (r, whole) = crate::audio::parse_wav(&bytes).unwrap();
            assert_eq!(r, rate);
            let want = crate::audio::resample(rate, whole);
            let (got, biggest) = collect(&bytes);
            assert_eq!(got.len(), want.data.len(), "{rate} Hz x{ch}");
            assert_eq!(got, want.data, "{rate} Hz x{ch}: identical samples");
            assert!(biggest <= CHUNK_FRAMES, "chunk of {biggest} frames");
        }
        // 24-bit and float too.
        let f: Vec<u8> = (0..4000)
            .flat_map(|i| (((i % 100) as f32 - 50.0) / 60.0).to_le_bytes())
            .collect();
        let bytes = wav(3, 32, 2, 32_000, &f);
        let (_, whole) = crate::audio::parse_wav(&bytes).unwrap();
        assert_eq!(
            collect(&bytes).0,
            crate::audio::resample(32_000, whole).data
        );
    }

    #[test]
    fn memory_is_bounded_by_the_chunk_size_not_the_file_length() {
        // 40 s of 44.1 kHz audio: many chunks, none bigger than the bound.
        let bytes = wav(1, 16, 2, 44_100, &pcm16(44_100 * 40, 2));
        let mut chunks = 0;
        let mut total = 0usize;
        stream_48k_reader(
            Cursor::new(bytes.clone()),
            bytes.len() as u64,
            "t",
            &mut |c| {
                assert!(c.len() / 2 <= CHUNK_FRAMES);
                chunks += 1;
                total += c.len() / 2;
            },
        )
        .unwrap();
        assert!(chunks > 25, "{chunks} chunks");
        assert_eq!(total, 44_100 * 40 * 48_000 / 44_100);
    }

    /// An RF64 file whose header declares far more than 4 GiB is read lazily: only
    /// the requested chunks are touched, and a short real file just ends early.
    #[test]
    fn rf64_with_a_huge_declared_size_streams_without_allocating_it() {
        struct Zeros {
            left: u64,
            head: Cursor<Vec<u8>>,
        }
        impl std::io::Read for Zeros {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                let n = self.head.read(b)?;
                if n > 0 {
                    return Ok(n);
                }
                let n = (b.len() as u64).min(self.left) as usize;
                b[..n].fill(0);
                self.left -= n as u64;
                Ok(n)
            }
        }
        impl std::io::Seek for Zeros {
            fn seek(&mut self, p: std::io::SeekFrom) -> std::io::Result<u64> {
                self.head.seek(p)
            }
        }
        let declared: u64 = 6 << 30; // 6 GiB of 16-bit stereo = 1.6 billion frames
        let mut h = b"RF64\xff\xff\xff\xffWAVEds64".to_vec();
        h.extend(28u32.to_le_bytes());
        h.extend((declared + 100).to_le_bytes());
        h.extend(declared.to_le_bytes());
        h.extend((declared / 4).to_le_bytes());
        h.extend(0u32.to_le_bytes());
        h.extend(b"fmt ");
        h.extend(16u32.to_le_bytes());
        h.extend([1, 0, 2, 0]);
        h.extend(48_000u32.to_le_bytes());
        h.extend(192_000u32.to_le_bytes());
        h.extend([4, 0, 16, 0]);
        h.extend(b"data");
        h.extend(0xFFFF_FFFFu32.to_le_bytes());
        let head_len = h.len() as u64;
        let mut s = WavStream::from_reader(
            Zeros {
                left: declared,
                head: Cursor::new(h),
            },
            head_len + declared,
            "rf64",
        )
        .unwrap();
        assert_eq!(s.frames(), declared / 4);
        assert!(s.frames() > u64::from(u32::MAX) / 4);
        let mut out = Vec::new();
        assert_eq!(s.read_frames(1000, &mut out).unwrap(), 1000);
        assert_eq!(out.len(), 2000);
        // A real file shorter than its header claims ends at the bytes that exist.
        let mut cut = wav(1, 16, 2, 48_000, &pcm16(100, 2));
        let n = cut.len();
        let len_field = n - 400 - 4;
        cut[len_field..len_field + 4].copy_from_slice(&1_000_000u32.to_le_bytes());
        assert_eq!(collect(&cut).0.len(), 200);
    }

    #[test]
    fn hostile_headers_are_errors() {
        let ok = wav(1, 16, 2, 48_000, &pcm16(10, 2));
        for cut in [0usize, 4, 11, 20, 40] {
            assert!(
                stream_48k_reader(
                    Cursor::new(ok[..cut].to_vec()),
                    cut as u64,
                    "t",
                    &mut |_| {}
                )
                .is_err(),
                "cut {cut}"
            );
        }
        for (tag, bits, ch, rate) in [
            (1u16, 0u16, 2u16, 48_000u32),
            (1, 16, 0, 48_000),
            (1, 16, 33, 48_000),
            (1, 16, 2, 0),
            (3, 16, 2, 48_000),
        ] {
            let b = wav(tag, bits, ch, rate, &[0; 64]);
            assert!(
                stream_48k_reader(Cursor::new(b.clone()), b.len() as u64, "t", &mut |_| {})
                    .is_err()
            );
        }
        let mut no_data = wav(1, 16, 2, 48_000, &[]);
        no_data.truncate(no_data.len() - 8);
        assert!(
            stream_48k_reader(
                Cursor::new(no_data.clone()),
                no_data.len() as u64,
                "t",
                &mut |_| {}
            )
            .is_err()
        );
    }

    /// The hostile case: 8 kHz 8-bit mono claiming gigabytes of data would resample
    /// to ~16 GB of 48 kHz stereo f32. Decoding stops at the project duration.
    #[test]
    fn hostile_low_rate_header_is_cut_at_the_project_duration_and_checked_against_it() {
        struct Zeros(u64, Cursor<Vec<u8>>);
        impl std::io::Read for Zeros {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                let n = self.1.read(b)?;
                if n > 0 {
                    return Ok(n);
                }
                let n = (b.len() as u64).min(self.0) as usize;
                b[..n].fill(128);
                self.0 -= n as u64;
                Ok(n)
            }
        }
        impl std::io::Seek for Zeros {
            fn seek(&mut self, p: std::io::SeekFrom) -> std::io::Result<u64> {
                self.1.seek(p)
            }
        }
        let declared: u64 = 3 << 30;
        let mut h = wav(1, 8, 1, 8_000, &[]);
        let n = h.len();
        h[n - 4..].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // data size
        // Plain RIFF cannot say > 4 GiB; use the largest 32-bit value instead.
        let head_len = h.len() as u64;
        let mk = || Zeros(declared, Cursor::new(h.clone()));
        let lim = Limits {
            max_out_frames: 48_000 * 2, // a 2 s project
            expect_rate: Some(8_000),
            expect_channels: Some(1),
        };
        let mut total = 0usize;
        let mut biggest = 0usize;
        stream_48k_limited(mk(), head_len + declared, "hostile", lim, &mut |c| {
            total += c.len() / 2;
            biggest = biggest.max(c.len() / 2);
        })
        .unwrap();
        assert!(
            total <= 96_000,
            "decoded {total} frames, project allows 96000"
        );
        assert!(total > 90_000 && biggest <= CHUNK_FRAMES);
        // The header must agree with media.*Audio of the project.
        for bad in [
            Limits {
                expect_rate: Some(48_000),
                ..lim
            },
            Limits {
                expect_channels: Some(2),
                ..lim
            },
        ] {
            assert!(stream_48k_limited(mk(), head_len + declared, "x", bad, &mut |_| {}).is_err());
        }
        // 48 kHz passthrough is cut too.
        let big = wav(1, 16, 2, 48_000, &pcm16(200_000, 2));
        let mut got = 0;
        stream_48k_limited(
            Cursor::new(big.clone()),
            big.len() as u64,
            "t",
            Limits {
                max_out_frames: 100_000,
                ..Limits::NONE
            },
            &mut |c| got += c.len() / 2,
        )
        .unwrap();
        assert_eq!(got, 100_000);
    }
}
