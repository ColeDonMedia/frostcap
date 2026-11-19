//! Screen capture: Windows Graphics Capture (free-threaded frame pool) feeding the
//! Media Foundation H.264 writer on the GPU.
//!
//! A dedicated owner thread (MTA) creates the D3D11 device (BGRA + video support,
//! multithread-protected), the capture item, the frame pool and the writer, then
//! waits for the stop request. Frames arrive on WGC worker threads: each frame is
//! copied (and cropped for region capture) into a fresh texture which is handed to
//! the sink writer, stamped on the [`Timeline`]. On stop the last frame is written
//! once more at the stop position ("hold frame") so the MP4 duration matches the
//! recording duration even if the screen was static.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Multithread, ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{IInspectable, Interface};

use crate::clock;
use crate::error::{Error, Result};
use crate::mf::{H264Writer, WriterConfig, WriterInput};
use crate::project::Rect;
use crate::timeline::Timeline;

/// What WGC should capture.
#[derive(Clone, Copy, Debug)]
pub enum ScreenTarget {
    /// Monitor handle.
    Monitor(usize),
    /// Window handle.
    Window(usize),
}

/// Screen capture settings.
#[derive(Clone, Debug)]
pub struct ScreenConfig {
    /// Capture target.
    pub target: ScreenTarget,
    /// Output file.
    pub path: PathBuf,
    /// Crop rectangle in source pixels (region capture); output size = crop size.
    pub crop: Option<Rect>,
    /// Output size override (even). None = locked to the item size at start
    /// (or the crop size for regions).
    pub size: Option<(u32, u32)>,
    /// Max fps.
    pub fps: u32,
    /// Show the system cursor in the video.
    pub cursor: bool,
}

/// Result of a finished screen capture.
#[derive(Debug)]
pub struct ScreenResult {
    /// Frames delivered by WGC and written.
    pub frames: u64,
    /// Samples written (frames + hold frame).
    pub samples: u64,
    /// MP4 duration on the output timeline (hns).
    pub duration_hns: i64,
    /// Non-fatal problems.
    pub warnings: Vec<String>,
}

/// Startup information (encoder size + diagnostics timings in ms).
#[derive(Clone, Copy, Debug, Default)]
pub struct StartTimings {
    /// Locked encoder width.
    pub width: u32,
    /// Locked encoder height.
    pub height: u32,
    /// D3D device + WGC item + frame pool.
    pub capture_setup_ms: u64,
    /// Sink writer + encoder MFT load.
    pub encoder_setup_ms: u64,
}

struct Shared {
    writer: Option<H264Writer>,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    last_tex: Option<ID3D11Texture2D>,
    pool_size: SizeInt32,
    crop: Option<Rect>,
    width: u32,
    height: u32,
    min_interval: i64,
    error: Option<Error>,
    warnings: Vec<String>,
}

// SAFETY: D3D11 objects here are used under the Mutex only, and the device is
// multithread-protected (ID3D11Multithread); H264Writer is MTA free-threaded.
unsafe impl Send for Shared {}

/// Moves the WinRT D3D device into a WGC callback.
struct SendDevice(IDirect3DDevice);
// SAFETY: WinRT IDirect3DDevice objects are agile (free-threaded).
unsafe impl Send for SendDevice {}
// SAFETY: as above.
unsafe impl Sync for SendDevice {}
impl SendDevice {
    fn get(&self) -> &IDirect3DDevice {
        &self.0
    }
}

/// Running screen capture (owner thread handle).
pub struct ScreenCapture {
    stop_tx: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<Result<ScreenResult>>>,
    frames: Arc<AtomicU64>,
    closed: Arc<AtomicBool>,
    fault: Arc<Mutex<Option<Error>>>,
    timings: StartTimings,
}

pub(crate) fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    create_device_with(true)
}

