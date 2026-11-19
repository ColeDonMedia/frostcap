//! Streaming audio mix for the export: every stage of [`crate::audio::render_whole`]
//! (high-pass, noise gate, gain, placement on the video clock, cuts + speed
//! remap with its anti-alias average, loudness gain, look-ahead limiter, i16)
//! re-expressed as a pull-based chain with explicit state, so memory stays
//! bounded (a few chunks plus a short source window) whatever the duration, and
//! the samples are identical to the whole-buffer path.

use std::collections::VecDeque;
use std::fs::File;
use std::sync::Arc;

use crate::audio::{Biquad, RATE};
use crate::error::{RenderError, Result};
use crate::model::{AudioRef, AudioStyle, Piece, RenderProject};
use crate::wavstream::{CHUNK_FRAMES, Limits, Pcm48k};

/// Second-order high-pass on both channels, state carried between chunks.
struct HighPass([Biquad; 2]);

impl HighPass {
    fn new(hz: f64) -> Self {
        Self([Biquad::high_pass(hz, f64::from(RATE)); 2])
    }
    fn process(&mut self, d: &mut [f32]) {
        for fr in d.as_chunks_mut::<2>().0 {
            for c in 0..2 {
                fr[c] = self.0[c].run(f64::from(fr[c])) as f32;
            }
        }
    }
}

/// Noise gate (same constants as `audio::noise_gate`), state carried.
struct Gate {
    thr: f64,
    env_rel: f64,
    att: f64,
    rel: f64,
    hold: usize,
    env: f64,
    gain: f64,
    held: usize,
}

impl Gate {
    fn new(threshold_db: f64) -> Self {
        let rate = f64::from(RATE);
        Self {
            thr: 10f64.powf(threshold_db / 20.0),
            env_rel: (-1.0 / (0.010 * rate)).exp(),
            att: 1.0 - (-1.0 / (0.005 * rate)).exp(),
            rel: 1.0 - (-1.0 / (0.120 * rate)).exp(),
            hold: (0.150 * rate) as usize,
            env: 0.0,
            gain: 0.001,
            held: 0,
        }
    }
    fn process(&mut self, d: &mut [f32]) {
        for fr in d.as_chunks_mut::<2>().0 {
            let peak = f64::from(fr[0].abs().max(fr[1].abs()));
            self.env = if peak > self.env {
                peak
            } else {
                self.env * self.env_rel
            };
            let target = if self.env >= self.thr {
                self.held = self.hold;
                1.0
            } else if self.held > 0 {
                self.held -= 1;
                1.0
            } else {
                0.001
            };
            self.gain += (target - self.gain)
                * if target > self.gain {
                    self.att
                } else {
                    self.rel
                };
            fr[0] = (f64::from(fr[0]) * self.gain) as f32;
            fr[1] = (f64::from(fr[1]) * self.gain) as f32;
        }
    }
}

/// BS.1770-4 integrated loudness, fed in chunks.
pub struct LufsAcc {
    f: [[Biquad; 2]; 2],
    step: usize,
    i: usize,
    acc: f64,
    sums: Vec<f64>,
}

impl Default for LufsAcc {
    fn default() -> Self {
        Self::new()
    }
}

impl LufsAcc {
    /// Fresh accumulator.
    pub fn new() -> Self {
        let shelf = Biquad::new(
            [
                1.535_124_859_586_97,
                -2.691_696_189_406_38,
                1.198_392_810_852_85,
            ],
            [-1.690_659_293_182_41, 0.732_480_774_215_85],
        );
        let hp = Biquad::new(
            [1.0, -2.0, 1.0],
            [-1.990_047_454_833_98, 0.990_072_250_366_21],
        );
        Self {
            f: [[shelf, hp]; 2],
            step: (RATE / 10) as usize,
            i: 0,
            acc: 0.0,
            sums: Vec::new(),
        }
    }

    /// Add interleaved stereo frames.
    pub fn feed(&mut self, d: &[f32]) {
        for fr in d.as_chunks::<2>().0 {
            for c in 0..2 {
                let s1 = self.f[c][0].run(f64::from(fr[c]));
                let y = self.f[c][1].run(s1);
                self.acc += y * y;
            }
            if (self.i + 1).is_multiple_of(self.step) {
                self.sums.push(self.acc);
                self.acc = 0.0;
            }
            self.i += 1;
        }
    }

