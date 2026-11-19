//! WASAPI audio capture to WAV (microphone and system loopback).
//!
//! Each endpoint runs on its own thread in shared mode using the device mix format
//! (typically 48 kHz float stereo), written verbatim into a RIFF/WAVE file.
//!
//! Alignment: every packet's QPC position is mapped through the recording
//! [`Timeline`]. Packets before the first video frame or during a pause are
//! dropped, and silence is inserted whenever the file falls behind the timeline
//! (loopback delivers nothing while nothing plays). The WAV therefore starts at
//! video t = 0 and stays aligned with `screen.mp4` (`offsetMs` = 0, within one
//! device period).
//!
//! Drift: the position is re-anchored at **every packet** (silence is inserted when the
//! file is behind, surplus samples are dropped when it is ahead, beyond a 20 ms
//! tolerance), so a device clock that is a few hundred ppm off never accumulates.
//!
//! Long-recording safety: the RIFF header is re-patched every few seconds so a
//! crash leaves a playable file; write errors (e.g. disk full) end the thread
//! with an error that the session reports.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX,
    eCapture, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};

use crate::clock::{self, HNS_PER_SEC};
use crate::error::{Error, Result};
use crate::timeline::Timeline;

/// Which endpoint to capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioEndpoint {
    /// Default console capture device.
    Microphone,
    /// Default render device, captured via loopback.
    SystemLoopback,
}

/// Information about a finished audio capture.
#[derive(Clone, Debug)]
pub struct AudioCaptureInfo {
    /// Written file.
    pub path: PathBuf,
    /// Output-timeline position of the first sample (hns, normally 0).
    pub start_pos_hns: i64,
    /// Sample rate.
    pub sample_rate: u32,
    /// Channels.
    pub channels: u16,
    /// Frames written.
    pub frames: u64,
    /// Frames of silence inserted to keep sync.
    pub padded_frames: u64,
    /// Frames dropped to keep sync (device clock faster than the video clock).
    pub dropped_frames: u64,
    /// Frames of a timeline gap that were too large to pad in one go (device fault).
    pub unfilled_frames: u64,
    /// Packets WASAPI flagged as discontinuity or bad timestamp.
    pub glitch_packets: u64,
    /// Why capture ended early, if it did (the partial WAV is kept).
    pub error: Option<crate::error::ErrorPayload>,
}

impl AudioCaptureInfo {
    /// Duration in ms.
    pub fn duration_ms(&self) -> u64 {
        self.frames * 1000 / u64::from(self.sample_rate.max(1))
    }
}

/// Running capture thread. Dropping it stops the thread and finalizes the file.
pub struct AudioRecorder {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<AudioCaptureInfo>>>,
}

impl AudioRecorder {
    /// Open the endpoint and start capturing into `path`. Returns once the device is
    /// running (or failed to open).
    pub fn start(endpoint: AudioEndpoint, path: &Path, timeline: Arc<Timeline>) -> Result<Self> {
        crate::com::ensure_mta();
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let path = path.to_path_buf();
        let handle = thread::Builder::new()
            .name(format!("frostcap-audio-{endpoint:?}"))
            .spawn({
                let stop = stop.clone();
                move || run(endpoint, &path, &timeline, &stop, &ready_tx)
            })
            .map_err(|e| Error::Audio(e.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                stop,
                handle: Some(handle),
            }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => Err(match handle.join() {
                Ok(Err(e)) => e,
                _ => Error::Audio("audio thread died".into()),
            }),
        }
    }

    /// The thread ended on its own (error such as disk full or device removed).
    pub fn has_failed(&self) -> bool {
        self.handle.as_ref().is_some_and(JoinHandle::is_finished)
    }

    /// Stop and finalize the WAV file.
    pub fn stop(&mut self) -> Result<AudioCaptureInfo> {
        self.stop.store(true, Ordering::Release);
        match self.handle.take().map(JoinHandle::join) {
            Some(Ok(r)) => r,
            Some(Err(_)) => Err(Error::Audio("audio thread panicked".into())),
            None => Err(Error::Invalid("audio already stopped".into())),
        }
    }
}

