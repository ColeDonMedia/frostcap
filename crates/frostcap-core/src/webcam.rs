//! Webcam capture: Media Foundation device enumeration + source reader (NV12 via
//! the reader's video processor) into the shared [`H264Writer`].
//!
//! Timestamps: device sample times are converted to the shared QPC clock by
//! anchoring the first sample to its arrival time (if the device already reports
//! QPC-based times within 1 s of "now", they are used directly), then mapped
//! through the recording [`Timeline`] (pre-t0 and paused samples are dropped).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use serde::{Deserialize, Serialize};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, IMFMediaSource, IMFMediaType, IMFSample, IMFSourceReader,
    MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR, MFCreateAttributes, MFCreateMediaType,
    MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFMediaType_Video,
    MFVideoFormat_NV12,
};
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::core::PWSTR;

use crate::clock;
use crate::error::{Error, Result};
use crate::mf::{H264Writer, MfRuntime, WriterConfig, WriterInput};
use crate::timeline::Timeline;

const FIRST_VIDEO_STREAM: u32 = 0xFFFF_FFFC;

/// A video capture device.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CameraInfo {
    /// Stable id (symbolic link). Pass back as `camera` in `start_recording`.
    pub id: String,
    /// Friendly name.
    pub name: String,
}

struct ComMta(bool);
impl ComMta {
    fn new() -> Self {
        // SAFETY: balanced in Drop when it succeeded.
        Self(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}
impl Drop for ComMta {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: paired with a successful CoInitializeEx.
            unsafe { CoUninitialize() };
        }
    }
}

fn attr_string(a: &IMFActivate, key: &windows::core::GUID) -> Option<String> {
    let mut p = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: MF allocates the string; we free it with CoTaskMemFree.
    unsafe {
        a.GetAllocatedString(key, &mut p, &mut len).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

fn enum_devices() -> Result<Vec<IMFActivate>> {
    // SAFETY: standard MFEnumDeviceSources usage; the array is freed after copying.
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.ok_or_else(|| Error::Capture("MFCreateAttributes".into()))?;
        attrs.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )?;
        let mut arr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFEnumDeviceSources(&attrs, &mut arr, &mut count)?;
        let mut out = Vec::with_capacity(count as usize);
        if !arr.is_null() {
            for i in 0..count as usize {
                if let Some(a) = (*arr.add(i)).take() {
                    out.push(a);
                }
            }
            CoTaskMemFree(Some(arr as *const _));
        }
        Ok(out)
    }
}

/// List video capture devices. An empty list means no camera.
pub fn list_cameras() -> Result<Vec<CameraInfo>> {
    crate::com::ensure_mta();
    let _com = ComMta::new();
    let _mf = MfRuntime::new()?;
    Ok(enum_devices()?
        .iter()
        .filter_map(|a| {
            Some(CameraInfo {
                id: attr_string(a, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK)?,
                name: attr_string(a, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)
                    .unwrap_or_else(|| "Camera".into()),
            })
        })
        .collect())
}

fn size_of_type(t: &IMFMediaType) -> Option<(u32, u32, f64)> {
    // SAFETY: attribute getters.
    unsafe {
        let s = t.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
        let r = t
            .GetUINT64(&MF_MT_FRAME_RATE)
            .ok()
            .unwrap_or(30u64 << 32 | 1);
        let (num, den) = ((r >> 32) as u32, (r & 0xFFFF_FFFF) as u32);
        Some((
            (s >> 32) as u32,
            (s & 0xFFFF_FFFF) as u32,
            f64::from(num) / f64::from(den.max(1)),
        ))
    }
}

/// Pick the best native mode: <= 1920x1080, >= 24 fps preferred, largest area.
fn choose_mode(reader: &IMFSourceReader) -> Option<(u32, u32, u32)> {
    let mut best: Option<(u32, u32, f64)> = None;
    for i in 0..256 {
        // SAFETY: enumerating native types until it fails.
        let Ok(t) = (unsafe { reader.GetNativeMediaType(FIRST_VIDEO_STREAM, i) }) else {
            break;
        };
        let Some((w, h, fps)) = size_of_type(&t) else {
            continue;
        };
        if w > 1920 || h > 1080 || w < 16 || h < 16 {
            continue;
        }
        let score = |(w, h, fps): (u32, u32, f64)| {
            (
                fps >= 24.0,
                u64::from(w) * u64::from(h),
                (fps * 100.0) as u64,
            )
        };
        if best.is_none_or(|b| score((w, h, fps)) > score(b)) {
            best = Some((w, h, fps));
        }
    }
    best.map(|(w, h, fps)| (w & !1, h & !1, fps.round().clamp(1.0, 60.0) as u32))
}