    /// Integrated loudness in LUFS, `None` if silent.
    pub fn finish(self) -> Option<f64> {
        let step = self.step;
        let blocks: Vec<f64> = self
            .sums
            .windows(4)
            .map(|w| w.iter().sum::<f64>() / (4 * step) as f64)
            .collect();
        let lufs = |ms: f64| -0.691 + 10.0 * ms.log10();
        let abs: Vec<f64> = blocks
            .iter()
            .copied()
            .filter(|&m| m > 0.0 && lufs(m) > -70.0)
            .collect();
        if abs.is_empty() {
            return None;
        }
        let rel_gate = lufs(abs.iter().sum::<f64>() / abs.len() as f64) - 10.0;
        let gated: Vec<f64> = abs.into_iter().filter(|&m| lufs(m) > rel_gate).collect();
        if gated.is_empty() {
            return None;
        }
        Some(lufs(gated.iter().sum::<f64>() / gated.len() as f64))
    }
}

/// Look-ahead peak limiter (same maths as `audio::limit`), streaming: output
/// lags the input by the 5 ms look-ahead until [`Limiter::finish`].
struct Limiter {
    ceiling: f32,
    look: u64,
    rel: f32,
    g: f32,
    /// Frames not yet emitted, with their requested gain; first is index `base`.
    pending: VecDeque<([f32; 2], f32)>,
    base: u64,
    pushed: u64,
    next_dq: u64,
    dq: VecDeque<(u64, f32)>,
}

impl Limiter {
    fn new(ceiling: f32) -> Self {
        Self {
            ceiling,
            look: u64::from(RATE / 200),
            rel: 1.0 - (-1.0 / (0.080 * RATE as f32)).exp(),
            g: 1.0,
            pending: VecDeque::new(),
            base: 0,
            pushed: 0,
            next_dq: 0,
            dq: VecDeque::new(),
        }
    }

    fn push(&mut self, d: &[f32], out: &mut Vec<f32>) {
        for fr in d.as_chunks::<2>().0 {
            let p = fr[0].abs().max(fr[1].abs());
            let req = if p > self.ceiling {
                self.ceiling / p
            } else {
                1.0
            };
            self.pending.push_back((*fr, req));
            self.pushed += 1;
        }
        self.drain(false, out);
    }

    fn finish(&mut self, out: &mut Vec<f32>) {
        self.drain(true, out);
    }

    fn drain(&mut self, done: bool, out: &mut Vec<f32>) {
        while !self.pending.is_empty() {
            let i = self.base;
            if !done && self.pushed <= i + self.look {
                break;
            }
            while self.next_dq <= i + self.look && self.next_dq < self.pushed {
                let req = self.pending[(self.next_dq - self.base) as usize].1;
                while self.dq.back().is_some_and(|&(_, r)| r >= req) {
                    self.dq.pop_back();
                }
                self.dq.push_back((self.next_dq, req));
                self.next_dq += 1;
            }
            while self.dq.front().is_some_and(|&(j, _)| j < i) {
                self.dq.pop_front();
            }
            let target = self.dq.front().map_or(1.0, |&(_, r)| r);
            self.g = if target < self.g {
                target
            } else {
                self.g + (target - self.g) * self.rel
            };
            let (fr, req) = self.pending.pop_front().expect("pending frame");
            let g_eff = self.g.min(req);
            out.push(fr[0] * g_eff);
            out.push(fr[1] * g_eff);
            self.base += 1;
        }
    }
}

/// One decoded, processed file placed on the video clock.
struct TrackFeed {
    pcm: Pcm48k<File>,
    hp: Option<HighPass>,
    gate: Option<Gate>,
    gain: f32,
    /// Video-clock frame of the next unread file frame (file frame + shift).
    next_v: i64,
    eof: bool,
    buf: Vec<f32>,
}

/// The mix on the video clock, produced on demand into a sliding window.
struct SourceMix {
    n_src: u64,
    base: u64,
    win: Vec<f32>,
    tracks: Vec<TrackFeed>,
}

