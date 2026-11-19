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
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
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
use windows::Win32::Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{IInspectable, Interface};

use crate::clock;
use crate::error::{Error, Result};
use crate::hdr_gpu::{Converter, HdrInfo};
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
    /// Every monitor, composed into one canvas (see [`ScreenConfig::canvas`]).
    AllDisplays,
}

/// One monitor of an all-displays capture.
#[derive(Clone, Copy, Debug)]
pub struct MonitorSrc {
    /// Monitor handle.
    pub handle: usize,
    /// Bounds on the virtual desktop (physical px).
    pub bounds: Rect,
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
    /// Keep the Windows capture border (a visible sign that the screen is recorded).
    pub border: bool,
    /// Monitors of an [`ScreenTarget::AllDisplays`] capture (index = [`crate::canvas::Tile::index`]).
    pub monitors: Vec<MonitorSrc>,
    /// Canvas layout of an all-displays capture.
    pub canvas: Option<crate::canvas::Layout>,
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
    /// `RecordingSession::start` before the screen thread was spawned (source
    /// resolution, disk check, provisional project.json).
    pub prep_ms: u64,
    /// Audio, webcam and input trackers starting (runs parallel to the screen setup).
    pub side_tracks_ms: u64,
    /// Time spent waiting for the screen pipeline after the side tracks were ready.
    pub screen_wait_ms: u64,
}

/// One captured monitor or window.
#[derive(Clone)]
struct Src {
    /// Where its pixels land on the canvas (multi capture only).
    dst: (u32, u32),
    /// Size of its tile on the canvas (multi capture only).
    tile: (u32, u32),
    /// Monitor size in pixels (multi capture only; the tile may be a scaled-down copy).
    src_size: (u32, u32),
    hdr: HdrInfo,
    pool_size: SizeInt32,
}

impl Src {
    fn format(&self) -> DirectXPixelFormat {
        if self.hdr.hdr {
            DirectXPixelFormat::R16G16B16A16Float
        } else {
            DirectXPixelFormat::B8G8R8A8UIntNormalized
        }
    }
}

struct Shared {
    writer: Option<H264Writer>,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    last_tex: Option<ID3D11Texture2D>,
    srcs: Vec<Src>,
    /// Persistent composition target of an all-displays capture.
    canvas: Option<ID3D11Texture2D>,
    /// A source changed since the last written sample (multi capture only).
    dirty: bool,
    converter: Option<Converter>,
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

/// Process-wide capture device (BGRA + video support). Creating a D3D11 device costs
/// 0.3-1 s, so the app prewarms it once and every session reuses it. The device is
/// multithread-protected; sessions never overlap. A removed/reset device is replaced.
struct SharedDevice(ID3D11Device, ID3D11DeviceContext);
// SAFETY: free-threaded D3D11 objects, multithread protection is enabled.
unsafe impl Send for SharedDevice {}

static SHARED_DEVICE: Mutex<Option<SharedDevice>> = Mutex::new(None);

pub(crate) fn shared_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut slot = SHARED_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: plain getter.
    if let Some(d) = slot.as_ref()
        && unsafe { d.0.GetDeviceRemovedReason() }.is_ok()
    {
        return Ok((d.0.clone(), d.1.clone()));
    }
    let (device, context) = create_device()?;
    *slot = Some(SharedDevice(device.clone(), context.clone()));
    Ok((device, context))
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
            ScreenTarget::AllDisplays => {
                return Err(Error::Invalid(
                    "all displays are captured one monitor at a time".into(),
                ));
            }
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

/// Whether a frame at `pos` is too soon after `last` for the fps cap (used when
/// `MinUpdateInterval` is unsupported, and to bound all-displays composition).
fn throttled(last: Option<i64>, pos: i64, min_interval: i64) -> bool {
    last.is_some_and(|l| pos - l < min_interval * 9 / 10)
}

/// The region of a copy as a pixel rectangle.
fn box_to_rect(b: &D3D11_BOX) -> Rect {
    Rect {
        x: b.left as i32,
        y: b.top as i32,
        width: (b.right - b.left) as i32,
        height: (b.bottom - b.top) as i32,
    }
}

/// Put region `b` of `tex` at `dst_xy` of `dst`: a plain GPU copy for SDR sources,
/// the HDR -> SDR shader pass for HDR ones.
#[allow(clippy::too_many_arguments)]
pub(crate) fn blit(
    context: &ID3D11DeviceContext,
    converter: Option<&Converter>,
    tex: &ID3D11Texture2D,
    b: &D3D11_BOX,
    dst: &ID3D11Texture2D,
    dst_xy: (u32, u32),
    dst_size: Option<(u32, u32)>,
    hdr: HdrInfo,
) -> Result<()> {
    let r = box_to_rect(b);
    let size = dst_size.unwrap_or((r.width as u32, r.height as u32));
    let scaled = size != (r.width as u32, r.height as u32);
    match converter {
        Some(c) if hdr.hdr || scaled => {
            c.draw(tex, r, dst, dst_xy, size, hdr.hdr, hdr.sdr_white_nits)
        }
        // SAFETY: copying between textures of the same device and format.
        _ => {
            unsafe {
                context.CopySubresourceRegion(dst, 0, dst_xy.0, dst_xy.1, 0, tex, 0, Some(b))
            };
            Ok(())
        }
    }
}

impl Shared {
    fn clear(&self, tex: &ID3D11Texture2D) -> Result<()> {
        let mut rtv: Option<ID3D11RenderTargetView> = None;
        // SAFETY: render target view of our own texture.
        unsafe {
            self.device
                .CreateRenderTargetView(tex, None, Some(&mut rtv))?;
            if let Some(rtv) = rtv {
                self.context
                    .ClearRenderTargetView(&rtv, &[0.0, 0.0, 0.0, 1.0]);
            }
        }
        Ok(())
    }

