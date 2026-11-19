//! Audio for the export: WAV reading, mic DSP (high-pass, noise gate), mixing on
//! the video clock (honouring `offsetMs`, which may be negative), remapping
//! through the output timeline (cuts + speed), BS.1770 loudness normalization
//! and a look-ahead peak limiter. Pure Rust, deterministic, unit tested.

use std::collections::VecDeque;
use std::path::Path;

use crate::error::{RenderError, Result};
use crate::model::{AudioRef, AudioStyle, RenderProject};

/// Output sample rate (AAC via Media Foundation supports 44.1/48 kHz).
pub const RATE: u32 = 48_000;
/// Output channels.
pub const CHANNELS: u16 = 2;

/// Interleaved stereo f32 at [`RATE`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stereo {
    /// L, R, L, R, ...
    pub data: Vec<f32>,
}

impl Stereo {
    /// Number of frames.
    pub fn frames(&self) -> usize {
        self.data.len() / 2
    }
}

/// Decoded WAV (any rate / channel count) converted to interleaved stereo f32.
pub fn read_wav(path: &Path) -> Result<(u32, Stereo)> {
    read_wav_limited(path, MAX_WAV_BYTES)
}

/// Largest WAV read (about 45 min of 48 kHz stereo f32 at 2 GiB).
pub const MAX_WAV_BYTES: u64 = 2 << 30;

/// [`read_wav`] refusing files larger than `max_bytes`.
pub fn read_wav_limited(path: &Path, max_bytes: u64) -> Result<(u32, Stereo)> {
    use std::io::Read as _;
    let f = std::fs::File::open(path).map_err(|e| RenderError::io(path, e))?;
    let mut bytes = Vec::new();
    f.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| RenderError::io(path, e))?;
    if bytes.len() as u64 > max_bytes {
        return Err(RenderError::Audio(format!(
            "{} is larger than {max_bytes} bytes",
            path.display()
        )));
    }
    parse_wav(&bytes).map_err(|m| RenderError::Audio(format!("{}: {m}", path.display())))
}

fn u16le(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}
fn u32le(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// Parse RIFF/WAVE: PCM 8/16/24/32-bit, IEEE float 32/64, WAVE_FORMAT_EXTENSIBLE.
/// A truncated `data` chunk (crashed recording) uses whatever bytes exist.
pub fn parse_wav(b: &[u8]) -> std::result::Result<(u32, Stereo), String> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let mut i = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut data: Option<&[u8]> = None;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let size = u32le(b, i + 4) as usize;
        let body_start = i + 8;
        let body_end = body_start.saturating_add(size).min(b.len());
        let body = &b[body_start..body_end];
        if id == b"fmt " && body.len() >= 16 {
            let mut tag = u16le(body, 0);
            if tag == 0xFFFE && body.len() >= 26 {
                tag = u16le(body, 24); // first two bytes of the SubFormat GUID
            }
            fmt = Some((tag, u16le(body, 2), u32le(body, 4), u16le(body, 14)));
        } else if id == b"data" {
            data = Some(body);
            if fmt.is_some() {
                break;
            }
        }
        i = body_start + size + (size & 1);
    }
    let (tag, ch, rate, bits) = fmt.ok_or("missing fmt chunk")?;
    let data = data.ok_or("missing data chunk")?;
    if !(1..=32).contains(&ch)
        || !(8_000..=768_000).contains(&rate)
        || !matches!(bits, 8 | 16 | 24 | 32 | 64)
    {
        return Err("invalid format".into());
    }
    let bps = (bits as usize).div_ceil(8);
    let frame = bps * ch as usize;
    let n = data.len() / frame;
    let sample = |o: usize| -> std::result::Result<f32, String> {
        let s = &data[o..o + bps];
        Ok(match (tag, bits) {
            (1, 8) => (s[0] as f32 - 128.0) / 128.0,
            (1, 16) => i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0,
            (1, 24) => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
            (1, 32) => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
            (3, 32) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            (3, 64) => f64::from_le_bytes(s.try_into().map_err(|_| "bad sample")?) as f32,
            _ => return Err(format!("unsupported WAV format tag {tag} / {bits} bit")),
        })
    };
    let mut out = Vec::with_capacity(n * 2);
    for f in 0..n {
        let o = f * frame;
        let l = sample(o)?;
        let r = if ch > 1 { sample(o + bps)? } else { l };
        let fix = |x: f32| if x.is_finite() { x } else { 0.0 };
        out.push(fix(l));
        out.push(fix(r));
    }
    Ok((rate, Stereo { data: out }))
}