impl SourceMix {
    fn ensure(&mut self, upto: u64) -> Result<()> {
        let upto = upto.min(self.n_src);
        for t in &mut self.tracks {
            while !t.eof && t.next_v < upto as i64 {
                t.buf.clear();
                if !t.pcm.next_chunk(&mut t.buf)? {
                    t.eof = true;
                    break;
                }
                if let Some(h) = &mut t.hp {
                    h.process(&mut t.buf);
                }
                if let Some(g) = &mut t.gate {
                    g.process(&mut t.buf);
                }
                let gain = t.gain;
                t.buf.iter_mut().for_each(|x| *x *= gain);
                let frames = (t.buf.len() / 2) as i64;
                for j in 0..frames {
                    let v = t.next_v + j;
                    if v < 0 {
                        continue;
                    }
                    let v = v as u64;
                    if v < self.base {
                        continue;
                    }
                    if v >= self.n_src {
                        t.eof = true;
                        break;
                    }
                    let o = ((v - self.base) * 2) as usize;
                    if self.win.len() < o + 2 {
                        self.win
                            .try_reserve(o + 2 - self.win.len())
                            .map_err(|_| RenderError::Audio("out of memory mixing".into()))?;
                        self.win.resize(o + 2, 0.0);
                    }
                    self.win[o] += t.buf[(j * 2) as usize];
                    self.win[o + 1] += t.buf[(j * 2) as usize + 1];
                }
                t.next_v += frames;
            }
        }
        Ok(())
    }

    fn discard_before(&mut self, frame: u64) {
        let frame = frame.min(self.base + (self.win.len() / 2) as u64);
        if frame > self.base {
            self.win.drain(..((frame - self.base) * 2) as usize);
            self.base = frame;
        }
    }

    /// Mix sample at a (possibly fractional) frame position, as `audio::remap`'s `at`.
    fn at(&self, pos: f64, c: usize) -> f32 {
        if self.n_src == 0 || pos < 0.0 {
            return 0.0;
        }
        let i = pos as u64;
        let get = |f: u64| -> f32 {
            if f < self.base {
                return 0.0;
            }
            self.win
                .get(((f - self.base) * 2) as usize + c)
                .copied()
                .unwrap_or(0.0)
        };
        if i + 1 >= self.n_src {
            return if i < self.n_src { get(i) } else { 0.0 };
        }
        let f = (pos - i as f64) as f32;
        let (a, b) = (get(i), get(i + 1));
        a + (b - a) * f
    }
}

/// Cuts + speed remap, pulling the source window as it goes.
struct Remap {
    pieces: Vec<Piece>,
    r: f64,
    n_out: u64,
    pi: usize,
    acc_ms: f64,
    done: u64,
    first: u64,
    end: u64,
    started: bool,
}

impl Remap {
    fn next(
        &mut self,
        mix: &mut SourceMix,
        max_frames: usize,
        out: &mut Vec<f32>,
    ) -> Result<usize> {
        let mut produced = 0;
        while produced < max_frames && self.done < self.n_out {
            if self.started && self.pi >= self.pieces.len() {
                // Past the last piece: the rest of the output is silence.
                let rest = (self.n_out - self.done).min((max_frames - produced) as u64);
                out.resize(out.len() + rest as usize * 2, 0.0);
                self.done += rest;
                produced += rest as usize;
                continue;
            }
            if !self.started || self.done >= self.end {
                // Next piece (a piece may be empty after rounding).
                if self.started {
                    self.pi += 1;
                }
                self.started = true;
                if let Some(piece) = self.pieces.get(self.pi) {
                    self.acc_ms += piece.out_len();
                    self.end = ((self.acc_ms * self.r).round() as u64).min(self.n_out);
                    self.first = self.done;
                }
                continue;
            }
            let piece = self.pieces[self.pi];
            let upto = self.end.min(self.done + (max_frames - produced) as u64);
            let taps = if piece.speed > 2.0 {
                ((piece.speed * 2.0).ceil() as usize).clamp(2, 16)
            } else {
                1
            };
            // Source frames needed for the whole run of output frames below.
            let last_pos = piece.src_start * self.r + (upto - 1 - self.first) as f64 * piece.speed;
            mix.ensure((last_pos + piece.speed + 2.0).max(0.0) as u64 + 1)?;
            while self.done < upto {
                let pos = piece.src_start * self.r + (self.done - self.first) as f64 * piece.speed;
                if taps == 1 {
                    out.push(mix.at(pos, 0));
                    out.push(mix.at(pos, 1));
                } else {
                    for c in 0..2 {
                        let sum: f32 = (0..taps)
                            .map(|t| {
                                let off = ((t as f64 + 0.5) / taps as f64 - 0.5) * piece.speed;
                                mix.at(pos + off, c)
                            })
                            .sum();
                        out.push(sum / taps as f32);
                    }
                }
                self.done += 1;
                produced += 1;
            }
            let next_pos = piece.src_start * self.r
                + self.done.saturating_sub(self.first) as f64 * piece.speed
                - piece.speed
                - 2.0;
            mix.discard_before(next_pos.max(0.0) as u64);
        }
        Ok(produced)
    }
}