    fn write(&mut self, out: ID3D11Texture2D, pos: i64) -> Result<()> {
        if let Some(w) = self.writer.as_mut() {
            let dur = w.config().frame_hns();
            w.write_texture(&out, pos, dur)?;
        }
        self.last_tex = Some(out);
        self.dirty = false;
        Ok(())
    }

    /// Write a snapshot of the composed canvas (multi capture).
    fn snapshot(&mut self, pos: i64, force: bool) -> Result<()> {
        let Some(canvas) = self.canvas.clone() else {
            return Ok(());
        };
        let last = self.writer.as_ref().and_then(H264Writer::last_time);
        if !force && throttled(last, pos, self.min_interval) {
            return Ok(());
        }
        let out = new_texture(&self.device, self.width, self.height)?;
        // SAFETY: same device, same size and format.
        unsafe { self.context.CopyResource(&out, &canvas) };
        self.write(out, pos)
    }

    /// Handle a frame of source `idx`.
    fn on_frame(
        &mut self,
        idx: usize,
        tex: &ID3D11Texture2D,
        content: SizeInt32,
        pos: i64,
    ) -> Result<()> {
        let src = self.srcs[idx].clone();
        if let Some(canvas) = self.canvas.clone() {
            // All displays: update this monitor's tile, then emit a snapshot (throttled).
            let Some(b) = source_box(
                None,
                content.Width,
                content.Height,
                src.src_size.0,
                src.src_size.1,
            ) else {
                return Ok(());
            };
            blit(
                &self.context,
                self.converter.as_ref(),
                tex,
                &b,
                &canvas,
                src.dst,
                Some(src.tile),
                src.hdr,
            )?;
            self.dirty = true;
            return self.snapshot(pos, false);
        }
        let last = self.writer.as_ref().and_then(H264Writer::last_time);
        if throttled(last, pos, self.min_interval) {
            return Ok(());
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
        if b.right - b.left < self.width || b.bottom - b.top < self.height {
            self.clear(&out)?;
        }
        blit(
            &self.context,
            self.converter.as_ref(),
            tex,
            &b,
            &out,
            (dst_x, dst_y),
            None,
            src.hdr,
        )?;
        self.write(out, pos)
    }

    /// Write the hold frame and finalize. Returns (samples, duration_hns).
    fn finish(&mut self, end_pos: Option<i64>) -> Result<(u64, i64)> {
        // All displays: changes not yet written (throttled) go out before the hold frame.
        if self.dirty
            && let Some(end) = end_pos
        {
            let dur = self.writer.as_ref().map_or(0, |w| w.config().frame_hns());
            self.snapshot((end - dur).max(0), true)?;
        }
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

/// Moves a value that is safe to use from the encoder-init thread (free-threaded COM).
struct SendBox<T>(T);
// SAFETY: only used for free-threaded D3D11 objects on a multithread-protected device.
unsafe impl<T> Send for SendBox<T> {}

/// Owns a worker thread: dropping the guard (e.g. on an early `?` return) waits for
/// the thread and drops its result, so nothing keeps running or holding files after
/// the caller moved on to cleanup.
struct JoinGuard<T>(Option<JoinHandle<T>>);

impl<T> JoinGuard<T> {
    fn new(h: JoinHandle<T>) -> Self {
        Self(Some(h))
    }

    /// Wait for the thread and take its result.
    fn join(mut self) -> thread::Result<T> {
        match self.0.take() {
            Some(h) => h.join(),
            None => Err(Box::new("already joined")),
        }
    }
}

impl<T> Drop for JoinGuard<T> {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            let _ = h.join();
        }
    }
}

/// A live WGC capture of one source.
struct Live {
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    token: i64,
    closed_token: i64,
}

impl Shared {
    /// Periodic work while waiting: flush pending all-displays changes, or repeat the
    /// last frame on a static screen so fragments keep reaching the disk.
    fn tick(&mut self, pos: i64) -> Result<()> {
        if self.dirty {
            return self.snapshot(pos, false);
        }
        if let Some(tex) = self.last_tex.clone()
            && let Some(w) = self.writer.as_mut()
            && keepalive_due(w.last_time(), pos)
        {
            let dur = w.config().frame_hns();
            w.write_texture(&tex, pos, dur)?;
        }
        Ok(())
    }
}

/// `FROSTCAP_FORCE_HDR_PATH=1` runs the scRGB capture + tone-map path on any display
/// (used by smoke tests on machines without an HDR monitor).
fn force_hdr_path() -> bool {
    std::env::var_os("FROSTCAP_FORCE_HDR_PATH").is_some_and(|v| v != "0")
}

fn hdr_of_monitor(handle: usize) -> HdrInfo {
    if force_hdr_path() {
        return HdrInfo {
            hdr: true,
            sdr_white_nits: crate::hdr::SCRGB_WHITE_NITS,
        };
    }
    crate::hdr_gpu::monitor_hdr(handle)
}

fn hdr_of(target: ScreenTarget) -> HdrInfo {
    match target {
        ScreenTarget::Monitor(h) => hdr_of_monitor(h),
        ScreenTarget::Window(h) => {
            // SAFETY: plain query on a window handle.
            let m = unsafe { MonitorFromWindow(HWND(h as *mut _), MONITOR_DEFAULTTONEAREST) };
            hdr_of_monitor(m.0 as usize)
        }
        ScreenTarget::AllDisplays => HdrInfo::default(),
    }
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
        let (device, context) = shared_device()?;
        let dxgi: IDXGIDevice = device.cast()?;
        // SAFETY: valid DXGI device.
        let inspectable: IInspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }?;
        let rt_device: IDirect3DDevice = inspectable.cast()?;