/// Linear resampler to [`RATE`] (identity when already at 48 kHz).
pub fn resample(rate: u32, s: Stereo) -> Stereo {
    if rate == RATE || s.frames() < 2 {
        return s;
    }
    let n_in = s.frames();
    let n_out = (n_in as u64 * RATE as u64 / rate as u64) as usize;
    let step = rate as f64 / RATE as f64;
    let mut out = Vec::with_capacity(n_out * 2);
    for k in 0..n_out {
        let pos = k as f64 * step;
        let i = (pos as usize).min(n_in - 2);
        let f = (pos - i as f64) as f32;
        for c in 0..2 {
            let a = s.data[i * 2 + c];
            let b = s.data[(i + 1) * 2 + c];
            out.push(a + (b - a) * f);
        }
    }
    Stereo { data: out }
}

/// Direct-form-I biquad.
#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    fn new(b: [f64; 3], a: [f64; 2]) -> Self {
        Self {
            b,
            a,
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }
    /// RBJ cookbook 2nd-order Butterworth high-pass.
    fn high_pass(hz: f64, rate: f64) -> Self {
        let w0 = 2.0 * std::f64::consts::PI * hz / rate;
        let alpha = w0.sin() / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let cw = w0.cos();
        let a0 = 1.0 + alpha;
        Self::new(
            [
                (1.0 + cw) / 2.0 / a0,
                -(1.0 + cw) / a0,
                (1.0 + cw) / 2.0 / a0,
            ],
            [-2.0 * cw / a0, (1.0 - alpha) / a0],
        )
    }
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// High-pass both channels in place.
pub fn high_pass(s: &mut Stereo, hz: f64) {
    let mut f = [Biquad::high_pass(hz, RATE as f64); 2];
    for fr in s.data.as_chunks_mut::<2>().0 {
        for c in 0..2 {
            fr[c] = f[c].run(fr[c] as f64) as f32;
        }
    }
}

/// Noise gate: opens above `threshold_db` (peak envelope), 5 ms attack,
/// 150 ms hold, 120 ms release, closed gain -60 dB.
pub fn noise_gate(s: &mut Stereo, threshold_db: f64) {
    let thr = 10f64.powf(threshold_db / 20.0);
    let rate = RATE as f64;
    let env_rel = (-1.0 / (0.010 * rate)).exp();
    let att = 1.0 - (-1.0 / (0.005 * rate)).exp();
    let rel = 1.0 - (-1.0 / (0.120 * rate)).exp();
    let hold = (0.150 * rate) as usize;
    let floor = 0.001;
    let (mut env, mut gain, mut held) = (0.0f64, floor, 0usize);
    for fr in s.data.as_chunks_mut::<2>().0 {
        let peak = fr[0].abs().max(fr[1].abs()) as f64;
        env = if peak > env { peak } else { env * env_rel };
        let target = if env >= thr {
            held = hold;
            1.0
        } else if held > 0 {
            held -= 1;
            1.0
        } else {
            floor
        };
        gain += (target - gain) * if target > gain { att } else { rel };
        fr[0] = (fr[0] as f64 * gain) as f32;
        fr[1] = (fr[1] as f64 * gain) as f32;
    }
}