/// D3D11 device; `video` adds `D3D11_CREATE_DEVICE_VIDEO_SUPPORT` (needed by the encoder).
pub(crate) fn create_device_with(video: bool) -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device = None;
    let mut context = None;
    // SAFETY: standard device creation with valid out params.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            if video {
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT
            } else {
                D3D11_CREATE_DEVICE_BGRA_SUPPORT
            },
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let device = device.ok_or_else(|| Error::Capture("no D3D11 device".into()))?;
    let context = context.ok_or_else(|| Error::Capture("no D3D11 context".into()))?;
    // MF and WGC worker threads share the immediate context.
    if let Ok(mt) = context.cast::<ID3D11Multithread>() {
        // SAFETY: plain setter.
        let _ = unsafe { mt.SetMultithreadProtected(true) };
    }
    Ok((device, context))
}

pub(crate) fn create_item(target: ScreenTarget) -> Result<GraphicsCaptureItem> {
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    // SAFETY: handles come from enumeration / validation in `sources`.
    let item = unsafe {
        match target {
            ScreenTarget::Monitor(h) => interop.CreateForMonitor(HMONITOR(h as *mut _))?,
            ScreenTarget::Window(h) => interop.CreateForWindow(HWND(h as *mut _))?,
        }
    };
    Ok(item)
}

fn new_texture(device: &ID3D11Device, w: u32, h: u32) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex = None;
    // SAFETY: valid descriptor and out param.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }?;
    tex.ok_or_else(|| Error::Capture("CreateTexture2D".into()))
}

/// Repeat the last frame after this long without a new one.
const KEEPALIVE: i64 = 500 * clock::HNS_PER_MS;

/// Whether a static screen needs its last frame written again at `pos`.
fn keepalive_due(last_written: Option<i64>, pos: i64) -> bool {
    last_written.is_some_and(|t| pos - t >= KEEPALIVE)
}

/// Source box for the copy: crop (or full frame) intersected with the content.
pub(crate) fn source_box(
    crop: Option<Rect>,
    content_w: i32,
    content_h: i32,
    out_w: u32,
    out_h: u32,
) -> Option<D3D11_BOX> {
    let r = crop.unwrap_or(Rect {
        x: 0,
        y: 0,
        width: out_w as i32,
        height: out_h as i32,
    });
    let left = r.x.clamp(0, content_w);
    let top = r.y.clamp(0, content_h);
    let right = (r.x + (out_w as i32).min(r.width)).clamp(0, content_w);
    let bottom = (r.y + (out_h as i32).min(r.height)).clamp(0, content_h);
    (right > left && bottom > top).then_some(D3D11_BOX {
        left: left as u32,
        top: top as u32,
        front: 0,
        right: right as u32,
        bottom: bottom as u32,
        back: 1,
    })
}

impl Shared {
    fn on_frame(&mut self, tex: &ID3D11Texture2D, content: SizeInt32, pos: i64) -> Result<()> {
        if let Some(last) = self.writer.as_ref().and_then(H264Writer::last_time)
            && pos - last < self.min_interval * 9 / 10
        {
            return Ok(()); // fps cap when MinUpdateInterval is unsupported
        }
        let out = new_texture(&self.device, self.width, self.height)?;
        let Some(b) = source_box(
            self.crop,
            content.Width,
            content.Height,
            self.width,
            self.height,
        ) else {
            return Ok(());
        };
        let (dst_x, dst_y) = match self.crop {
            Some(c) => (
                (b.left as i32 - c.x).max(0) as u32,
                (b.top as i32 - c.y).max(0) as u32,
            ),
            None => (0, 0),
        };
        // SAFETY: copying between textures of the same device/format.
        unsafe {
            if b.right - b.left < self.width || b.bottom - b.top < self.height {
                let mut rtv: Option<ID3D11RenderTargetView> = None;
                self.device
                    .CreateRenderTargetView(&out, None, Some(&mut rtv))?;
                if let Some(rtv) = rtv {
                    self.context
                        .ClearRenderTargetView(&rtv, &[0.0, 0.0, 0.0, 1.0]);
                }
            }
            self.context
                .CopySubresourceRegion(&out, 0, dst_x, dst_y, 0, tex, 0, Some(&b));
        }
        if let Some(w) = self.writer.as_mut() {
            let dur = w.config().frame_hns();
            w.write_texture(&out, pos, dur)?;
        }
        self.last_tex = Some(out);
        Ok(())
    }