/// Everything needed to start (several) identical streaming mixes of a project:
/// the loudness gain comes from one measuring pass.
pub struct MixPlan {
    p: RenderProject,
    style: AudioStyle,
    norm_gain: f32,
    n_out: u64,
    chunk: usize,
}

impl MixPlan {
    /// `None` when the project has no readable audio. Runs the loudness
    /// measuring pass (streaming) when normalization is on.
    pub fn new(p: &RenderProject) -> Result<Option<Arc<Self>>> {
        Self::with_chunk(p, CHUNK_FRAMES)
    }

    /// [`Self::new`] with an explicit chunk size (frames), for tests.
    pub fn with_chunk(p: &RenderProject, chunk: usize) -> Result<Option<Arc<Self>>> {
        let has = |t: &Option<AudioRef>| t.as_ref().is_some_and(|t| t.path.is_file());
        if !has(&p.mic) && !has(&p.system) {
            return Ok(None);
        }
        let style = p.style.audio;
        let n_out = (p.output_ms() * f64::from(RATE) / 1000.0).round() as u64;
        let mut plan = Self {
            p: p.clone(),
            style,
            norm_gain: 1.0,
            n_out,
            chunk: chunk.max(2),
        };
        if style.normalize {
            let mut s = plan.open(false)?;
            let mut acc = LufsAcc::new();
            let mut buf = Vec::new();
            loop {
                buf.clear();
                if s.raw_chunk(&mut buf)? == 0 {
                    break;
                }
                acc.feed(&buf);
            }
            if let Some(l) = acc.finish() {
                plan.norm_gain =
                    10f64.powf((style.target_lufs - l).clamp(-24.0, 24.0) / 20.0) as f32;
            }
        }
        Ok(Some(Arc::new(plan)))
    }

    /// Output length in 48 kHz frames.
    pub fn frames(&self) -> u64 {
        self.n_out
    }

    /// A fresh stream from the start (own file handles and DSP state).
    pub fn stream(&self) -> Result<AudioStream> {
        self.open(true)
    }

    fn open(&self, finalize: bool) -> Result<AudioStream> {
        let p = &self.p;
        let st = &self.style;
        let n_src =
            (p.duration_ms.max(0.0) * f64::from(RATE) / 1000.0).ceil() as u64 + u64::from(RATE);
        let mut tracks = Vec::new();
        for (t, mic) in [(&p.mic, true), (&p.system, false)] {
            let Some(t) = t else { continue };
            let gain = if mic { st.mic_gain } else { st.system_gain };
            if !t.path.is_file() || gain <= 0.0 {
                continue;
            }
            let need_ms = (p.duration_ms - t.offset_ms.min(0.0)).max(0.0) + 1000.0;
            let lim = Limits {
                max_out_frames: (need_ms * f64::from(RATE) / 1000.0).ceil() as u64,
                expect_rate: t.expect.map(|e| e.0),
                expect_channels: t.expect.map(|e| e.1),
            };
            let file = File::open(&t.path).map_err(|e| RenderError::io(&t.path, e))?;
            let len = file
                .metadata()
                .map_err(|e| RenderError::io(&t.path, e))?
                .len();
            let mut pcm = Pcm48k::new(file, len, &t.path.display().to_string(), lim)?;
            pcm.set_chunk(self.chunk);
            tracks.push(TrackFeed {
                pcm,
                hp: (mic && st.high_pass).then(|| HighPass::new(st.high_pass_hz)),
                gate: (mic && st.noise_gate).then(|| Gate::new(st.gate_threshold_db)),
                gain: gain as f32,
                next_v: (t.offset_ms * f64::from(RATE) / 1000.0).round() as i64,
                eof: false,
                buf: Vec::new(),
            });
        }
        let r = f64::from(RATE) / 1000.0;
        Ok(AudioStream {
            mix: SourceMix {
                n_src,
                base: 0,
                win: Vec::new(),
                tracks,
            },
            remap: Remap {
                pieces: p.pieces.clone(),
                r,
                n_out: self.n_out,
                pi: 0,
                acc_ms: 0.0,
                done: 0,
                first: 0,
                end: 0,
                started: false,
            },
            gain: if finalize { self.norm_gain } else { 1.0 },
            limiter: finalize.then(|| Limiter::new(0.891)),
            chunk: self.chunk,
            raw: Vec::new(),
            ready: VecDeque::new(),
            flushed: false,
            taken: 0,
            total: self.n_out,
        })
    }
}