impl Drop for AudioRecorder {
    fn drop(&mut self) {
        if self.handle.is_some() {
            let _ = self.stop();
        }
    }
}

struct ComGuard;
impl ComGuard {
    fn new() -> Result<Self> {
        // SAFETY: balanced by CoUninitialize in Drop.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
        Ok(Self)
    }
}
impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: paired with a successful CoInitializeEx.
        unsafe { CoUninitialize() };
    }
}

/// Mix format copied out of CoTaskMem: the raw `fmt ` chunk bytes plus key fields.
struct Format {
    fmt_chunk: Vec<u8>,
    sample_rate: u32,
    channels: u16,
    block_align: u16,
}

/// # Safety
/// `p` must point to a valid `WAVEFORMATEX` (possibly extensible) from `GetMixFormat`.
unsafe fn copy_format(p: *const WAVEFORMATEX) -> Format {
    // SAFETY: caller guarantees validity; WAVEFORMATEX is packed so read unaligned.
    let wf = unsafe { std::ptr::read_unaligned(p) };
    let extra = if wf.wFormatTag == 1 {
        0
    } else {
        usize::from(wf.cbSize)
    };
    let len = size_of::<WAVEFORMATEX>() + extra;
    // SAFETY: the allocation is at least `len` bytes per the WAVEFORMATEX contract.
    let fmt_chunk = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), len) }.to_vec();
    Format {
        fmt_chunk,
        sample_rate: wf.nSamplesPerSec,
        channels: wf.nChannels,
        block_align: wf.nBlockAlign,
    }
}

/// Minimal streaming WAV writer: RIFF, upgraded in place to RF64 beyond 4 GiB.
pub(crate) struct WavWriter<W: Write + Seek = File> {
    w: BufWriter<W>,
    header: crate::wav::Header,
    block: u16,
    data_bytes: u64,
}

impl WavWriter<File> {
    pub(crate) fn create(path: &Path, fmt_chunk: &[u8]) -> std::io::Result<Self> {
        Self::with_sink(File::create(path)?, fmt_chunk)
    }
}

impl<W: Write + Seek> WavWriter<W> {
    pub(crate) fn with_sink(sink: W, fmt_chunk: &[u8]) -> std::io::Result<Self> {
        let header = crate::wav::header(fmt_chunk);
        let mut w = BufWriter::with_capacity(256 * 1024, sink);
        w.write_all(&header.bytes)?;
        let block = fmt_chunk
            .get(12..14)
            .map_or(1, |b| u16::from_le_bytes([b[0], b[1]]));
        Ok(Self {
            w,
            header,
            block,
            data_bytes: 0,
        })
    }
    pub(crate) fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.data_bytes += bytes.len() as u64;
        self.w.write_all(bytes)
    }
    pub(crate) fn silence(&mut self, n: usize) -> std::io::Result<()> {
        const ZEROS: [u8; 4096] = [0; 4096];
        let mut left = n;
        while left > 0 {
            let k = left.min(ZEROS.len());
            self.write(&ZEROS[..k])?;
            left -= k;
        }
        Ok(())
    }
    fn patch_sizes(&mut self) -> std::io::Result<()> {
        let end = self.w.stream_position()?;
        for (off, bytes) in crate::wav::patch_ops(&self.header, end, self.data_bytes, self.block) {
            self.w.seek(SeekFrom::Start(off))?;
            self.w.write_all(&bytes)?;
        }
        self.w.seek(SeekFrom::Start(end))?;
        Ok(())
    }
    /// Flush and patch the header so the file is valid if the process dies.
    pub(crate) fn checkpoint(&mut self) -> std::io::Result<()> {
        self.patch_sizes()?;
        self.w.flush()
    }
    /// Pad to an even length, patch the header and hand back the sink.
    pub(crate) fn finish(mut self) -> std::io::Result<W> {
        if self.data_bytes % 2 == 1 {
            self.w.write_all(&[0])?;
        }
        self.patch_sizes()?;
        self.w
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)
    }
}