/// ITU-R BS.1770-4 integrated loudness (LUFS) of 48 kHz stereo; `None` if silent.
pub fn integrated_lufs(s: &Stereo) -> Option<f64> {
    // K-weighting coefficients for 48 kHz.
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
    let mut f = [[shelf, hp]; 2];
    let step = (RATE / 10) as usize; // 100 ms
    let n = s.frames();
    let mut sums = Vec::with_capacity(n / step + 1);
    let mut acc = 0.0f64;
    for (i, fr) in s.data.as_chunks::<2>().0.iter().enumerate() {
        for c in 0..2 {
            let s1 = f[c][0].run(fr[c] as f64);
            let y = f[c][1].run(s1);
            acc += y * y;
        }
        if (i + 1) % step == 0 {
            sums.push(acc);
            acc = 0.0;
        }
    }
    // 400 ms blocks with 75 % overlap = 4 consecutive 100 ms sub-blocks.
    let blocks: Vec<f64> = sums
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

/// Look-ahead (5 ms) peak limiter to `ceiling` (linear), 80 ms release.
pub fn limit(s: &mut Stereo, ceiling: f32) {
    let n = s.frames();
    let look = (RATE / 200) as usize;
    let req: Vec<f32> = s
        .data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|f| {
            let p = f[0].abs().max(f[1].abs());
            if p > ceiling { ceiling / p } else { 1.0 }
        })
        .collect();
    // Sliding-window minimum of req over [i, i + look].
    let mut dq: VecDeque<usize> = VecDeque::new();
    let mut g = 1.0f32;
    let rel = 1.0 - (-1.0 / (0.080 * RATE as f32)).exp();
    let mut next = 0usize;
    for i in 0..n {
        while next < n && next <= i + look {
            while dq.back().is_some_and(|&j| req[j] >= req[next]) {
                dq.pop_back();
            }
            dq.push_back(next);
            next += 1;
        }
        while dq.front().is_some_and(|&j| j < i) {
            dq.pop_front();
        }
        let target = dq.front().map_or(1.0, |&j| req[j]);
        g = if target < g {
            target
        } else {
            g + (target - g) * rel
        };
        let g_eff = g.min(req[i]);
        s.data[i * 2] *= g_eff;
        s.data[i * 2 + 1] *= g_eff;
    }
}

fn load_track(t: &AudioRef) -> Result<Option<Stereo>> {
    if !t.path.is_file() {
        return Ok(None);
    }
    let (rate, s) = read_wav(&t.path)?;
    Ok(Some(resample(rate, s)))
}

/// Add `s` (whose sample 0 sits at `offset_ms` on the video clock) into `dst`
/// (sample 0 = video t = 0).
pub fn place(dst: &mut Stereo, s: &Stereo, offset_ms: f64) {
    let shift = (offset_ms * RATE as f64 / 1000.0).round() as i64;
    let n_dst = dst.frames() as i64;
    for j in 0..s.frames() as i64 {
        let i = j + shift;
        if i < 0 {
            continue;
        }
        if i >= n_dst {
            break;
        }
        let (i, j) = (i as usize, j as usize);
        dst.data[i * 2] += s.data[j * 2];
        dst.data[i * 2 + 1] += s.data[j * 2 + 1];
    }
}

/// Remap a source-clock buffer through the project's pieces (cuts + speed).
/// Speed changes resample (pitch follows speed).
pub fn remap(src: &Stereo, p: &RenderProject) -> Stereo {
    let r = RATE as f64 / 1000.0;
    let n_out = (p.output_ms() * r).round() as usize;
    let mut out = Vec::with_capacity(n_out * 2);
    let n_src = src.frames();
    let at = |pos: f64, c: usize| -> f32 {
        if n_src == 0 || pos < 0.0 {
            return 0.0;
        }
        let i = pos as usize;
        if i + 1 >= n_src {
            return if i < n_src { src.data[i * 2 + c] } else { 0.0 };
        }
        let f = (pos - i as f64) as f32;
        let (a, b) = (src.data[i * 2 + c], src.data[(i + 1) * 2 + c]);
        a + (b - a) * f
    };
    let mut done = 0usize;
    let mut acc_ms = 0.0;
    for piece in &p.pieces {
        acc_ms += piece.out_len();
        let end = ((acc_ms * r).round() as usize).min(n_out);
        let first = done;
        while done < end {
            let pos = piece.src_start * r + (done - first) as f64 * piece.speed;
            out.push(at(pos, 0));
            out.push(at(pos, 1));
            done += 1;
        }
    }
    out.resize(n_out * 2, 0.0);
    Stereo { data: out }
}