/// Result of a webcam recording.
#[derive(Debug)]
pub struct WebcamResult {
    /// Output file.
    pub path: PathBuf,
    /// Camera name.
    pub name: String,
    /// Frame size.
    pub width: u32,
    /// Frame size.
    pub height: u32,
    /// Frames written.
    pub frames: u64,
    /// Duration of the MP4 on the output timeline (hns).
    pub duration_hns: i64,
    /// Output-timeline position of the first frame (hns).
    pub first_hns: i64,
    /// Why capture ended early, if it did (the partial file is kept).
    pub error: Option<crate::error::ErrorPayload>,
}

/// Running webcam capture.
pub struct WebcamRecorder {
    stop: Arc<AtomicBool>,
    done: Option<mpsc::Receiver<Result<WebcamResult>>>,
    finished: Arc<AtomicBool>,
    frames: Arc<AtomicU64>,
    shutdown: Arc<SourceShutdown>,
}

/// How long `stop` waits for the camera thread before giving up on it.
const STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);
/// Time a delivering camera gets to return its next frame before we interrupt it.
const STOP_GRACE: std::time::Duration = std::time::Duration::from_millis(1500);

impl WebcamRecorder {
    /// Open the camera (`id` from [`list_cameras`], `None` = first) and start writing.
    /// Fails cleanly (no thread left running) if there is no camera. With
    /// `wait == false` it returns as soon as the device is known to exist; camera
    /// activation (which can take seconds) then continues in the background and a
    /// later failure shows up via [`Self::has_failed`] / [`Self::stop`].
    pub fn start(
        id: Option<&str>,
        path: &Path,
        timeline: Arc<Timeline>,
        wait: bool,
    ) -> Result<Self> {
        crate::com::ensure_mta();
        let cams = list_cameras()?;
        if cams.is_empty() || id.is_some_and(|id| !cams.iter().any(|c| c.id == id)) {
            return Err(Error::SourceNotFound(id.map_or_else(
                || "no camera found".to_owned(),
                |i| format!("camera {i}"),
            )));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let frames = Arc::new(AtomicU64::new(0));
        let shutdown = Arc::new(SourceShutdown(Mutex::new(None)));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let (done_tx, done_rx) = mpsc::channel::<Result<WebcamResult>>();
        let id = id.map(str::to_owned);
        let path = path.to_path_buf();
        thread::Builder::new()
            .name("frostcap-webcam".into())
            .spawn({
                let (stop, frames, finished, shutdown) = (
                    stop.clone(),
                    frames.clone(),
                    finished.clone(),
                    shutdown.clone(),
                );
                move || {
                    let r = run(
                        id.as_deref(),
                        &path,
                        &timeline,
                        &stop,
                        &frames,
                        &ready_tx,
                        &shutdown,
                    );
                    finished.store(true, Ordering::Release);
                    let _ = done_tx.send(r);
                }
            })
            .map_err(|e| Error::Capture(e.to_string()))?;
        let me = Self {
            stop,
            done: Some(done_rx),
            finished,
            frames,
            shutdown,
        };
        if !wait {
            return Ok(me);
        }
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(me),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(Error::Capture("webcam thread exited during startup".into())),
        }
    }

    /// Frames written so far.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// The capture thread ended on its own (camera unplugged, write error).
    pub fn has_failed(&self) -> bool {
        self.finished.load(Ordering::Acquire) && !self.stop.load(Ordering::Acquire)
    }

    /// Stop and finalize. Never hangs: a camera that does not respond within
    /// 5 s is abandoned (its thread is left to finish on its own).
    pub fn stop(&mut self) -> Result<WebcamResult> {
        let Some(done) = self.done.take() else {
            return Err(Error::Invalid("webcam already stopped".into()));
        };
        stop_worker(&self.stop, &*self.shutdown, &done, STOP_GRACE, STOP_TIMEOUT).unwrap_or_else(
            || {
                Err(Error::Capture(
                    "the camera did not stop in time; webcam.mp4 may be incomplete".into(),
                ))
            },
        )
    }
}

