//! Source thumbnails: one WGC frame per source, downscaled and PNG-encoded with
//! the OS Windows Imaging Component, returned as `data:image/png;base64,` URIs.

use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::Direct3D11CaptureFramePool;
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat24bppBGR,
    GUID_WICPixelFormat32bppBGR, IWICImagingFactory, WICBitmapEncoderNoCache,
    WICBitmapInterpolationModeFant,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, STATFLAG_NONAME,
    STATSTG, STREAM_SEEK_SET,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::core::{IInspectable, Interface};

use crate::error::{Error, Result};
use crate::screen::{ScreenTarget, create_device_with, create_item};

/// Total time budget for all thumbnails.
const BUDGET: Duration = Duration::from_millis(1400);
/// Per-source wait for the first frame.
const PER_SOURCE: Duration = Duration::from_millis(300);

/// Process-wide capture objects (D3D device creation alone can take ~1 s).
struct Ctx {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    rt: IDirect3DDevice,
    wic: IWICImagingFactory,
}
// SAFETY: only used while holding the CTX mutex; the device is multithread
// protected and the WIC factory is free-threaded.
unsafe impl Send for Ctx {}

fn make_ctx() -> Result<Ctx> {
    let (device, context) = create_device_with(false)?;
    let dxgi: IDXGIDevice = device.cast()?;
    // SAFETY: valid DXGI device.
    let rt: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }?.cast()?;
    // SAFETY: standard WIC factory creation.
    let wic: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }?;
    Ok(Ctx {
        device,
        context,
        rt,
        wic,
    })
}

static CTX: Mutex<Option<Ctx>> = Mutex::new(None);

/// Capture thumbnails for `targets` (None entries stay None). Never fails as a
/// whole. Warm calls are bounded by `BUDGET`; sources past it get `None`.
pub fn capture_thumbnails(targets: &[Option<ScreenTarget>], max_width: u32) -> Vec<Option<String>> {
    crate::com::ensure_mta();
    // SAFETY: the MTA is pinned process-wide, so this is only a per-thread join.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let mut guard = CTX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_none() {
        *guard = make_ctx().ok();
    }
    let Some(ctx) = guard.as_ref() else {
        return vec![None; targets.len()];
    };
    let start = Instant::now();
    targets
        .iter()
        .map(|t| {
            let t = (*t)?;
            let left = BUDGET.saturating_sub(start.elapsed());
            if left < Duration::from_millis(60) {
                return None;
            }
            one(
                t,
                &ctx.device,
                &ctx.context,
                &ctx.rt,
                &ctx.wic,
                max_width,
                left.min(PER_SOURCE),
            )
            .ok()
        })
        .collect()
}
fn one(
    target: ScreenTarget,
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    rt: &IDirect3DDevice,
    wic: &IWICImagingFactory,
    max_width: u32,
    wait: Duration,
) -> Result<String> {
    let item = create_item(target)?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        rt,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        1,
        item.Size()?,
    )?;
    let session = pool.CreateCaptureSession(&item)?;
    let _ = session.SetIsBorderRequired(false);
    let _ = session.SetIsCursorCaptureEnabled(false);
    let (tx, rx) = mpsc::sync_channel::<ID3D11Texture2D>(1);
    let dev = device.clone();
    let ctx = context.clone();
    let token = pool.FrameArrived(
        &TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |p, _| {
            let Some(p) = p.as_ref() else { return Ok(()) };
            let frame = p.TryGetNextFrame()?;
            let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
            // SAFETY: WGC surfaces are D3D11 textures.
            let tex: ID3D11Texture2D = unsafe { access.GetInterface() }?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: plain getters / copy into a staging texture we create.
            unsafe {
                tex.GetDesc(&mut desc);
                desc.Usage = D3D11_USAGE_STAGING;
                desc.BindFlags = 0;
                desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                desc.MiscFlags = 0;
                let mut staging = None;
                dev.CreateTexture2D(&desc, None, Some(&mut staging))?;
                if let Some(s) = staging {
                    ctx.CopyResource(&s, &tex);
                    let _ = tx.try_send(s);
                }
            }
            frame.Close()?;
            Ok(())
        }),
    )?;
    session.StartCapture()?;
    let staging = rx
        .recv_timeout(wait)
        .map_err(|_| Error::Capture("no frame".into()));
    let _ = pool.RemoveFrameArrived(token);
    let _ = session.Close();
    let _ = pool.Close();
    encode_png(&staging?, context, wic, max_width)
}

