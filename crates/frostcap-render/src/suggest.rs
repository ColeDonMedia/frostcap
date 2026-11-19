//! Silence / idle cut suggestions: spans where the recording is quiet (mic and
//! system RMS below a threshold) *and* nothing happened on screen (no cursor
//! movement, click or key press). Pure signal processing over the decoded WAVs
//! and the recorded input tracks; no AI, deterministic, unit tested.

use std::path::Path;

use frostcap_project::{Project, Tracks, TrimRange};

use crate::audio::{RATE, Stereo};
use crate::error::Result;

/// Resolution of the analysis (ms): spans are snapped to this grid.
const WINDOW_MS: u64 = 50;
/// Suggestions shorter than this after the margins are dropped.
const MIN_CUT_MS: u64 = 250;

/// Tuning knobs for [`suggest_cuts`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SuggestOptions {
    /// A 50 ms window is "loud" when the RMS of any audio track exceeds this
    /// (dBFS, -100..=0).
    pub rms_threshold_db: f64,
    /// Minimum length of a quiet *and* idle span to suggest (ms, 200..=600 000).
    pub min_idle_ms: u64,
    /// Time kept at each interior edge of a cut, so speech and actions are not
    /// clipped (ms, 0..=5 000). Edges at the very start / end get no margin.
    pub margin_ms: u64,
    /// Cursor travel (px, canvas pixels) since the last activity that counts as
    /// movement (0.5..=200). Smaller jitter is ignored; slow drift accumulates.
    pub cursor_min_px: f64,
}

impl Default for SuggestOptions {
    fn default() -> Self {
        Self {
            rms_threshold_db: -45.0,
            min_idle_ms: 1500,
            margin_ms: 200,
            cursor_min_px: 3.0,
        }
    }
}

impl SuggestOptions {
    /// Clamp every field into its documented range (NaN -> default).
    pub fn sanitized(self) -> Self {
        let d = Self::default();
        let num = |v: f64, lo: f64, hi: f64, dflt: f64| {
            if v.is_finite() { v.clamp(lo, hi) } else { dflt }
        };
        Self {
            rms_threshold_db: num(self.rms_threshold_db, -100.0, 0.0, d.rms_threshold_db),
            min_idle_ms: self.min_idle_ms.clamp(200, 600_000),
            margin_ms: self.margin_ms.min(5000),
            cursor_min_px: num(self.cursor_min_px, 0.5, 200.0, d.cursor_min_px),
        }
    }
}

fn window_count(duration_ms: u64) -> usize {
    duration_ms.div_ceil(WINDOW_MS) as usize
}

/// Per 50 ms window of the video clock: does any track exceed `threshold_db`?
/// A track's sample 0 sits at `offset_ms` (may be negative) on that clock.
fn loud_windows(tracks: &[(Stereo, f64)], n: usize, threshold_db: f64) -> Vec<bool> {
    let mut loud = vec![false; n];
    for (s, offset_ms) in tracks {
        let mut acc = LoudAcc::new(n, *offset_ms);
        acc.feed(&s.data);
        acc.or_into(&mut loud, threshold_db);
    }
    loud
}

/// Streaming per-window energy of one track: feed 48 kHz stereo chunks in order;
/// memory is one `(sum, count)` pair per 50 ms window whatever the length.
struct LoudAcc {
    sum: Vec<f64>,
    count: Vec<u64>,
    /// Video-clock frame of the track's sample 0 (can be negative).
    shift: i64,
    next: i64,
}

impl LoudAcc {
    fn new(n: usize, offset_ms: f64) -> Self {
        let shift = (offset_ms * f64::from(RATE) / 1000.0).round() as i64;
        Self {
            sum: vec![0.0; n],
            count: vec![0; n],
            shift,
            next: 0,
        }
    }

    fn feed(&mut self, stereo: &[f32]) {
        let per_window = (WINDOW_MS * u64::from(RATE) / 1000) as i64;
        for fr in stereo.as_chunks::<2>().0 {
            let v = self.next + self.shift;
            self.next += 1;
            if v < 0 {
                continue;
            }
            let k = (v / per_window) as usize;
            if let (Some(s), Some(c)) = (self.sum.get_mut(k), self.count.get_mut(k)) {
                *s += f64::from(fr[0]).powi(2) + f64::from(fr[1]).powi(2);
                *c += 2;
            }
        }
    }