/// Loopback delivers late: pad the silent stretch only up to `now - 50 ms`.
const LATENCY_ALLOWANCE_HNS: i64 = 50 * clock::HNS_PER_MS;

/// Silence frames to insert so a file of `frames` frames reaches output position
/// `pos_hns`, ignoring gaps up to `slack` frames (device jitter). Never negative.
fn pad_frames(pos_hns: i64, rate: u32, frames: u64, slack: u64) -> u64 {
    let expected = frames_at(pos_hns, rate);
    if expected > frames.saturating_add(slack) {
        expected - frames
    } else {
        0
    }
}

/// What to do with one packet so the file stays on the timeline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PacketPlan {
    /// Frames of silence to write first (file is behind: slow device clock, stall).
    pad: u64,
    /// Frames to drop from the start of the packet (file is ahead: fast device clock).
    skip: u64,
    /// Frames of a gap that were NOT padded because of [`MAX_PAD_SECS`] (reported as a
    /// discontinuity; later packets keep closing the gap one bound at a time).
    excess: u64,
}

/// Most silence written for one packet (seconds). A larger gap is a clock or device
/// fault, not something to fill in one go.
const MAX_PAD_SECS: u64 = 1;

/// The WASAPI buffer flags that change how a packet is aligned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PacketFlags {
    /// `AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR`: the QPC position is unreliable.
    timestamp_error: bool,
    /// `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`: the device dropped or glitched data.
    discontinuity: bool,
}

impl PacketFlags {
    fn from_wasapi(flags: u32) -> Self {
        Self {
            timestamp_error: flags & (AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32) != 0,
            discontinuity: flags & (AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32) != 0,
        }
    }
}

/// Re-anchor the file to the timeline at every packet: `pos_hns` is the packet's own
/// QPC position mapped through the timeline, `frames` the frames written so far and
/// `n` the packet length. Differences up to `slack` frames (device jitter) are kept;
/// beyond that silence is inserted or surplus samples are dropped, so the device
/// clock's drift against the QPC never accumulates over long recordings.
fn plan_packet(
    pos_hns: i64,
    rate: u32,
    frames: u64,
    n: u64,
    slack: u64,
    flags: PacketFlags,
) -> PacketPlan {
    // A bad timestamp says nothing about where the packet belongs: keep it as it is.
    if flags.timestamp_error {
        return PacketPlan::default();
    }
    let expected = frames_at(pos_hns, rate);
    let max_pad = u64::from(rate) * MAX_PAD_SECS;
    // After a reported glitch the gap is filled even when it is within the jitter slack.
    let tolerance = if flags.discontinuity { 0 } else { slack };
    if expected > frames.saturating_add(tolerance) {
        let gap = expected - frames;
        PacketPlan {
            pad: gap.min(max_pad),
            skip: 0,
            excess: gap.saturating_sub(max_pad),
        }
    } else if frames > expected.saturating_add(slack) && !flags.discontinuity {
        PacketPlan {
            pad: 0,
            skip: (frames - expected).min(n),
            excess: 0,
        }
    } else {
        PacketPlan::default()
    }
}

/// Frames the file must contain to reach output position `pos_hns`.
fn frames_at(pos_hns: i64, rate: u32) -> u64 {
    (i128::from(pos_hns.max(0)) * i128::from(rate) / i128::from(HNS_PER_SEC)) as u64
}