fn encode_png(
    staging: &ID3D11Texture2D,
    context: &ID3D11DeviceContext,
    wic: &IWICImagingFactory,
    max_width: u32,
) -> Result<String> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: map/unmap of our staging texture; WIC copies the pixels before unmap.
    unsafe {
        staging.GetDesc(&mut desc);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let len = mapped.RowPitch as usize * desc.Height as usize;
        let pixels = std::slice::from_raw_parts(mapped.pData as *const u8, len);
        let bitmap = wic.CreateBitmapFromMemory(
            desc.Width,
            desc.Height,
            &GUID_WICPixelFormat32bppBGR,
            mapped.RowPitch,
            pixels,
        );
        context.Unmap(staging, 0);
        let bitmap = bitmap?;
        let tw = max_width.min(desc.Width).max(1);
        let th =
            ((u64::from(desc.Height) * u64::from(tw)) / u64::from(desc.Width.max(1))).max(1) as u32;
        let scaler = wic.CreateBitmapScaler()?;
        scaler.Initialize(&bitmap, tw, th, WICBitmapInterpolationModeFant)?;

        let stream =
            SHCreateMemStream(None).ok_or_else(|| Error::Capture("SHCreateMemStream".into()))?;
        let encoder = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or_else(|| Error::Capture("CreateNewFrame".into()))?;
        frame.Initialize(None)?;
        frame.SetSize(tw, th)?;
        let mut fmt = GUID_WICPixelFormat24bppBGR;
        frame.SetPixelFormat(&mut fmt)?;
        frame.WriteSource(&scaler, std::ptr::null())?;
        frame.Commit()?;
        encoder.Commit()?;

        let mut stat = STATSTG::default();
        stream.Stat(&mut stat, STATFLAG_NONAME)?;
        let size = usize::try_from(stat.cbSize).unwrap_or(0).min(8 << 20);
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut buf = vec![0u8; size];
        let mut read = 0u32;
        stream
            .Read(buf.as_mut_ptr().cast(), size as u32, Some(&mut read))
            .ok()?;
        buf.truncate(read as usize);
        Ok(format!("data:image/png;base64,{}", base64(&buf)))
    }
}

/// Standard base64 (RFC 4648) without line breaks.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    #[ignore = "needs interactive desktop"]
    fn many_sources_stay_within_the_time_budget() {
        let m = windows_capture::monitor::Monitor::primary().unwrap();
        let t = Some(ScreenTarget::Monitor(m.as_raw_hmonitor() as usize));
        // Cold D3D driver load (~1-2 s on some machines) happens once per process
        // (the app prewarms at startup); the budget applies to warm calls.
        let _ = capture_thumbnails(&[t], 64);
        let start = Instant::now();
        let out = capture_thumbnails(&[t; 12], 64);
        assert_eq!(out.len(), 12);
        assert!(
            start.elapsed() < Duration::from_millis(2000),
            "took {:?}",
            start.elapsed()
        );
        assert!(out[0].is_some());
    }

    #[test]
    #[ignore = "needs interactive desktop"]
    fn primary_monitor_thumbnail_is_png() {
        let m = windows_capture::monitor::Monitor::primary().unwrap();
        let t = capture_thumbnails(
            &[Some(ScreenTarget::Monitor(m.as_raw_hmonitor() as usize))],
            160,
        );
        let uri = t[0].as_ref().expect("thumbnail");
        assert!(
            uri.starts_with("data:image/png;base64,iVBORw0KGgo"),
            "{}",
            &uri[..40.min(uri.len())]
        );
    }
}