        // Sources: one for monitors/windows/regions, one per monitor for all displays.
        let mut items: Vec<GraphicsCaptureItem> = Vec::new();
        let mut srcs: Vec<Src> = Vec::new();
        let (width, height) = match (cfg.target, &cfg.canvas) {
            (ScreenTarget::AllDisplays, Some(layout)) => {
                for tile in &layout.tiles {
                    let m = cfg
                        .monitors
                        .get(tile.index)
                        .ok_or_else(|| Error::Invalid("canvas tile without a monitor".into()))?;
                    let item = create_item(ScreenTarget::Monitor(m.handle))?;
                    srcs.push(Src {
                        dst: tile.dst,
                        tile: tile.size,
                        src_size: (m.bounds.width as u32, m.bounds.height as u32),
                        hdr: hdr_of_monitor(m.handle),
                        pool_size: item.Size()?,
                    });
                    items.push(item);
                }
                (layout.width, layout.height)
            }
            (ScreenTarget::AllDisplays, None) => {
                return Err(Error::Invalid("all displays need a canvas layout".into()));
            }
            (target, _) => {
                let item = create_item(target)?;
                let size = item.Size()?;
                srcs.push(Src {
                    dst: (0, 0),
                    tile: (0, 0),
                    src_size: (0, 0),
                    hdr: hdr_of(target),
                    pool_size: size,
                });
                items.push(item);
                match (cfg.size, cfg.crop) {
                    (Some(s), _) => s,
                    (None, Some(c)) => (c.width.max(2) as u32 & !1, c.height.max(2) as u32 & !1),
                    (None, None) => (
                        size.Width.max(2) as u32 & !1,
                        size.Height.max(2) as u32 & !1,
                    ),
                }
            }
        };
        let mut warnings = Vec::new();
        let scaled = cfg.canvas.as_ref().is_some_and(|c| c.scale < 1.0);
        let converter = if scaled || srcs.iter().any(|s| s.hdr.hdr) {
            match Converter::new(&device, &context) {
                Ok(c) => Some(c),
                Err(e) if scaled => return Err(e),
                Err(e) => {
                    warnings.push(format!(
                        "HDR conversion unavailable ({e}); capturing as SDR"
                    ));
                    srcs.iter_mut().for_each(|s| s.hdr.hdr = false);
                    None
                }
            }
        } else {
            None
        };

