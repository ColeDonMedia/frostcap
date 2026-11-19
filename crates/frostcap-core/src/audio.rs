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
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
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

/// Minimal streaming RIFF/WAVE writer.
pub(crate) struct WavWriter {
    w: BufWriter<File>,
    data_bytes: u64,
    data_size_pos: u64,
}

impl WavWriter {
    pub(crate) fn create(path: &Path, fmt_chunk: &[u8]) -> std::io::Result<Self> {
        let mut w = BufWriter::with_capacity(256 * 1024, File::create(path)?);
        w.write_all(b"RIFF")?;
        w.write_all(&0u32.to_le_bytes())?; // patched in checkpoint/finish
        w.write_all(b"WAVE")?;
        w.write_all(b"fmt ")?;
        w.write_all(&(fmt_chunk.len() as u32).to_le_bytes())?;
        w.write_all(fmt_chunk)?;
        if fmt_chunk.len() % 2 == 1 {
            w.write_all(&[0])?;
        }
        w.write_all(b"data")?;
        let data_size_pos = w.stream_position()?;
        w.write_all(&0u32.to_le_bytes())?;
        Ok(Self {
            w,
            data_bytes: 0,
            data_size_pos,
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
    fn patch_sizes(&mut self, pad: bool) -> std::io::Result<()> {
        let end = self.w.stream_position()?;
        let data = u32::try_from(self.data_bytes).unwrap_or(u32::MAX);
        let riff = u32::try_from(end + u64::from(pad) - 8).unwrap_or(u32::MAX);
        self.w.seek(SeekFrom::Start(4))?;
        self.w.write_all(&riff.to_le_bytes())?;
        self.w.seek(SeekFrom::Start(self.data_size_pos))?;
        self.w.write_all(&data.to_le_bytes())?;
        self.w.seek(SeekFrom::Start(end))?;
        Ok(())
    }
    /// Flush and patch the header so the file is valid if the process dies.
    pub(crate) fn checkpoint(&mut self) -> std::io::Result<()> {
        self.patch_sizes(false)?;
        self.w.flush()
    }
    pub(crate) fn finish(mut self) -> std::io::Result<()> {
        let pad = self.data_bytes % 2 == 1;
        if pad {
            self.w.write_all(&[0])?;
        }
        self.patch_sizes(false)?;
        self.w.flush()?;
        self.w.get_ref().sync_all()
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
                let ts = if qpc > 0 {
                    qpc as i64
                } else {
                    clock::now_hns()
                };
                let write = (|| -> Result<()> {
                    let Some(pos) = timeline.map(ts) else {
                        return Ok(());
                    };
                    start_pos.get_or_insert(0);
                    let pad = pad_frames(pos, rate, frames, slack);
                    if pad > 0 {
                        wav.silence(pad as usize * block).map_err(io)?;
                        padded += pad;
                        frames += pad;
                    }
                    let bytes = n as usize * block;
                    if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                        wav.silence(bytes).map_err(io)?;
                    } else {
                        // SAFETY: WASAPI guarantees `n * block_align` readable bytes.
                        wav.write(unsafe { std::slice::from_raw_parts(data, bytes) })
                            .map_err(io)?;
                    }
                    frames += u64::from(n);
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
    let finished = wav.finish().map_err(io);
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
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 4);
        w.silence(6).unwrap();
        w.finish().unwrap();
        let b = std::fs::read(&path).unwrap();
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize,
            b.len() - 8
        );
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 10);
        let _ = std::fs::remove_file(&path);
    }
}