/// Pull-based finished audio (interleaved stereo i16 at 48 kHz).
pub struct AudioStream {
    mix: SourceMix,
    remap: Remap,
    gain: f32,
    limiter: Option<Limiter>,
    chunk: usize,
    raw: Vec<f32>,
    ready: VecDeque<i16>,
    flushed: bool,
    taken: u64,
    total: u64,
}

impl AudioStream {
    /// Total frames this stream will deliver.
    pub fn total_frames(&self) -> u64 {
        self.total
    }

    /// Remapped, un-normalized chunk (measuring pass).
    fn raw_chunk(&mut self, out: &mut Vec<f32>) -> Result<usize> {
        self.remap.next(&mut self.mix, self.chunk, out)
    }

    /// Up to `max_frames` finished frames (fewer only at the end of the stream).
    pub fn take(&mut self, max_frames: usize) -> Result<Vec<i16>> {
        let want = max_frames.min((self.total - self.taken) as usize) * 2;
        let mut out: Vec<i16> = Vec::new();
        out.try_reserve(want)
            .map_err(|_| RenderError::Audio("out of memory".into()))?;
        while out.len() < want {
            if let Some(v) = self.ready.pop_front() {
                out.push(v);
                continue;
            }
            if self.flushed {
                break;
            }
            self.raw.clear();
            let n = self.remap.next(&mut self.mix, self.chunk, &mut self.raw)?;
            let mut limited = Vec::new();
            if n == 0 {
                if let Some(l) = &mut self.limiter {
                    l.finish(&mut limited);
                }
                self.flushed = true;
            } else {
                let gain = self.gain;
                self.raw.iter_mut().for_each(|x| *x *= gain);
                match &mut self.limiter {
                    Some(l) => l.push(&self.raw, &mut limited),
                    None => limited.extend_from_slice(&self.raw),
                }
            }
            self.ready.extend(
                limited
                    .iter()
                    .map(|x| (x.clamp(-1.0, 1.0) * 32767.0).round() as i16),
            );
        }
        self.taken += (out.len() / 2) as u64;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{self, Stereo};
    use std::path::{Path, PathBuf};

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))
            .join("test-tmp")
            .join(format!("mixer-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Stereo 16-bit WAV of `ms` with a few tones, bursts and a noise floor (so the
    /// gate, high-pass and limiter all have something to do), deterministic.
    fn write_track(path: &Path, rate: u32, ms: u64, seed: u32, hz: f64) {
        let n = (u64::from(rate) * ms / 1000) as usize;
        let mut x = seed;
        let mut pcm = Vec::with_capacity(n * 4);
        for i in 0..n {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = ((x >> 16) as f32 / 65_536.0 - 0.5) * 0.004;
            let t = i as f64 / f64::from(rate);
            let burst = if (t * 3.0).fract() < 0.4 { 1.4 } else { 0.0 };
            let v = noise + burst * (2.0 * std::f64::consts::PI * hz * t).sin() as f32 * 0.7;
            let s = (v.clamp(-1.0, 1.0) * 32767.0) as i16;
            pcm.extend(s.to_le_bytes());
            pcm.extend((s / 2).to_le_bytes());
        }
        let mut w = b"RIFF".to_vec();
        w.extend((36 + pcm.len() as u32).to_le_bytes());
        w.extend(b"WAVEfmt ");
        w.extend(16u32.to_le_bytes());
        w.extend([1, 0, 2, 0]);
        w.extend(rate.to_le_bytes());
        w.extend((rate * 4).to_le_bytes());
        w.extend([4, 0, 16, 0]);
        w.extend(b"data");
        w.extend((pcm.len() as u32).to_le_bytes());
        w.extend(pcm);
        std::fs::write(path, w).unwrap();
    }

    fn piece(a: f64, b: f64, speed: f64) -> Piece {
        Piece {
            src_start: a,
            src_end: b,
            speed,
        }
    }

    fn project(tag: &str, pieces: Vec<Piece>, duration_ms: f64) -> RenderProject {
        let d = scratch(tag);
        write_track(&d.join("mic.wav"), 48_000, duration_ms as u64, 1, 440.0);
        write_track(
            &d.join("sys.wav"),
            44_100,
            duration_ms as u64 - 300,
            7,
            1250.0,
        );
        let mut p = RenderProject::empty_for_tests();
        p.duration_ms = duration_ms;
        p.pieces = pieces;
        p.mic = Some(AudioRef {
            path: d.join("mic.wav"),
            offset_ms: 0.0,
            expect: Some((48_000, 2)),
        });
        p.system = Some(AudioRef {
            path: d.join("sys.wav"),
            offset_ms: 120.0,
            expect: Some((44_100, 2)),
        });
        p.style.audio = AudioStyle {
            noise_gate: true,
            gate_threshold_db: -45.0,
            high_pass: true,
            high_pass_hz: 90.0,
            normalize: true,
            target_lufs: -16.0,
            mic_gain: 1.3,
            system_gain: 0.7,
        };
        p
    }

    fn stream_all(plan: &MixPlan, pull: usize) -> Vec<i16> {
        let mut s = plan.stream().unwrap();
        let mut out = Vec::new();
        loop {
            let c = s.take(pull).unwrap();
            if c.is_empty() {
                return out;
            }
            assert!(c.len() <= pull * 2);
            out.extend(c);
        }
    }

    #[test]
    fn chunked_mix_is_sample_identical_to_the_whole_buffer_path() {
        // Cuts, then 0.5x, 2x and 4x pieces, with piece edges that never align with
        // any chunk size below.
        let cases: Vec<(&str, Vec<Piece>)> = vec![
            ("plain", vec![piece(0.0, 2500.0, 1.0)]),
            (
                "cuts",
                vec![
                    piece(0.0, 600.0, 1.0),
                    piece(900.0, 1500.0, 1.0),
                    piece(1777.0, 2500.0, 1.0),
                ],
            ),
            (
                "slow",
                vec![piece(100.0, 1300.0, 0.5), piece(1300.0, 2300.0, 1.0)],
            ),
            (
                "fast2",
                vec![piece(0.0, 1000.0, 1.0), piece(1000.0, 2400.0, 2.0)],
            ),
            (
                "fast4_mixed",
                vec![
                    piece(0.0, 413.0, 1.0),
                    piece(413.0, 1500.0, 4.0),
                    piece(1700.0, 2100.0, 0.5),
                    piece(2100.0, 2500.0, 3.0),
                ],
            ),
        ];
        for (name, pieces) in cases {
            let p = project(name, pieces, 2500.0);
            let want = audio::render_whole(&p).unwrap().unwrap();
            for chunk in [97usize, 1000, 4097, 65_536] {
                let plan = MixPlan::with_chunk(&p, chunk).unwrap().unwrap();
                assert_eq!(plan.frames() as usize * 2, want.len(), "{name}");
                for pull in [333usize, 5000] {
                    let got = stream_all(&plan, pull);
                    assert_eq!(got.len(), want.len(), "{name} chunk {chunk} pull {pull}");
                    assert!(
                        got == want,
                        "{name}: samples differ (chunk {chunk}, pull {pull})"
                    );
                }
            }
        }
    }

    #[test]
    fn identical_without_processing_single_track_and_negative_offset() {
        let mut p = project(
            "simple",
            vec![piece(0.0, 1500.0, 1.0), piece(1500.0, 2500.0, 2.0)],
            2500.0,
        );
        p.style.audio = AudioStyle::OFF;
        p.system.as_mut().unwrap().offset_ms = -200.0;
        p.mic.as_mut().unwrap().offset_ms = 40.0;
        let want = audio::render_whole(&p).unwrap().unwrap();
        let plan = MixPlan::with_chunk(&p, 1234).unwrap().unwrap();
        assert!(stream_all(&plan, 777) == want);
        // Muting a source drops it entirely (gain 0 -> not even decoded).
        p.style.audio.mic_gain = 0.0;
        p.style.audio.normalize = true;
        let want = audio::render_whole(&p).unwrap().unwrap();
        let plan = MixPlan::with_chunk(&p, 999).unwrap().unwrap();
        assert!(stream_all(&plan, 4000) == want);
        // Two streams of one plan are independent and equal.
        let a = stream_all(&plan, 1000);
        let b = stream_all(&plan, 3000);
        assert!(a == b);
    }

    #[test]
    fn stages_match_their_whole_buffer_counterparts() {
        // Limiter, loudness and filters in isolation, fed in odd chunk sizes.
        let n = 48_000 * 2;
        let data: Vec<f32> = (0..n * 2)
            .map(|i| ((i as f32 * 0.013).sin() * 1.7).clamp(-2.0, 2.0))
            .collect();
        let mut whole = Stereo { data: data.clone() };
        audio::limit(&mut whole, 0.891);
        let mut lim = Limiter::new(0.891);
        let mut got = Vec::new();
        for c in data.chunks(2 * 777) {
            lim.push(c, &mut got);
        }
        lim.finish(&mut got);
        assert!(got == whole.data, "limiter");
        let want = audio::integrated_lufs(&Stereo { data: data.clone() });
        let mut acc = LufsAcc::new();
        for c in data.chunks(2 * 4001) {
            acc.feed(c);
        }
        assert_eq!(acc.finish(), want, "loudness");
        let mut w = Stereo { data: data.clone() };
        audio::high_pass(&mut w, 120.0);
        audio::noise_gate(&mut w, -30.0);
        let mut d = data.clone();
        let (mut hp, mut g) = (HighPass::new(120.0), Gate::new(-30.0));
        for c in d.chunks_mut(2 * 555) {
            hp.process(c);
            g.process(c);
        }
        assert!(d == w.data, "high-pass + gate");
    }

    /// Throughput probe (run with `--release -- --ignored --nocapture`).
    #[test]
    #[ignore = "timing probe"]
    fn mixing_runs_much_faster_than_realtime() {
        let p = project("perf", vec![piece(0.0, 60_000.0, 1.0)], 60_000.0);
        let t = std::time::Instant::now();
        let plan = MixPlan::new(&p).unwrap().unwrap();
        let t_plan = t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        let out = stream_all(&plan, 48_000);
        let t_stream = t.elapsed().as_secs_f64();
        eprintln!(
            "60 s of audio: measuring pass {t_plan:.2} s, mixing pass {t_stream:.2} s ({} frames)",
            out.len() / 2
        );
    }

    #[test]
    fn memory_stays_bounded_for_long_outputs() {
        // 30 s of output at 4x speed-ups and cuts: the source window never grows
        // past a few chunks, and chunk delivery is capped.
        let mut p = project("bounded", vec![piece(0.0, 30_000.0, 1.0)], 30_000.0);
        p.style.audio.normalize = false;
        let plan = MixPlan::with_chunk(&p, 4096).unwrap().unwrap();
        let mut s = plan.stream().unwrap();
        let mut peak_window = 0usize;
        let mut total = 0usize;
        loop {
            let c = s.take(2048).unwrap();
            if c.is_empty() {
                break;
            }
            total += c.len() / 2;
            peak_window = peak_window.max(s.mix.win.len() / 2);
            assert!(
                s.ready.len() < 2 * 4096 * 2 + 1000,
                "ready queue {}",
                s.ready.len()
            );
        }
        assert_eq!(total as u64, plan.frames());
        assert!(
            peak_window < 4096 * 4,
            "source window peaked at {peak_window} frames for a 1.4M-frame output"
        );
    }
}