/// Full audio for the export: `None` when the project has no readable audio.
/// Returns interleaved stereo i16 at 48 kHz, exactly `output_ms` long.
pub fn render(p: &RenderProject) -> Result<Option<Vec<i16>>> {
    let style: AudioStyle = p.style.audio;
    let mic = match &p.mic {
        Some(t) => load_track(t)?,
        None => None,
    };
    let sys = match &p.system {
        Some(t) => load_track(t)?,
        None => None,
    };
    if mic.is_none() && sys.is_none() {
        return Ok(None);
    }
    let src_len = (p.duration_ms.max(0.0) * RATE as f64 / 1000.0).ceil() as usize + RATE as usize;
    let mut mix = Stereo {
        data: vec![0.0; src_len * 2],
    };
    if let (Some(mut m), Some(t)) = (mic, &p.mic)
        && style.mic_gain > 0.0
    {
        if style.high_pass {
            high_pass(&mut m, style.high_pass_hz);
        }
        if style.noise_gate {
            noise_gate(&mut m, style.gate_threshold_db);
        }
        m.data.iter_mut().for_each(|x| *x *= style.mic_gain as f32);
        place(&mut mix, &m, t.offset_ms);
    }
    if let (Some(mut s), Some(t)) = (sys, &p.system)
        && style.system_gain > 0.0
    {
        s.data
            .iter_mut()
            .for_each(|x| *x *= style.system_gain as f32);
        place(&mut mix, &s, t.offset_ms);
    }
    let mut out = remap(&mix, p);
    if style.normalize
        && let Some(l) = integrated_lufs(&out)
    {
        let gain = 10f64.powf(((style.target_lufs - l).clamp(-24.0, 24.0)) / 20.0) as f32;
        out.data.iter_mut().for_each(|x| *x *= gain);
    }
    limit(&mut out, 0.891); // -1 dBFS
    Ok(Some(
        out.data
            .iter()
            .map(|x| (x.clamp(-1.0, 1.0) * 32767.0).round() as i16)
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Piece;

    fn sine(hz: f64, amp: f32, secs: f64) -> Stereo {
        let n = (secs * RATE as f64) as usize;
        let mut d = Vec::with_capacity(n * 2);
        for i in 0..n {
            let v = amp * (2.0 * std::f64::consts::PI * hz * i as f64 / RATE as f64).sin() as f32;
            d.push(v);
            d.push(v);
        }
        Stereo { data: d }
    }

    fn wav_bytes(tag: u16, bits: u16, ch: u16, rate: u32, samples: &[u8]) -> Vec<u8> {
        let mut v = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        v.extend(16u32.to_le_bytes());
        v.extend(tag.to_le_bytes());
        v.extend(ch.to_le_bytes());
        v.extend(rate.to_le_bytes());
        v.extend((rate.wrapping_mul(ch as u32 * bits as u32) / 8).to_le_bytes());
        v.extend((ch * bits / 8).to_le_bytes());
        v.extend(bits.to_le_bytes());
        v.extend(b"data");
        v.extend((samples.len() as u32).to_le_bytes());
        v.extend(samples);
        v
    }

    #[test]
    fn parses_pcm16_mono_and_float_stereo() {
        let pcm: Vec<u8> = [0i16, 16384, -32768]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let (rate, s) = parse_wav(&wav_bytes(1, 16, 1, 44100, &pcm)).unwrap();
        assert_eq!(rate, 44100);
        assert_eq!(s.data, vec![0.0, 0.0, 0.5, 0.5, -1.0, -1.0]);
        let fl: Vec<u8> = [0.25f32, -0.5]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let (_, s) = parse_wav(&wav_bytes(3, 32, 2, 48000, &fl)).unwrap();
        assert_eq!(s.data, vec![0.25, -0.5]);
        assert!(parse_wav(b"nope").is_err());
    }

    #[test]
    fn hostile_wav_formats_are_rejected_not_panicked() {
        let pcm = [0u8; 64];
        for (tag, bits, ch, rate) in [
            (1u16, 0u16, 2u16, 48_000u32), // bits = 0 divided by zero
            (1, 12, 2, 48_000),
            (1, 7, 2, 48_000),
            (1, 16, 0, 48_000),
            (1, 16, 33, 48_000),
            (1, 16, 2, 0),
            (1, 16, 2, 7_999),
            (1, 16, 2, 768_001),
            (3, 16, 2, 48_000),
            (1, 16, 2, u32::MAX),
        ] {
            let r = parse_wav(&wav_bytes(tag, bits, ch, rate, &pcm));
            assert!(
                r.is_err(),
                "tag {tag} bits {bits} ch {ch} rate {rate} accepted"
            );
        }
        // Bounds themselves are fine.
        assert!(parse_wav(&wav_bytes(1, 16, 32, 8_000, &[0u8; 64])).is_ok());
        assert!(parse_wav(&wav_bytes(1, 16, 1, 768_000, &pcm)).is_ok());
    }

    #[test]
    fn wav_read_is_size_capped() {
        let dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))
            .join("test-tmp");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join(format!("cap-{}.wav", std::process::id()));
        let pcm: Vec<u8> = [0i16; 64].iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::write(&f, wav_bytes(1, 16, 1, 48000, &pcm)).unwrap();
        assert!(read_wav_limited(&f, 1 << 20).is_ok());
        assert!(matches!(
            read_wav_limited(&f, 100),
            Err(RenderError::Audio(_))
        ));
        const { assert!(MAX_WAV_BYTES >= 1 << 30) };
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn resample_changes_length() {
        let s = Stereo {
            data: vec![0.0; 44100 * 2],
        };
        assert_eq!(resample(44100, s).frames(), 48000);
    }

    #[test]
    fn negative_offset_drops_leading_audio() {
        let mut dst = Stereo {
            data: vec![0.0; 10 * 2],
        };
        // 1 ms = 48 frames before video t=0.
        let mut s = Stereo {
            data: vec![0.0; 58 * 2],
        };
        s.data[48 * 2] = 1.0; // the sample at video t = 0
        place(&mut dst, &s, -1.0);
        assert_eq!(dst.data[0], 1.0);
        assert_eq!(dst.data.iter().filter(|&&x| x != 0.0).count(), 1);
    }

    #[test]
    fn high_pass_kills_dc_keeps_voice() {
        let mut dc = Stereo {
            data: vec![0.5; RATE as usize * 2],
        };
        high_pass(&mut dc, 80.0);
        assert!(dc.data[dc.data.len() - 2].abs() < 1e-3);
        let mut v = sine(1000.0, 0.5, 0.5);
        high_pass(&mut v, 80.0);
        let peak = v.data[RATE as usize / 4..]
            .iter()
            .fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.48);
    }

    #[test]
    fn gate_mutes_hiss_passes_speech() {
        let mut hiss = sine(3000.0, 0.001, 1.0); // -60 dBFS
        noise_gate(&mut hiss, -50.0);
        assert!(hiss.data[RATE as usize..].iter().all(|x| x.abs() < 1e-5));
        let mut loud = sine(300.0, 0.3, 1.0);
        noise_gate(&mut loud, -50.0);
        let peak = loud.data[RATE as usize / 2..]
            .iter()
            .fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.29);
    }

    #[test]
    fn loudness_of_reference_sine() {
        // A 1 kHz sine at 0 dBFS peak in both channels reads about -0.0 LUFS
        // (-3.01 per channel + 3.01 for two channels; K-weighting ~ +0.7 dB at 1 kHz).
        let l = integrated_lufs(&sine(1000.0, 1.0, 3.0)).unwrap();
        assert!((l - 0.0).abs() < 1.0, "{l}");
        let l20 = integrated_lufs(&sine(1000.0, 0.1, 3.0)).unwrap();
        assert!((l - l20 - 20.0).abs() < 0.05);
        assert!(
            integrated_lufs(&Stereo {
                data: vec![0.0; 96000 * 2]
            })
            .is_none()
        );
    }

    #[test]
    fn limiter_caps_peaks() {
        let mut s = sine(200.0, 1.6, 0.5);
        limit(&mut s, 0.891);
        assert!(s.data.iter().all(|x| x.abs() <= 0.8911));
    }

    fn tone_wav(dir: &std::path::Path, name: &str, hz: f64, amp: f32) -> std::path::PathBuf {
        let s = sine(hz, amp, 1.0);
        let stereo: Vec<u8> = s
            .data
            .iter()
            .flat_map(|x| ((x * 32767.0) as i16).to_le_bytes())
            .collect();
        let path = dir.join(name);
        std::fs::write(&path, wav_bytes(1, 16, 2, RATE, &stereo)).unwrap();
        path
    }

    fn rms(v: &[i16]) -> f64 {
        (v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / v.len().max(1) as f64).sqrt()
    }

    fn tone_project(tag: &str) -> RenderProject {
        let dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")
            })
            .join("test-tmp")
            .join(format!("render-audio-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut p = RenderProject::empty_for_tests();
        p.duration_ms = 1000.0;
        p.pieces = vec![Piece {
            src_start: 0.0,
            src_end: 1000.0,
            speed: 1.0,
        }];
        p.mic = Some(AudioRef {
            path: tone_wav(&dir, "mic.wav", 440.0, 0.3),
            offset_ms: 0.0,
        });
        p.system = Some(AudioRef {
            path: tone_wav(&dir, "sys.wav", 880.0, 0.3),
            offset_ms: 0.0,
        });
        p.style.audio = AudioStyle::OFF;
        p
    }

    #[test]
    fn gains_scale_and_mutes_remove_each_source() {
        let mut p = tone_project("gain");
        let both = rms(&render(&p).unwrap().unwrap());
        // Mic muted: only the system tone remains (half the power of two equal tones).
        p.style.audio.mic_gain = 0.0;
        let sys_only = rms(&render(&p).unwrap().unwrap());
        assert!(
            (sys_only / both - (0.5f64).sqrt()).abs() < 0.03,
            "{sys_only} vs {both}"
        );
        // Mic muted: its 440 Hz energy is gone entirely (Goertzel at 440 vs 880).
        let out = render(&p).unwrap().unwrap();
        assert!(goertzel(&out, 440.0) < goertzel(&out, 880.0) * 0.02);
        // Both muted: silence, but still an audio track of the right length.
        p.style.audio.system_gain = 0.0;
        let silent = render(&p).unwrap().unwrap();
        assert_eq!(silent.len(), 48_000 * 2);
        assert!(silent.iter().all(|x| *x == 0));
        // Gain 0.5 on the system source halves its level.
        p.style.audio.mic_gain = 0.0;
        p.style.audio.system_gain = 1.0;
        let full = rms(&render(&p).unwrap().unwrap());
        p.style.audio.system_gain = 0.5;
        let half = rms(&render(&p).unwrap().unwrap());
        assert!((half / full - 0.5).abs() < 0.02, "{half} vs {full}");
    }

    /// Magnitude of `hz` in interleaved stereo i16 (left channel).
    fn goertzel(v: &[i16], hz: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * hz / RATE as f64;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, f) in v.chunks(2).enumerate() {
            re += f[0] as f64 * (w * i as f64).cos();
            im += f[0] as f64 * (w * i as f64).sin();
        }
        (re * re + im * im).sqrt() / (v.len() / 2) as f64
    }

    #[test]
    fn remap_cuts_and_speed() {
        let mut p = RenderProject::empty_for_tests();
        p.pieces = vec![
            Piece {
                src_start: 1000.0,
                src_end: 1010.0,
                speed: 1.0,
            },
            Piece {
                src_start: 0.0,
                src_end: 20.0,
                speed: 2.0,
            },
        ];
        // Source: value = frame index.
        let n = 48 * 1100;
        let src = Stereo {
            data: (0..n).flat_map(|i| [i as f32, i as f32]).collect(),
        };
        let out = remap(&src, &p);
        assert_eq!(out.frames(), 20 * 48);
        assert_eq!(out.data[0], 48_000.0);
        assert_eq!(out.data[480 * 2], 0.0);
        assert_eq!(out.data[481 * 2], 2.0);
    }
}