fn run(
    endpoint: AudioEndpoint,
    path: &Path,
    timeline: &Timeline,
    stop: &AtomicBool,
    ready: &mpsc::Sender<Result<()>>,
) -> Result<AudioCaptureInfo> {
    // COM guard first: bindings drop in reverse order, so COM is released last.
    let (_com, client, capture, fmt, mut wav) = match open(endpoint, path) {
        Ok(v) => {
            let _ = ready.send(Ok(()));
            v
        }
        Err(e) => {
            let _ = std::fs::remove_file(path);
            let _ = ready.send(Err(Error::Coded(e.to_payload())));
            return Err(e);
        }
    };
    let io = |e: std::io::Error| Error::io(path, e);
    let block = usize::from(fmt.block_align);
    let rate = fmt.sample_rate;
    let slack = u64::from(rate) / 50; // 20 ms jitter tolerance
    let mut start_pos: Option<i64> = None;
    let mut frames: u64 = 0;
    let mut padded: u64 = 0;
    let mut dropped: u64 = 0;
    let mut unfilled: u64 = 0;
    let mut glitches: u64 = 0;
    let mut last_checkpoint = Instant::now();

    let result: Result<()> = (|| {
        while !stop.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(10));
            loop {
                // SAFETY: COM calls on interfaces owned by this thread.
                let packet = unsafe { capture.GetNextPacketSize() }?;
                if packet == 0 {
                    break;
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut n: u32 = 0;
                let mut flags: u32 = 0;
                let mut qpc: u64 = 0;
                // SAFETY: valid out params; buffer released below.
                unsafe { capture.GetBuffer(&mut data, &mut n, &mut flags, None, Some(&mut qpc)) }?;
                let pflags = PacketFlags::from_wasapi(flags);
                let ts = if qpc > 0 && !pflags.timestamp_error {
                    qpc as i64
                } else {
                    clock::now_hns()
                };
                let write = (|| -> Result<()> {
                    let Some(pos) = timeline.map(ts) else {
                        return Ok(());
                    };
                    start_pos.get_or_insert(0);
                    let plan = plan_packet(pos, rate, frames, u64::from(n), slack, pflags);
                    unfilled += plan.excess;
                    glitches += u64::from(pflags.discontinuity || pflags.timestamp_error);
                    if plan.pad > 0 {
                        wav.silence(plan.pad as usize * block).map_err(io)?;
                        padded += plan.pad;
                        frames += plan.pad;
                    }
                    // Device clock ahead of the timeline: drop the surplus from the packet start.
                    let skip = plan.skip as usize;
                    dropped += plan.skip;
                    let bytes = (n as usize - skip) * block;
                    if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                        wav.silence(bytes).map_err(io)?;
                    } else {
                        // SAFETY: WASAPI guarantees `n * block_align` readable bytes.
                        let all = unsafe { std::slice::from_raw_parts(data, n as usize * block) };
                        wav.write(&all[skip * block..]).map_err(io)?;
                    }
                    frames += u64::from(n) - plan.skip;
                    Ok(())
                })();
                // SAFETY: releasing the buffer obtained above.
                unsafe { capture.ReleaseBuffer(n) }?;
                write?;
            }
            // Keep up with the timeline while no packets arrive (silence / stalls).
            if let Some(pos) = timeline.position(clock::now_hns()) {
                let pad = pad_frames(pos - LATENCY_ALLOWANCE_HNS, rate, frames, slack);
                if pad > 0 {
                    start_pos.get_or_insert(0);
                    wav.silence(pad as usize * block).map_err(io)?;
                    padded += pad;
                    frames += pad;
                }
            }
            if last_checkpoint.elapsed() >= Duration::from_secs(3) {
                wav.checkpoint().map_err(io)?;
                last_checkpoint = Instant::now();
            }
        }
        Ok(())
    })();
    // Pad to the end of the timeline so the WAV ends with the video.
    if result.is_ok()
        && start_pos.is_some()
        && let Some(pos) = timeline.position(clock::now_hns())
    {
        let pad = pad_frames(pos, rate, frames, 0);
        if pad > 0 && wav.silence(pad as usize * block).is_ok() {
            padded += pad;
            frames += pad;
        }
    }
    // SAFETY: stopping the client we started.
    let _ = unsafe { client.Stop() };
    let finished = wav.finish().and_then(|f| f.sync_all()).map_err(io);
    let failure = result.err().or(finished.err());
    if frames == 0
        && let Some(e) = failure
    {
        return Err(e);
    }
    // Partial audio is kept; the error (if any) travels with the result.
    let error = failure.map(|e| e.to_payload());
    Ok(AudioCaptureInfo {
        error,
        path: path.to_path_buf(),
        start_pos_hns: start_pos.unwrap_or(0),
        sample_rate: fmt.sample_rate,
        channels: fmt.channels,
        frames,
        padded_frames: padded,
        dropped_frames: dropped,
        unfilled_frames: unfilled,
        glitch_packets: glitches,
    })
}