    fn or_into(&self, loud: &mut [bool], threshold_db: f64) {
        let thr = 10f64.powf(threshold_db / 20.0);
        for (k, flag) in loud.iter_mut().enumerate() {
            if !*flag && self.count[k] > 0 && (self.sum[k] / self.count[k] as f64).sqrt() > thr {
                *flag = true;
            }
        }
    }
}

/// Windows with input activity: a cursor that travelled `cursor_min_px` since
/// its last counted position, any click, any key press.
fn active_windows(t: &Tracks, n: usize, cursor_min_px: f64) -> Vec<bool> {
    let mut active = vec![false; n];
    let mut mark = |t_ms: u64| {
        if let Some(a) = active.get_mut((t_ms / WINDOW_MS) as usize) {
            *a = true;
        }
    };
    let mut anchor: Option<(i32, i32)> = None;
    for c in &t.cursor {
        match anchor {
            None => anchor = Some((c.x, c.y)),
            Some((ax, ay)) => {
                let d = f64::from(c.x - ax).hypot(f64::from(c.y - ay));
                if d >= cursor_min_px {
                    mark(c.t_ms);
                    anchor = Some((c.x, c.y));
                }
            }
        }
    }
    t.clicks.iter().for_each(|c| mark(c.t_ms));
    t.keys.iter().for_each(|k| mark(k.t_ms));
    active
}

/// Suggest cuts from already-decoded audio (`(samples, offsetMs)` per track,
/// 48 kHz stereo). Pure and deterministic. With no audio tracks only input
/// activity decides.
pub fn suggest_cuts_from(
    p: &Project,
    audio: &[(Stereo, f64)],
    opts: &SuggestOptions,
) -> Vec<TrimRange> {
    let n = window_count(p.duration_ms);
    let loud = loud_windows(audio, n, opts.sanitized().rms_threshold_db);
    cuts_from_loud(p, loud, opts)
}

fn cuts_from_loud(p: &Project, loud: Vec<bool>, opts: &SuggestOptions) -> Vec<TrimRange> {
    let o = opts.sanitized();
    let duration = p.duration_ms;
    let n = window_count(duration);
    let active = active_windows(&p.tracks, n, o.cursor_min_px);
    let mut cuts = Vec::new();
    let mut k = 0;
    while k < n {
        if loud[k] || active[k] {
            k += 1;
            continue;
        }
        let start = k;
        while k < n && !loud[k] && !active[k] {
            k += 1;
        }
        let (s, e) = (
            start as u64 * WINDOW_MS,
            (k as u64 * WINDOW_MS).min(duration),
        );
        if e - s < o.min_idle_ms {
            continue;
        }
        let s = if s > 0 { s + o.margin_ms } else { s };
        let e = if e < duration {
            e.saturating_sub(o.margin_ms)
        } else {
            e
        };
        if e > s && e - s >= MIN_CUT_MS {
            cuts.push(TrimRange {
                start_ms: s,
                end_ms: e,
            });
        }
    }
    cuts
}