        // The encoder takes ~1 s to start (hardware MFT): create it while the WGC
        // frame pools and sessions are set up below.
        let writer_cfg = WriterConfig::with_auto_bitrate(width, height, cfg.fps);
        let writer_job = {
            let (dev, path) = (SendBox(device.clone()), cfg.path.clone());
            let h = thread::Builder::new()
                .name("frostcap-encoder-init".into())
                .spawn(move || {
                    let dev = dev;
                    let t = Instant::now();
                    // SAFETY: balanced below; the process MTA is pinned anyway.
                    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
                    let w = H264Writer::create(&path, writer_cfg, WriterInput::D3D11Bgra(&dev.0));
                    if com {
                        // SAFETY: paired with the successful CoInitializeEx above.
                        unsafe { CoUninitialize() };
                    }
                    (w, t.elapsed().as_millis() as u64)
                })
                .map_err(|e| Error::Capture(e.to_string()))?;
            JoinGuard::new(h)
        };

        let min_interval = clock::HNS_PER_SEC / i64::from(cfg.fps.max(1));
        let mut sessions = Vec::new();
        for (item, src) in items.iter().zip(&srcs) {
            let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                &rt_device,
                src.format(),
                2,
                src.pool_size,
            )?;
            let session = pool.CreateCaptureSession(item)?;
            let _ = session.SetIsCursorCaptureEnabled(cfg.cursor);
            // The yellow capture border is on by default (a visible sign of recording).
            if !cfg.border && session.SetIsBorderRequired(false).is_err() {
                warnings.push("the capture border could not be hidden".to_owned());
            }
            let _ = session.SetMinUpdateInterval(windows::Foundation::TimeSpan {
                Duration: min_interval,
            });
            sessions.push((pool, session));
        }
        let capture_setup_ms = t.elapsed().as_millis() as u64;

        let (writer, encoder_setup_ms) = writer_job
            .join()
            .map_err(|_| Error::Capture("encoder start panicked".into()))?;
        let writer = writer?;

        let canvas = matches!(cfg.target, ScreenTarget::AllDisplays)
            .then(|| new_texture(&device, width, height))
            .transpose()?;
        let mut shared = Shared {
            writer: Some(writer),
            device: device.clone(),
            context,
            last_tex: None,
            srcs,
            canvas: None,
            dirty: false,
            converter,
            crop: cfg.crop,
            width,
            height,
            min_interval,
            error: None,
            warnings,
        };
        if let Some(c) = canvas {
            shared.clear(&c)?; // monitor gaps stay black
            shared.canvas = Some(c);
        }
        let shared = Arc::new(Mutex::new(shared));