impl Drop for WebcamRecorder {
    fn drop(&mut self) {
        if self.done.is_some() {
            let _ = self.stop();
        }
    }
}
fn open(id: Option<&str>) -> Result<(IMFSourceReader, IMFMediaSource, String, u32, u32, u32)> {
    let devices = enum_devices()?;
    let activate = match id {
        Some(id) => devices
            .into_iter()
            .find(|a| {
                attr_string(a, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK).as_deref()
                    == Some(id)
            })
            .ok_or_else(|| Error::SourceNotFound(format!("camera {id}")))?,
        None => devices
            .into_iter()
            .next()
            .ok_or_else(|| Error::SourceNotFound("no camera found".into()))?,
    };
    let name = attr_string(&activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)
        .unwrap_or_else(|| "Camera".into());
    // SAFETY: standard source reader setup.
    unsafe {
        let source: IMFMediaSource = activate.ActivateObject().map_err(|e| {
            Error::Capture(format!(
                "camera {name} unavailable (in use or blocked by privacy settings?): {e}"
            ))
        })?;
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.ok_or_else(|| Error::Capture("MFCreateAttributes".into()))?;
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
        let reader = MFCreateSourceReaderFromMediaSource(&source, &attrs)?;
        let (w, h, fps) = choose_mode(&reader)
            .ok_or_else(|| Error::Capture(format!("{name}: no usable video mode")))?;
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(w) << 32) | u64::from(h))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps) << 32) | 1)?;
        reader.SetCurrentMediaType(FIRST_VIDEO_STREAM, None, &t)?;
        Ok((reader, source, name, w, h, fps))
    }
}

/// Result of one blocking read.
pub(crate) enum ReadOutcome<S> {
    /// A frame and its device timestamp (hns).
    Sample(S, i64),
    /// Nothing usable this time (gap, format change).
    Skip,
    /// The stream ended or failed (also returned after an interrupt).
    End(Error),
}

/// A blocking frame source (the MF source reader, or a fake in tests).
pub(crate) trait FrameReader {
    type Sample;
    fn read(&mut self) -> ReadOutcome<Self::Sample>;
}

/// Unblocks a pending [`FrameReader::read`] from another thread.
pub(crate) trait Interrupt: Send + Sync {
    fn interrupt(&self);
}

/// Read until `stop` or an error. A read that ends *because* of a stop request
/// is a clean end. Returns the error that ended the loop, if any.
pub(crate) fn capture_loop<R: FrameReader>(
    reader: &mut R,
    stop: &AtomicBool,
    mut on_sample: impl FnMut(R::Sample, i64) -> Result<()>,
) -> Option<Error> {
    while !stop.load(Ordering::Acquire) {
        match reader.read() {
            ReadOutcome::Sample(s, ts) => {
                if let Err(e) = on_sample(s, ts) {
                    return Some(e);
                }
            }
            ReadOutcome::Skip => {}
            ReadOutcome::End(e) => return (!stop.load(Ordering::Acquire)).then_some(e),
        }
    }
    None
}

/// Stop a capture worker: raise `stop` and give a live camera `grace` to
/// deliver its next frame (the loop then exits by itself). Only a camera that
/// stalls is interrupted (MF source shutdown, which can disturb a pending
/// synchronous read), and after `timeout` the worker is abandoned.
pub(crate) fn stop_worker<T>(
    stop: &AtomicBool,
    interrupt: &dyn Interrupt,
    done: &mpsc::Receiver<T>,
    grace: std::time::Duration,
    timeout: std::time::Duration,
) -> Option<T> {
    stop.store(true, Ordering::Release);
    if let Some(r) = wait_done(done, grace) {
        return Some(r);
    }
    interrupt.interrupt();
    wait_done(done, timeout)
}

/// Wait for a worker's result, giving up after `timeout` (the thread is then
/// left detached; it can only hold the camera, never the caller).
pub(crate) fn wait_done<T>(rx: &mpsc::Receiver<T>, timeout: std::time::Duration) -> Option<T> {
    rx.recv_timeout(timeout).ok()
}

struct MfReader {
    reader: IMFSourceReader,
    name: String,
}