/// Suggest cuts for a project folder: decodes the mic / system WAVs (a listed
/// file that is missing is skipped, a corrupt one is an error) and analyses
/// them together with the recorded cursor, click and key tracks. The result is
/// a sorted list of non-overlapping trims that [`Project::validate`] accepts.
pub fn suggest_cuts(dir: &Path, p: &Project, opts: &SuggestOptions) -> Result<Vec<TrimRange>> {
    let n = window_count(p.duration_ms);
    let thr = opts.sanitized().rms_threshold_db;
    let mut loud = vec![false; n];
    for t in [&p.media.mic_audio, &p.media.system_audio]
        .into_iter()
        .flatten()
    {
        let path = dir.join(&t.file);
        if !path.is_file() {
            continue;
        }
        let mut acc = LoudAcc::new(n, t.offset_ms as f64);
        let need_ms = (p.duration_ms as f64 - (t.offset_ms as f64).min(0.0)).max(0.0) + 1000.0;
        let lim = crate::wavstream::Limits {
            max_out_frames: (need_ms * f64::from(RATE) / 1000.0).ceil() as u64,
            expect_rate: Some(t.sample_rate),
            expect_channels: Some(t.channels),
        };
        crate::wavstream::stream_48k(&path, lim, &mut |c| acc.feed(c))?;
        acc.or_into(&mut loud, thr);
    }
    Ok(cuts_from_loud(p, loud, opts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{RATE, Stereo};
    use frostcap_project::{
        ButtonState, ClickEvent, CursorSample, KeyEvent, Modifiers, MouseButton, Project,
        TrimRange, samples::sample,
    };

    /// `(start_ms, end_ms, amplitude)` tone segments on a silent 48 kHz stereo track.
    fn track(total_ms: u64, tones: &[(u64, u64, f32)]) -> Stereo {
        let n = (total_ms * u64::from(RATE) / 1000) as usize;
        let mut data = vec![0.0f32; n * 2];
        for &(a, b, amp) in tones {
            let (a, b) = (
                (a * u64::from(RATE) / 1000) as usize,
                ((b * u64::from(RATE) / 1000) as usize).min(n),
            );
            for i in a..b {
                let v = amp * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / RATE as f32).sin();
                data[i * 2] = v;
                data[i * 2 + 1] = v;
            }
        }
        Stereo { data }
    }

    fn project(duration_ms: u64) -> Project {
        let mut p = sample();
        p.duration_ms = duration_ms;
        p.tracks.cursor.clear();
        p.tracks.clicks.clear();
        p.tracks.keys.clear();
        p
    }

    fn cut(a: u64, b: u64) -> TrimRange {
        TrimRange {
            start_ms: a,
            end_ms: b,
        }
    }

    fn opts() -> SuggestOptions {
        SuggestOptions::default()
    }

    #[test]
    fn trailing_silence_after_speech_is_suggested_with_a_margin() {
        let p = project(10_000);
        let mic = track(10_000, &[(0, 3000, 0.3)]);
        let cuts = suggest_cuts_from(&p, &[(mic, 0.0)], &opts());
        // Speech ends at 3000; keep the default 200 ms margin, cut to the end.
        assert_eq!(cuts, vec![cut(3200, 10_000)]);
    }

    #[test]
    fn speech_in_the_middle_splits_the_silence_into_two_cuts() {
        let p = project(8000);
        let mic = track(8000, &[(3000, 4000, 0.3)]);
        let cuts = suggest_cuts_from(&p, &[(mic, 0.0)], &opts());
        assert_eq!(cuts, vec![cut(0, 2800), cut(4200, 8000)]);
    }

    #[test]
    fn silence_shorter_than_the_minimum_is_kept() {
        let p = project(6000);
        // 1000 ms gap between two utterances; the default minimum is 1500 ms.
        let mic = track(6000, &[(0, 2000, 0.3), (3000, 6000, 0.3)]);
        assert!(suggest_cuts_from(&p, &[(mic.clone(), 0.0)], &opts()).is_empty());
        let mut o = opts();
        o.min_idle_ms = 800;
        o.margin_ms = 100;
        assert_eq!(
            suggest_cuts_from(&p, &[(mic, 0.0)], &o),
            vec![cut(2100, 2900)]
        );
    }

    #[test]
    fn quiet_noise_below_the_threshold_counts_as_silence_loud_does_not() {
        let p = project(6000);
        let hiss = track(6000, &[(0, 6000, 0.001)]); // -60 dBFS-ish
        assert_eq!(
            suggest_cuts_from(&p, &[(hiss, 0.0)], &opts()),
            vec![cut(0, 6000)]
        );
        let loud = track(6000, &[(0, 6000, 0.05)]); // about -29 dBFS RMS
        assert!(suggest_cuts_from(&p, &[(loud, 0.0)], &opts()).is_empty());
        // Either track being loud keeps the span: system audio plays while the mic is silent.
        let mic = track(6000, &[]);
        let sys = track(6000, &[(2000, 3000, 0.3)]);
        let cuts = suggest_cuts_from(&p, &[(mic, 0.0), (sys, 0.0)], &opts());
        assert_eq!(cuts, vec![cut(0, 1800), cut(3200, 6000)]);
    }

    #[test]
    fn audio_offsets_move_the_loud_span_on_the_video_clock() {
        let p = project(8000);
        // The mic file starts 2000 ms into the video; speech at file time 0..1000.
        let mic = track(5000, &[(0, 1000, 0.3)]);
        let cuts = suggest_cuts_from(&p, &[(mic.clone(), 2000.0)], &opts());
        assert_eq!(cuts, vec![cut(0, 1800), cut(3200, 8000)]);
        // A negative offset drops the head of the file instead.
        let cuts = suggest_cuts_from(&p, &[(mic, -500.0)], &opts());
        assert_eq!(cuts, vec![cut(700, 8000)]);
    }

    #[test]
    fn input_activity_keeps_a_silent_span_alive() {
        let mut p = project(10_000);
        let silent = track(10_000, &[]);
        p.tracks.cursor = vec![
            CursorSample {
                t_ms: 0,
                x: 100,
                y: 100,
            },
            CursorSample {
                t_ms: 5000,
                x: 400,
                y: 100,
            },
        ];
        let cuts = suggest_cuts_from(&p, &[(silent.clone(), 0.0)], &opts());
        // The jump at 5000 ms is activity: two idle spans around it, margins on both sides.
        assert_eq!(cuts, vec![cut(0, 4800), cut(5250, 10_000)]);
        // A click and a key press count too.
        let mut p = project(10_000);
        p.tracks.clicks = vec![ClickEvent {
            t_ms: 3000,
            x: 1,
            y: 1,
            button: MouseButton::Left,
            state: ButtonState::Down,
        }];
        p.tracks.keys = vec![KeyEvent {
            t_ms: 7000,
            vk: 65,
            label: "A".into(),
            modifiers: Modifiers::default(),
        }];
        let cuts = suggest_cuts_from(&p, &[(silent, 0.0)], &opts());
        assert_eq!(cuts, vec![cut(0, 2800), cut(3250, 6800), cut(7250, 10_000)]);
    }

    #[test]
    fn cursor_jitter_and_slow_drift_are_handled() {
        let mut p = project(10_000);
        // 1 px jitter around a resting position is not activity...
        p.tracks.cursor = (0..100)
            .map(|i| CursorSample {
                t_ms: i * 100,
                x: 500 + (i % 2) as i32,
                y: 300,
            })
            .collect();
        assert_eq!(
            suggest_cuts_from(&p, &[], &opts()),
            vec![cut(0, 10_000)],
            "input only, no audio tracks"
        );
        // ...but a drift of 1 px per sample accumulates past the threshold.
        p.tracks.cursor = (0..100)
            .map(|i| CursorSample {
                t_ms: i * 100,
                x: 500 + i as i32,
                y: 300,
            })
            .collect();
        assert!(suggest_cuts_from(&p, &[], &opts()).is_empty());
    }

    #[test]
    fn whole_recording_quiet_and_idle_is_one_cut_and_results_are_valid_trims() {
        let mut p = project(4000);
        let cuts = suggest_cuts_from(&p, &[(track(4000, &[]), 0.0)], &opts());
        assert_eq!(cuts, vec![cut(0, 4000)]);
        p.edits.trims = cuts;
        p.validate().unwrap();
        // Empty recording, nothing to suggest.
        assert!(suggest_cuts_from(&project(0), &[], &opts()).is_empty());
    }

    #[test]
    fn options_are_clamped_and_nan_falls_back() {
        let o = SuggestOptions {
            rms_threshold_db: f64::NAN,
            min_idle_ms: 0,
            margin_ms: u64::MAX,
            cursor_min_px: f64::NAN,
        }
        .sanitized();
        let d = SuggestOptions::default();
        assert_eq!(o.rms_threshold_db, d.rms_threshold_db);
        assert!(o.min_idle_ms >= 200);
        assert!(o.margin_ms <= 5000);
        assert_eq!(o.cursor_min_px, d.cursor_min_px);
        assert_eq!(
            SuggestOptions {
                rms_threshold_db: 20.0,
                ..d
            }
            .sanitized()
            .rms_threshold_db,
            0.0
        );
    }

    #[test]
    fn streaming_analysis_matches_the_in_memory_one_and_is_bounded() {
        let dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")
            })
            .join("test-tmp")
            .join(format!("suggest-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A 44.1 kHz mono 16-bit file: speech 2..3 s of 8 s.
        let rate = 44_100u32;
        let pcm: Vec<u8> = (0..rate as usize * 8)
            .flat_map(|i| {
                let t = i as f32 / rate as f32;
                let v = if (2.0..3.0).contains(&t) {
                    0.3 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
                } else {
                    0.0
                };
                ((v * 32767.0) as i16).to_le_bytes()
            })
            .collect();
        let mut w = b"RIFF".to_vec();
        w.extend((36 + pcm.len() as u32).to_le_bytes());
        w.extend(b"WAVEfmt ");
        w.extend(16u32.to_le_bytes());
        w.extend([1, 0, 1, 0]);
        w.extend(rate.to_le_bytes());
        w.extend((rate * 2).to_le_bytes());
        w.extend([2, 0, 16, 0]);
        w.extend(b"data");
        w.extend((pcm.len() as u32).to_le_bytes());
        w.extend(&pcm);
        std::fs::write(dir.join("audio-mic.wav"), &w).unwrap();
        let mut p = project(8000);
        p.media.mic_audio = Some(frostcap_project::AudioTrack {
            file: "audio-mic.wav".into(),
            offset_ms: 0,
            sample_rate: rate,
            channels: 1,
            duration_ms: 8000,
        });
        let streamed = suggest_cuts(&dir, &p, &opts()).unwrap();
        let (r, s) = crate::audio::parse_wav(&w).unwrap();
        let mem = suggest_cuts_from(&p, &[(crate::audio::resample(r, s), 0.0)], &opts());
        assert_eq!(streamed, mem);
        assert_eq!(streamed, vec![cut(0, 1800), cut(3200, 8000)]);
        // A header that disagrees with the project is refused, not analysed.
        p.media.mic_audio.as_mut().unwrap().sample_rate = 48_000;
        assert!(suggest_cuts(&dir, &p, &opts()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn write_wav(path: &std::path::Path, s: &Stereo) {
        let pcm: Vec<u8> = s
            .data
            .iter()
            .flat_map(|x| ((x * 32767.0) as i16).to_le_bytes())
            .collect();
        let mut w = b"RIFF".to_vec();
        w.extend((36 + pcm.len() as u32).to_le_bytes());
        w.extend(b"WAVEfmt ");
        w.extend(16u32.to_le_bytes());
        w.extend(1u16.to_le_bytes());
        w.extend(2u16.to_le_bytes());
        w.extend(RATE.to_le_bytes());
        w.extend((RATE * 4).to_le_bytes());
        w.extend(4u16.to_le_bytes());
        w.extend(16u16.to_le_bytes());
        w.extend(b"data");
        w.extend((pcm.len() as u32).to_le_bytes());
        w.extend(pcm);
        std::fs::write(path, w).unwrap();
    }

    #[test]
    fn suggest_cuts_reads_the_projects_wav_files_and_skips_missing_ones() {
        let dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")
            })
            .join("test-tmp")
            .join(format!("suggest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write_wav(&dir.join("audio-mic.wav"), &track(6000, &[(0, 1000, 0.3)]));
        let mut p = project(6000);
        let t = |f: &str| frostcap_project::AudioTrack {
            file: f.into(),
            offset_ms: 0,
            sample_rate: RATE,
            channels: 2,
            duration_ms: 6000,
        };
        p.media.mic_audio = Some(t("audio-mic.wav"));
        p.media.system_audio = Some(t("audio-system.wav")); // not on disk
        let cuts = suggest_cuts(&dir, &p, &opts()).unwrap();
        assert_eq!(cuts, vec![cut(1200, 6000)]);
        // A corrupt file is an error, not a silent "everything is quiet".
        std::fs::write(dir.join("audio-mic.wav"), b"not a wav").unwrap();
        assert!(suggest_cuts(&dir, &p, &opts()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