    /// Write the hold frame and finalize. Returns (samples, duration_hns).
    fn finish(&mut self, end_pos: Option<i64>) -> Result<(u64, i64)> {
        let Some(mut w) = self.writer.take() else {
            return Err(Error::Capture("encoder already closed".into()));
        };
        let dur = w.config().frame_hns();
        if let (Some(end), Some(last), Some(tex)) = (end_pos, w.last_time(), self.last_tex.as_ref())
            && end - dur > last
        {
            w.write_texture(tex, end - dur, dur)?;
        }
        let duration = w.last_time().map_or(0, |t| t + dur);
        let samples = w.samples();
        w.finish()?;
        Ok((samples, duration))
    }
}

impl ScreenCapture {
    /// Start capturing. Returns once the encoder is ready and capture has started.
    pub fn start(cfg: ScreenConfig, timeline: Arc<Timeline>) -> Result<Self> {
        crate::com::ensure_mta();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<StartTimings>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let frames = Arc::new(AtomicU64::new(0));
        let closed = Arc::new(AtomicBool::new(false));
        let fault: Arc<Mutex<Option<Error>>> = Arc::default();
        let handle = thread::Builder::new()
            .name("frostcap-screen".into())
            .spawn({
                let (frames, closed, fault) = (frames.clone(), closed.clone(), fault.clone());
                move || owner(cfg, timeline, &frames, &closed, &fault, &ready_tx, &stop_rx)
            })
            .map_err(|e| Error::Capture(e.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(timings)) => Ok(Self {
                stop_tx: Some(stop_tx),
                handle: Some(handle),
                frames,
                closed,
                fault,
                timings,
            }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => Err(match handle.join() {
                Ok(Err(e)) => e,
                _ => Error::Capture("screen thread exited during startup".into()),
            }),
        }
    }

    /// Frames written so far.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// The captured window was closed.
    pub fn source_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// A fatal frame/encoder error happened (recording should be stopped).
    pub fn fault(&self) -> Option<String> {
        self.fault
            .lock()
            .ok()
            .and_then(|f| f.as_ref().map(ToString::to_string))
    }

    /// Code of the fault, if any.
    pub fn fault_code(&self) -> Option<crate::error::ErrorCode> {
        self.fault
            .lock()
            .ok()
            .and_then(|f| f.as_ref().map(Error::code))
    }

    /// Startup info.
    pub fn timings(&self) -> StartTimings {
        self.timings
    }

    /// Stop, write the hold frame and finalize the MP4. Idempotent: a second call errors.
    pub fn stop(&mut self) -> Result<ScreenResult> {
        let Some(tx) = self.stop_tx.take() else {
            return Err(Error::Invalid("screen capture already stopped".into()));
        };
        let _ = tx.send(());
        match self.handle.take().map(JoinHandle::join) {
            Some(Ok(r)) => r,
            Some(Err(_)) => Err(Error::Capture("screen thread panicked".into())),
            None => Err(Error::Invalid("screen capture already stopped".into())),
        }
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        if self.stop_tx.is_some() {
            let _ = self.stop();
        }
    }
}

fn owner(
    cfg: ScreenConfig,
    timeline: Arc<Timeline>,
    frames: &Arc<AtomicU64>,
    closed: &Arc<AtomicBool>,
    fault: &Arc<Mutex<Option<Error>>>,
    ready: &mpsc::Sender<Result<StartTimings>>,
    stop_rx: &mpsc::Receiver<()>,
) -> Result<ScreenResult> {
    // SAFETY: balanced below.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let res = run_owner(cfg, timeline, frames, closed, fault, ready, stop_rx);
    if com {
        // SAFETY: paired with the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    res
}

fn run_owner(
    cfg: ScreenConfig,
    timeline: Arc<Timeline>,
    frames: &Arc<AtomicU64>,
    closed: &Arc<AtomicBool>,
    fault: &Arc<Mutex<Option<Error>>>,
    ready: &mpsc::Sender<Result<StartTimings>>,
    stop_rx: &mpsc::Receiver<()>,
) -> Result<ScreenResult> {
    let setup = (|| -> Result<_> {
        let t = Instant::now();
        let (device, context) = create_device()?;
        let item = create_item(cfg.target)?;
        let dxgi: IDXGIDevice = device.cast()?;
        // SAFETY: valid DXGI device.
        let inspectable: IInspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }?;
        let rt_device: IDirect3DDevice = inspectable.cast()?;
        let size = item.Size()?;
        let (width, height) = match (cfg.size, cfg.crop) {
            (Some(s), _) => s,
            (None, Some(c)) => (c.width.max(2) as u32 & !1, c.height.max(2) as u32 & !1),
            (None, None) => (
                size.Width.max(2) as u32 & !1,
                size.Height.max(2) as u32 & !1,
            ),
        };
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &rt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        let _ = session.SetIsCursorCaptureEnabled(cfg.cursor);
        // Keep the yellow capture border on: a visible sign that the screen is recorded.
        let min_interval = clock::HNS_PER_SEC / i64::from(cfg.fps.max(1));
        let _ = session.SetMinUpdateInterval(windows::Foundation::TimeSpan {
            Duration: min_interval,
        });
        let capture_setup_ms = t.elapsed().as_millis() as u64;

        let t = Instant::now();
        let writer = H264Writer::create(
            &cfg.path,
            WriterConfig::with_auto_bitrate(width, height, cfg.fps),
            WriterInput::D3D11Bgra(&device),
        )?;
        let encoder_setup_ms = t.elapsed().as_millis() as u64;

        let shared = Arc::new(Mutex::new(Shared {
            writer: Some(writer),
            device: device.clone(),
            context,
            last_tex: None,
            pool_size: size,
            crop: cfg.crop,
            width,
            height,
            min_interval,
            error: None,
            warnings: Vec::new(),
        }));

        let token = pool.FrameArrived(&TypedEventHandler::<
            Direct3D11CaptureFramePool,
            IInspectable,
        >::new({
            let (shared, timeline, frames, rt_device, fault) = (
                shared.clone(),
                timeline.clone(),
                frames.clone(),
                SendDevice(rt_device.clone()),
                fault.clone(),
            );
            move |pool, _| {
                let Some(pool) = pool.as_ref() else {
                    return Ok(());
                };
                let frame = pool.TryGetNextFrame()?;
                let ts = frame.SystemRelativeTime()?.Duration;
                let content = frame.ContentSize()?;
                timeline.anchor(ts);
                let mut s = shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if s.writer.is_some()
                    && s.error.is_none()
                    && let Some(pos) = timeline.map(ts)
                {
                    let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
                    // SAFETY: WGC surfaces are DXGI-backed D3D11 textures.
                    let tex: ID3D11Texture2D = unsafe { access.GetInterface() }?;
                    match s.on_frame(&tex, content, pos) {
                        Ok(()) => {
                            frames.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            if let Ok(mut f) = fault.lock() {
                                *f = Some(Error::Coded(e.to_payload()));
                            }
                            s.error = Some(e);
                        }
                    }
                }
                if content.Width != s.pool_size.Width || content.Height != s.pool_size.Height {
                    s.pool_size = content;
                    pool.Recreate(
                        rt_device.get(),
                        DirectXPixelFormat::B8G8R8A8UIntNormalized,
                        2,
                        content,
                    )?;
                }
                drop(s);
                frame.Close()?;
                Ok(())
            }
        }))?;
        let closed_token = item.Closed(
            &TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new({
                let closed = closed.clone();
                move |_, _| {
                    closed.store(true, Ordering::Relaxed);
                    Ok(())
                }
            }),
        )?;
        session.StartCapture()?;
        Ok((
            item,
            pool,
            session,
            shared,
            token,
            closed_token,
            StartTimings {
                width,
                height,
                capture_setup_ms,
                encoder_setup_ms,
            },
        ))
    })();

    let (item, pool, session, shared, token, closed_token, timings) = match setup {
        Ok(v) => {
            let _ = ready.send(Ok(v.6));
            v
        }
        Err(e) => {
            let _ = ready.send(Err(Error::Coded(e.to_payload())));
            let _ = std::fs::remove_file(&cfg.path);
            return Err(e);
        }
    };
    let _ = timings;

    // Wait for stop (or the session owner being dropped). Meanwhile repeat the
    // last frame on a static screen so fragments keep reaching the disk (a crash
    // loses at most ~KEEPALIVE of video).
    while let Err(mpsc::RecvTimeoutError::Timeout) =
        stop_rx.recv_timeout(Duration::from_millis(250))
    {
        let Some(pos) = timeline.map(clock::now_hns()) else {
            continue;
        };
        let mut s = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.error.is_none()
            && let Some(tex) = s.last_tex.clone()
            && let Some(w) = s.writer.as_mut()
            && keepalive_due(w.last_time(), pos)
        {
            let dur = w.config().frame_hns();
            if let Err(e) = w.write_texture(&tex, pos, dur) {
                if let Ok(mut f) = fault.lock() {
                    *f = Some(Error::Coded(e.to_payload()));
                }
                s.error = Some(e);
            }
        }
    }
    let stop_now = clock::now_hns();

    // Stop frames first, then finalize with the hold frame.
    let _ = pool.RemoveFrameArrived(token);
    let _ = item.RemoveClosed(closed_token);
    let _ = session.Close();
    // Give an in-flight callback a moment to release the lock.
    thread::sleep(Duration::from_millis(10));
    let _ = pool.Close();

    let mut s = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut warnings = std::mem::take(&mut s.warnings);
    if closed.load(Ordering::Relaxed) {
        warnings.push("the captured window was closed during recording".into());
    }
    let frame_error = s.error.take();
    let finish = s.finish(timeline.position(stop_now));
    drop(s);
    if let Some(e) = frame_error {
        // Keep what was written if possible, but report the failure.
        warnings.push(format!("recording stopped early: {e}"));
        if finish.is_err() {
            return Err(e);
        }
    }
    let (samples, duration_hns) = finish?;
    Ok(ScreenResult {
        frames: frames.load(Ordering::Relaxed),
        samples,
        duration_hns,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keepalive_repeats_a_static_screen() {
        let ms = clock::HNS_PER_MS;
        assert!(!keepalive_due(None, 5_000 * ms), "nothing to repeat yet");
        assert!(!keepalive_due(Some(4_800 * ms), 5_000 * ms));
        assert!(keepalive_due(Some(4_000 * ms), 5_000 * ms));
    }

    #[test]
    fn source_box_clamps_crop_to_content() {
        let b = source_box(
            Some(Rect {
                x: 100,
                y: 50,
                width: 400,
                height: 300,
            }),
            300,
            1000,
            400,
            300,
        )
        .unwrap();
        assert_eq!((b.left, b.top, b.right, b.bottom), (100, 50, 300, 350));
        assert!(
            source_box(
                Some(Rect {
                    x: 500,
                    y: 0,
                    width: 10,
                    height: 10
                }),
                300,
                300,
                10,
                10
            )
            .is_none()
        );
        let b = source_box(None, 1920, 1080, 1920, 1080).unwrap();
        assert_eq!((b.right, b.bottom), (1920, 1080));
    }

    /// Regression: thumbnails used to CoUninitialize the MTA, leaving windows-rs'
    /// cached WinRT factories dangling, so the next capture crashed (0xC0000005).
    #[test]
    #[ignore = "needs interactive desktop"]
    fn capture_after_thumbnails_and_hold_frame() {
        let m = windows_capture::monitor::Monitor::primary().unwrap();
        let target = ScreenTarget::Monitor(m.as_raw_hmonitor() as usize);
        let _ = crate::thumb::capture_thumbnails(&[Some(target)], 64);
        let path = crate::test_tmp_dir().join(format!("screen-{}.mp4", std::process::id()));
        let tl = Arc::new(Timeline::default());
        let mut cap = ScreenCapture::start(
            ScreenConfig {
                target,
                path: path.clone(),
                crop: None,
                size: None,
                fps: 30,
                cursor: false,
            },
            tl,
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(1200));
        let r = cap.stop().unwrap();
        assert!(r.samples >= 1);
        // Hold frame: the MP4 must cover (almost) the whole recording even if static.
        assert!(
            r.duration_hns >= 900 * clock::HNS_PER_MS,
            "duration {} hns",
            r.duration_hns
        );
        assert!(cap.stop().is_err());
        let _ = std::fs::remove_file(&path);
    }
}