type Opened = (
    ComGuard,
    IAudioClient,
    IAudioCaptureClient,
    Format,
    WavWriter,
);

fn open(endpoint: AudioEndpoint, path: &Path) -> Result<Opened> {
    let com = ComGuard::new()?;
    // SAFETY: standard WASAPI initialization sequence; the mix format is freed with
    // CoTaskMemFree after being copied.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let flow = match endpoint {
            AudioEndpoint::Microphone => eCapture,
            AudioEndpoint::SystemLoopback => eRender,
        };
        let device = enumerator
            .GetDefaultAudioEndpoint(flow, eConsole)
            .map_err(|e| {
                Error::Coded(crate::error::ErrorPayload {
                    code: crate::error::ErrorCode::SourceGone,
                    message: format!("no default {endpoint:?} device: {e}"),
                })
            })?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let mix = client.GetMixFormat()?;
        let fmt = copy_format(mix);
        let stream_flags = match endpoint {
            AudioEndpoint::Microphone => 0,
            AudioEndpoint::SystemLoopback => AUDCLNT_STREAMFLAGS_LOOPBACK,
        };
        // 200 ms buffer; we drain every ~10 ms.
        let init = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            stream_flags,
            2_000_000,
            0,
            mix,
            None,
        );
        CoTaskMemFree(Some(mix as *const _));
        init?;
        let capture: IAudioCaptureClient = client.GetService()?;
        let wav = WavWriter::create(path, &fmt.fmt_chunk).map_err(|e| Error::io(path, e))?;
        client.Start()?;
        Ok((com, client, capture, fmt, wav))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_aligns_the_file_to_the_timeline() {
        let r = 48_000;
        // File is at 0 frames, packet maps to 100 ms: pad 4800 frames.
        assert_eq!(pad_frames(100 * clock::HNS_PER_MS, r, 0, 960), 4_800);
        // Within jitter tolerance (20 ms = 960 frames): no padding.
        assert_eq!(pad_frames(100 * clock::HNS_PER_MS, r, 4_000, 960), 0);
        // Ahead of the timeline (device clock faster): never negative.
        assert_eq!(pad_frames(100 * clock::HNS_PER_MS, r, 9_000, 960), 0);
        // Silent loopback: keep up with "now" minus latency allowance.
        assert_eq!(
            pad_frames(
                1_000 * clock::HNS_PER_MS - 50 * clock::HNS_PER_MS,
                r,
                0,
                960
            ),
            45_600
        );
    }

    /// Simulated device clock: the device produces `rate * (1 + ppm/1e6)` frames per
    /// true second; each packet carries its true start time (QPC). The file must stay
    /// within one packet + the jitter slack of the timeline for the whole hour.
    fn simulate(ppm: f64, secs: u64) -> (u64, u64, u64) {
        let rate = 48_000u32;
        let slack = u64::from(rate) / 50;
        let packet = 480u64;
        let dev_rate = f64::from(rate) * (1.0 + ppm / 1e6);
        let (mut frames, mut padded, mut dropped) = (0u64, 0u64, 0u64);
        let mut worst = 0i64;
        let mut k = 0u64;
        loop {
            let true_s = (k * packet) as f64 / dev_rate;
            if true_s >= secs as f64 {
                break;
            }
            let pos = (true_s * HNS_PER_SEC as f64) as i64;
            let plan = plan_packet(pos, rate, frames, packet, slack, PacketFlags::default());
            padded += plan.pad;
            dropped += plan.skip;
            frames += plan.pad + packet - plan.skip;
            let drift = frames as i64
                - frames_at(pos + packet as i64 * HNS_PER_SEC / i64::from(rate), rate) as i64;
            worst = worst.max(drift.abs());
            k += 1;
        }
        assert!(
            worst <= (slack + packet) as i64,
            "drift {worst} frames at {ppm} ppm"
        );
        (padded, dropped, frames)
    }

    #[test]
    fn fast_device_clock_is_trimmed_not_accumulated() {
        let (padded, dropped, _) = simulate(500.0, 3600);
        assert_eq!(padded, 0);
        // 500 ppm over an hour = 1.8 s of surplus audio (86 400 frames), dropped in steps.
        assert!((80_000..=90_000).contains(&dropped), "{dropped}");
    }

    #[test]
    fn slow_device_clock_is_padded_with_silence() {
        let (padded, dropped, _) = simulate(-500.0, 3600);
        assert_eq!(dropped, 0);
        assert!((80_000..=90_000).contains(&padded), "{padded}");
    }

    #[test]
    fn perfect_clock_is_left_alone() {
        assert_eq!(simulate(0.0, 600), (0, 0, 600 * 48_000));
    }

    #[test]
    fn small_jitter_inside_the_slack_is_ignored() {
        let p = plan_packet(
            100 * clock::HNS_PER_MS,
            48_000,
            4_000,
            480,
            960,
            PacketFlags::default(),
        );
        assert_eq!((p.pad, p.skip), (0, 0));
        // 1 s ahead of the timeline: drop the surplus, never more than the packet.
        let p = plan_packet(0, 48_000, 48_000, 480, 960, PacketFlags::default());
        assert_eq!((p.pad, p.skip), (0, 480));
    }

    const BAD_TS: PacketFlags = PacketFlags {
        timestamp_error: true,
        discontinuity: false,
    };
    const GLITCH: PacketFlags = PacketFlags {
        timestamp_error: false,
        discontinuity: true,
    };

    #[test]
    fn a_timestamp_error_never_re_anchors() {
        // 10 s "ahead" and 10 s "behind" timestamps are both ignored.
        for pos in [0, 20 * HNS_PER_SEC] {
            let p = plan_packet(10 * HNS_PER_SEC, 48_000, 48_000 * 10, 480, 960, BAD_TS);
            assert_eq!((p.pad, p.skip, p.excess), (0, 0, 0));
            let p = plan_packet(pos, 48_000, 48_000 * 10, 480, 960, BAD_TS);
            assert_eq!((p.pad, p.skip, p.excess), (0, 0, 0), "pos {pos}");
        }
    }

    #[test]
    fn padding_is_bounded_to_one_second_and_the_rest_is_reported() {
        // File is 5 s behind: pad 1 s now, report 4 s as not filled.
        let p = plan_packet(5 * HNS_PER_SEC, 48_000, 0, 480, 960, PacketFlags::default());
        assert_eq!(p.pad, 48_000);
        assert_eq!(p.excess, 4 * 48_000);
        // A gap under the bound is filled completely with nothing reported.
        let p = plan_packet(HNS_PER_SEC / 2, 48_000, 0, 480, 960, PacketFlags::default());
        assert_eq!((p.pad, p.excess), (24_000, 0));
    }

    #[test]
    fn a_discontinuity_pads_even_a_small_gap_up_to_the_bound() {
        // 10 ms behind is inside the jitter slack for a normal packet ...
        let behind = 100 * clock::HNS_PER_MS;
        let normal = plan_packet(behind, 48_000, 4_000, 480, 960, PacketFlags::default());
        assert_eq!(normal.pad, 0);
        // ... but after a reported glitch the gap is filled.
        let g = plan_packet(behind, 48_000, 4_000, 480, 960, GLITCH);
        assert_eq!(g.pad, 4_800 - 4_000);
        let g = plan_packet(30 * HNS_PER_SEC, 48_000, 0, 480, 960, GLITCH);
        assert_eq!((g.pad, g.excess), (48_000, 29 * 48_000));
        // A discontinuity never drops audio.
        let g = plan_packet(0, 48_000, 48_000, 480, 960, GLITCH);
        assert_eq!(g.skip, 0);
    }

    #[test]
    fn frames_at_rounds_down() {
        assert_eq!(frames_at(HNS_PER_SEC, 48_000), 48_000);
        assert_eq!(frames_at(-5, 48_000), 0);
        assert_eq!(frames_at(clock::HNS_PER_MS, 48_000), 48);
    }

    #[test]
    fn wav_writer_checkpoint_and_finish_are_valid() {
        let dir = crate::test_tmp_dir();
        let path = dir.join(format!("wav-{}.wav", std::process::id()));
        // PCM 16-bit mono 8 kHz fmt chunk.
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1u16.to_le_bytes());
        fmt.extend_from_slice(&1u16.to_le_bytes());
        fmt.extend_from_slice(&8000u32.to_le_bytes());
        fmt.extend_from_slice(&16000u32.to_le_bytes());
        fmt.extend_from_slice(&2u16.to_le_bytes());
        fmt.extend_from_slice(&16u16.to_le_bytes());
        let mut w = WavWriter::create(&path, &fmt).unwrap();
        w.write(&[1, 2, 3, 4]).unwrap();
        w.checkpoint().unwrap();
        let b = std::fs::read(&path).unwrap();
        let info = crate::wav::parse(&b, b.len() as u64).unwrap();
        assert_eq!(info.data_len, 4);
        w.silence(6).unwrap();
        w.finish().unwrap();
        let b = std::fs::read(&path).unwrap();
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize,
            b.len() - 8
        );
        let info = crate::wav::parse(&b, b.len() as u64).unwrap();
        assert_eq!(info.data_len, 10);
        let _ = std::fs::remove_file(&path);
    }

    /// Sink that keeps only the first bytes of what is written (a 5 GiB file without 5 GiB of disk).
    #[derive(Default)]
    struct HeadSink {
        head: Vec<u8>,
        pos: u64,
        len: u64,
    }
    impl Write for HeadSink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            let end = self.pos + b.len() as u64;
            if self.pos < 512 {
                let keep = (512 - self.pos).min(b.len() as u64) as usize;
                if self.head.len() < self.pos as usize + keep {
                    self.head.resize(self.pos as usize + keep, 0);
                }
                self.head[self.pos as usize..self.pos as usize + keep].copy_from_slice(&b[..keep]);
            }
            self.pos = end;
            self.len = self.len.max(end);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Seek for HeadSink {
        fn seek(&mut self, p: SeekFrom) -> std::io::Result<u64> {
            self.pos = match p {
                SeekFrom::Start(o) => o,
                SeekFrom::End(d) => self.len.saturating_add_signed(d),
                SeekFrom::Current(d) => self.pos.saturating_add_signed(d),
            };
            Ok(self.pos)
        }
    }

    impl WavWriter<HeadSink> {
        fn sink_head(&self) -> Vec<u8> {
            self.w.get_ref().head.clone()
        }
    }

    fn pcm_fmt() -> Vec<u8> {
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1u16.to_le_bytes());
        fmt.extend_from_slice(&2u16.to_le_bytes());
        fmt.extend_from_slice(&48_000u32.to_le_bytes());
        fmt.extend_from_slice(&192_000u32.to_le_bytes());
        fmt.extend_from_slice(&4u16.to_le_bytes());
        fmt.extend_from_slice(&16u16.to_le_bytes());
        fmt
    }

    #[test]
    fn wav_writer_switches_to_rf64_beyond_4_gib() {
        let mut w = WavWriter::with_sink(HeadSink::default(), &pcm_fmt()).unwrap();
        let chunk = vec![0u8; 1 << 20];
        for _ in 0..3 * 1024 {
            w.write(&chunk).unwrap();
        }
        w.checkpoint().unwrap();
        let info = crate::wav::parse(&w.sink_head(), 0).unwrap();
        assert!(!info.rf64, "3 GiB still fits RIFF");
        assert_eq!(info.data_len, 3 << 30);
        for _ in 0..2 * 1024 {
            w.write(&chunk).unwrap();
        }
        w.checkpoint().unwrap();
        let info = crate::wav::parse(&w.sink_head(), 0).unwrap();
        assert!(info.rf64, "5 GiB must be RF64");
        assert_eq!(info.data_len, 5 << 30);
        assert_eq!(info.frames(), (5u64 << 30) / 4);
        let sink = w.finish().unwrap();
        let info = crate::wav::parse(&sink.head, sink.len).unwrap();
        assert!(info.rf64 && info.data_len == 5 << 30);
        assert_eq!(info.riff_len, sink.len - 8);
    }
}