impl FrameReader for MfReader {
    type Sample = IMFSample;
    fn read(&mut self) -> ReadOutcome<IMFSample> {
        let mut flags = 0u32;
        let mut ts = 0i64;
        let mut sample: Option<IMFSample> = None;
        // SAFETY: synchronous read with valid out params.
        let r = unsafe {
            self.reader.ReadSample(
                FIRST_VIDEO_STREAM,
                0,
                None,
                Some(&mut flags),
                Some(&mut ts),
                Some(&mut sample),
            )
        };
        if let Err(e) = r {
            return ReadOutcome::End(e.into());
        }
        if flags & (MF_SOURCE_READERF_ERROR.0 as u32 | MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0
        {
            return ReadOutcome::End(Error::SourceNotFound(format!(
                "{}: stream ended (flags {flags:#x})",
                self.name
            )));
        }
        match sample {
            Some(s) => ReadOutcome::Sample(s, ts),
            None => ReadOutcome::Skip,
        }
    }
}

/// Shuts the media source down, which makes a blocked `ReadSample` return.
struct SourceShutdown(Mutex<Option<IMFMediaSource>>);
// SAFETY: MF media sources are free-threaded; access is serialized by the mutex.
unsafe impl Send for SourceShutdown {}
// SAFETY: as above.
unsafe impl Sync for SourceShutdown {}
impl Interrupt for SourceShutdown {
    fn interrupt(&self) {
        if let Ok(g) = self.0.lock()
            && let Some(s) = g.as_ref()
        {
            // SAFETY: Shutdown is valid at any time and idempotent.
            let _ = unsafe { s.Shutdown() };
        }
    }
}

fn run(
    id: Option<&str>,
    path: &Path,
    timeline: &Timeline,
    stop: &AtomicBool,
    frames: &AtomicU64,
    ready: &mpsc::Sender<Result<()>>,
    shutdown: &SourceShutdown,
) -> Result<WebcamResult> {
    let _com = ComMta::new();
    let started = (|| -> Result<_> {
        let mf = MfRuntime::new()?;
        let (reader, source, name, w, h, fps) = open(id)?;
        let writer = H264Writer::create(
            path,
            WriterConfig::with_auto_bitrate(w, h, fps),
            WriterInput::Nv12,
        )?;
        Ok((mf, reader, source, name, w, h, writer))
    })();
    let (_mf, reader, source, name, width, height, mut writer) = match started {
        Ok(v) => {
            let _ = ready.send(Ok(()));
            v
        }
        Err(e) => {
            let _ = std::fs::remove_file(path);
            let code = e.code();
            let _ = ready.send(Err(e));
            return Err(Error::Capture(format!("webcam failed to open ({code:?})")));
        }
    };
    if let Ok(mut g) = shutdown.0.lock() {
        *g = Some(source.clone());
    }
    let dur = writer.config().frame_hns();
    let mut offset: Option<i64> = None;
    let mut last_sample: Option<IMFSample> = None;
    let mut first_pos: Option<i64> = None;
    let mut mf_reader = MfReader {
        reader,
        name: name.clone(),
    };
    let failure = capture_loop(&mut mf_reader, stop, |sample, ts| {
        let now = clock::now_hns();
        let off = *offset.get_or_insert(if (now - ts).abs() < clock::HNS_PER_SEC {
            0
        } else {
            now - ts
        });
        let Some(pos) = timeline.map(ts + off) else {
            return Ok(());
        };
        writer.write_sample(&sample, pos, dur)?;
        frames.fetch_add(1, Ordering::Relaxed);
        first_pos.get_or_insert(pos);
        last_sample = Some(sample);
        Ok(())
    });
    // Hold frame at the stop position so durations line up with the screen track.
    if failure.is_none()
        && let (Some(end), Some(last), Some(s)) = (
            timeline.position(clock::now_hns()),
            writer.last_time(),
            last_sample.as_ref(),
        )
        && end - dur > last
    {
        // A fresh sample sharing the buffer: the queued one must not be re-stamped.
        // SAFETY: plain MF calls.
        let hold = unsafe {
            s.GetBufferByIndex(0).and_then(|b| {
                let n = windows::Win32::Media::MediaFoundation::MFCreateSample()?;
                n.AddBuffer(&b)?;
                Ok(n)
            })
        };
        if let Ok(h) = hold {
            let _ = writer.write_sample(&h, end - dur, dur);
        }
    }
    let duration_hns = writer.last_time().map_or(0, |t| t + dur);
    let finished = writer.finish();
    if let Ok(mut g) = shutdown.0.lock()
        && let Some(s) = g.take()
    {
        // SAFETY: release the camera.
        let _ = unsafe { s.Shutdown() };
    }
    if frames.load(Ordering::Relaxed) == 0 {
        finished?;
        if let Some(e) = failure {
            return Err(e);
        }
    }
    Ok(WebcamResult {
        path: path.to_path_buf(),
        name,
        width,
        height,
        frames: frames.load(Ordering::Relaxed),
        duration_hns,
        first_hns: first_pos.unwrap_or(0),
        // Partial file is kept (fragmented MP4 is playable); report why it ended.
        error: failure.map(|e| e.to_payload()),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};

    /// A camera that delivers `n` frames and then stalls forever until interrupted.
    struct StalledReader {
        left: u32,
        gate: Arc<(Mutex<bool>, Condvar)>,
    }
    struct GateInterrupt(Arc<(Mutex<bool>, Condvar)>);
    impl Interrupt for GateInterrupt {
        fn interrupt(&self) {
            let (m, c) = &*self.0;
            *m.lock().unwrap() = true;
            c.notify_all();
        }
    }
    impl FrameReader for StalledReader {
        type Sample = u32;
        fn read(&mut self) -> ReadOutcome<u32> {
            if self.left > 0 {
                self.left -= 1;
                return ReadOutcome::Sample(self.left, 0);
            }
            let (m, c) = &*self.gate;
            let mut done = m.lock().unwrap();
            while !*done {
                done = c.wait(done).unwrap();
            }
            ReadOutcome::End(Error::Capture("interrupted".into()))
        }
    }

    #[test]
    fn stop_interrupts_a_stalled_camera() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let mut reader = StalledReader {
            left: 3,
            gate: gate.clone(),
        };
        let (tx, rx) = mpsc::channel();
        let st = stop.clone();
        std::thread::spawn(move || {
            let mut got = 0;
            let err = capture_loop(&mut reader, &st, |_, _| {
                got += 1;
                Ok(())
            });
            let _ = tx.send((got, err.is_some()));
        });
        std::thread::sleep(Duration::from_millis(100));
        let t = Instant::now();
        let (got, errored) = stop_worker(
            &stop,
            &GateInterrupt(gate),
            &rx,
            Duration::from_millis(200),
            Duration::from_secs(2),
        )
        .expect("loop must end");
        assert!(t.elapsed() < Duration::from_secs(1));
        assert!(
            t.elapsed() >= Duration::from_millis(200),
            "the grace period is honoured first"
        );
        assert_eq!(got, 3);
        assert!(
            !errored,
            "an interrupt after stop is a clean end, not an error"
        );
    }

    #[test]
    fn a_live_camera_stops_without_interrupt() {
        struct Live;
        impl FrameReader for Live {
            type Sample = ();
            fn read(&mut self) -> ReadOutcome<()> {
                std::thread::sleep(Duration::from_millis(30));
                ReadOutcome::Sample((), 0)
            }
        }
        struct Panics;
        impl Interrupt for Panics {
            fn interrupt(&self) {
                panic!("must not interrupt a camera that is still delivering");
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let st = stop.clone();
        std::thread::spawn(move || {
            let _ = tx.send(capture_loop(&mut Live, &st, |_, _| Ok(())).is_none());
        });
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            stop_worker(
                &stop,
                &Panics,
                &rx,
                Duration::from_millis(500),
                Duration::from_secs(2)
            ),
            Some(true)
        );
    }

    #[test]
    fn wait_done_never_hangs_on_a_stuck_thread() {
        let (_tx, rx) = mpsc::channel::<()>();
        let t = Instant::now();
        assert!(wait_done(&rx, Duration::from_millis(200)).is_none());
        assert!(t.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn writer_errors_end_the_loop_with_an_error() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let stop = AtomicBool::new(false);
        let mut reader = StalledReader { left: 5, gate };
        let err = capture_loop(&mut reader, &stop, |_, _| {
            Err(Error::DiskFull("full".into()))
        });
        assert!(matches!(err, Some(Error::DiskFull(_))));
    }
}