        let mut live = Vec::new();
        for (idx, (item, (pool, session))) in items.into_iter().zip(sessions).enumerate() {
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
                        match s.on_frame(idx, &tex, content, pos) {
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
                    let src = &mut s.srcs[idx];
                    if content.Width != src.pool_size.Width
                        || content.Height != src.pool_size.Height
                    {
                        src.pool_size = content;
                        let format = src.format();
                        pool.Recreate(rt_device.get(), format, 2, content)?;
                    }
                    drop(s);
                    frame.Close()?;
                    Ok(())
                }
            }))?;
            let closed_token = item.Closed(&TypedEventHandler::<
                GraphicsCaptureItem,
                IInspectable,
            >::new({
                let closed = closed.clone();
                move |_, _| {
                    closed.store(true, Ordering::Relaxed);
                    Ok(())
                }
            }))?;
            live.push(Live {
                item,
                pool,
                session,
                token,
                closed_token,
            });
        }
        for l in &live {
            l.session.StartCapture()?;
        }
        Ok((
            live,
            shared,
            StartTimings {
                width,
                height,
                capture_setup_ms,
                encoder_setup_ms,
                ..StartTimings::default()
            },
        ))
    })();

    let (live, shared, timings) = match setup {
        Ok(v) => {
            let _ = ready.send(Ok(v.2));
            v
        }
        Err(e) => {
            let _ = ready.send(Err(Error::Coded(e.to_payload())));
            let _ = std::fs::remove_file(&cfg.path);
            return Err(e);
        }
    };
    let _ = timings;

    // Wait for stop (or the session owner being dropped). Meanwhile flush pending
    // changes / repeat the last frame so fragments keep reaching the disk (a crash
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
            && let Err(e) = s.tick(pos)
        {
            if let Ok(mut f) = fault.lock() {
                *f = Some(Error::Coded(e.to_payload()));
            }
            s.error = Some(e);
        }
    }
    let stop_now = clock::now_hns();

    // Stop frames first, then finalize with the hold frame.
    for l in &live {
        let _ = l.pool.RemoveFrameArrived(l.token);
        let _ = l.item.RemoveClosed(l.closed_token);
        let _ = l.session.Close();
    }
    // Give an in-flight callback a moment to release the lock.
    thread::sleep(Duration::from_millis(10));
    for l in &live {
        let _ = l.pool.Close();
    }

    let mut s = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut warnings = std::mem::take(&mut s.warnings);
    if closed.load(Ordering::Relaxed) {
        warnings.push("the captured window or display was closed during recording".into());
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
    #[ignore = "needs a D3D11 GPU"]
    fn shared_device_is_created_once_and_reused() {
        let (a, _) = shared_device().unwrap();
        let (b, _) = shared_device().unwrap();
        assert_eq!(a.as_raw(), b.as_raw());
    }

    #[test]
    fn dropping_the_guard_waits_for_the_thread() {
        use std::sync::atomic::AtomicBool;
        let done = Arc::new(AtomicBool::new(false));
        let d = done.clone();
        let g = JoinGuard::new(thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            d.store(true, Ordering::SeqCst);
        }));
        // An early return drops the guard: the work must have finished by then.
        let early = || -> Result<()> {
            let _g = g;
            Err(Error::Capture("injected failure".into()))
        };
        assert!(early().is_err());
        assert!(done.load(Ordering::SeqCst), "thread was left running");
        let g = JoinGuard::new(thread::spawn(|| 7));
        assert_eq!(g.join().unwrap(), 7);
    }

    #[test]
    fn throttle_and_box_helpers() {
        let iv = clock::HNS_PER_SEC / 60;
        assert!(!throttled(None, 5, iv));
        assert!(throttled(Some(1_000), 1_000 + iv / 2, iv));
        assert!(!throttled(Some(1_000), 1_000 + iv, iv));
        let r = box_to_rect(&D3D11_BOX {
            left: 4,
            top: 2,
            front: 0,
            right: 14,
            bottom: 12,
            back: 1,
        });
        assert_eq!((r.x, r.y, r.width, r.height), (4, 2, 10, 10));
    }

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
                border: true,
                monitors: vec![],
                canvas: None,
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
