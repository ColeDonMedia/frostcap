//! Media Foundation: H.264 decode (Source Reader, DXVA hardware decode through a
//! D3D11 device manager when available) and H.264 + AAC encode (Sink Writer,
//! hardware MFTs enabled). Uses only OS components; no codecs are bundled.

#![allow(clippy::undocumented_unsafe_blocks)]

use std::mem::ManuallyDrop;
use std::path::Path;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11CreateDevice, ID3D11Device, ID3D11Multithread,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Variant::VT_I8;
use windows::core::{GUID, HSTRING, Interface};

use crate::error::{RenderError, Result};

/// COM + Media Foundation lifetime for the current thread.
pub struct MfGuard {
    co: bool,
}

impl MfGuard {
    /// `CoInitializeEx(MTA)` (tolerates an existing apartment) + `MFStartup`.
    pub fn new() -> Result<Self> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let co = hr.is_ok(); // S_OK / S_FALSE; RPC_E_CHANGED_MODE leaves the STA in place
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }?;
        Ok(Self { co })
    }
}

impl Drop for MfGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            if self.co {
                CoUninitialize();
            }
        }
    }
}

fn attrs(n: u32) -> Result<IMFAttributes> {
    let mut a = None;
    unsafe { MFCreateAttributes(&mut a, n) }?;
    a.ok_or_else(|| RenderError::Media("MFCreateAttributes".into()))
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

/// D3D11 device + DXGI device manager for DXVA decoding.
fn d3d11_manager() -> Option<IMFDXGIDeviceManager> {
    unsafe {
        let mut dev: Option<ID3D11Device> = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut dev),
            None,
            None,
        )
        .ok()?;
        let dev = dev?;
        if let Ok(mt) = dev.cast::<ID3D11Multithread>() {
            let _ = mt.SetMultithreadProtected(true);
        }
        let mut token = 0u32;
        let mut mgr = None;
        MFCreateDXGIDeviceManager(&mut token, &mut mgr).ok()?;
        let mgr = mgr?;
        mgr.ResetDevice(&dev, token).ok()?;
        Some(mgr)
    }
}

/// Row pitch to assume for a flat (non-2D) buffer: the media type's
/// `MF_MT_DEFAULT_STRIDE` when present, else the *coded* width (e.g. 1088 for
/// a 1080-wide H.264 stream), never the visible aperture width.
pub fn fallback_pitch(default_stride: Option<i32>, coded_width: u32, bytes_per_px: u32) -> usize {
    match default_stride {
        Some(s) if s != 0 => s.unsigned_abs() as usize,
        _ => (coded_width * bytes_per_px) as usize,
    }
}

/// Where the planes of a locked buffer are, as byte ranges `(offset, len)`
/// from the buffer start; every range lies inside the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// Row pitch (bytes, positive).
    pub pitch: usize,
    /// First plane (Y or BGRA). For bottom-up buffers the *last* row in memory
    /// is the top picture row (row r starts at `end - row_bytes - r * pitch`... see
    /// [`Geometry::row`]).
    pub plane0: (usize, usize),
    /// Interleaved UV plane (NV12 only).
    pub plane1: Option<(usize, usize)>,
    /// Rows are stored bottom-up (RGB only).
    pub bottom_up: bool,
}

impl Geometry {
    /// Byte offset (from the buffer start) of picture row `r` of plane 0.
    pub fn row(&self, r: usize, height: usize) -> usize {
        if self.bottom_up {
            self.plane0.0 + (height - 1 - r) * self.pitch
        } else {
            self.plane0.0 + r * self.pitch
        }
    }
}

/// Validate a locked buffer: `line0_off` = scanline 0 minus buffer start
/// (`None` if negative), `pitch` as reported (negative = bottom-up), `len` =
/// bytes available from the buffer start. Returns `None` for anything that
/// would read outside the buffer.
pub fn buffer_geometry(
    nv12: bool,
    line0_off: Option<usize>,
    pitch: i32,
    len: usize,
    width: u32,
    height: u32,
) -> Option<Geometry> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || pitch == 0 {
        return None;
    }
    let off = line0_off?;
    let ap = pitch.unsigned_abs() as usize;
    let row_bytes = if nv12 { w } else { w.checked_mul(4)? };
    if ap < row_bytes || off >= len {
        return None;
    }
    if pitch < 0 {
        if nv12 {
            return None;
        }
        let start = off.checked_sub(ap.checked_mul(h - 1)?)?;
        let end = off.checked_add(row_bytes)?;
        return (end <= len).then_some(Geometry {
            pitch: ap,
            plane0: (start, end - start),
            plane1: None,
            bottom_up: true,
        });
    }
    let p0_len = ap.checked_mul(h - 1)?.checked_add(row_bytes)?;
    if off.checked_add(p0_len)? > len {
        return None;
    }
    let plane1 = if nv12 {
        let (cw, ch) = (w.div_ceil(2) * 2, h.div_ceil(2));
        let rows_alloc = ((len - off) / ap * 2 / 3).max(h);
        let uv0 = off.checked_add(ap.checked_mul(rows_alloc)?)?;
        let uv_len = ap.checked_mul(ch - 1)?.checked_add(cw)?;
        if ap < cw || uv0.checked_add(uv_len)? > len {
            return None;
        }
        Some((uv0, uv_len))
    } else {
        None
    };
    Some(Geometry {
        pitch: ap,
        plane0: (off, p0_len),
        plane1,
        bottom_up: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(
        nv12: bool,
        off: Option<usize>,
        pitch: i32,
        len: usize,
        w: u32,
        h: u32,
    ) -> Option<Geometry> {
        buffer_geometry(nv12, off, pitch, len, w, h)
    }

    #[test]
    fn geometry_top_down_tight_and_padded() {
        let tight = g(true, Some(0), 1920, 1920 * 1080 * 3 / 2, 1920, 1080).unwrap();
        assert_eq!(tight.pitch, 1920);
        assert_eq!(tight.plane0, (0, 1920 * 1080));
        assert_eq!(tight.plane1, Some((1920 * 1080, 1920 * 540)));
        assert!(!tight.bottom_up);
        // Decoder surfaces padded to 1088 rows: UV starts after the padding.
        let padded = g(true, Some(0), 1920, 1920 * 1088 * 3 / 2, 1920, 1080).unwrap();
        assert_eq!(padded.plane1.unwrap().0, 1920 * 1088);
        // Coded width 1088 for a 1080-wide picture.
        let wide = g(true, Some(0), 1088, 1088 * 1920 * 3 / 2, 1080, 1920).unwrap();
        assert_eq!(wide.pitch, 1088);
        assert_eq!(wide.plane1.unwrap().0, 1088 * 1920);
        // Odd width: chroma rows are ceil(w/2)*2 bytes, ceil(h/2) rows.
        let odd = g(true, Some(0), 6, 6 * 3 + 6 * 2, 5, 3).unwrap();
        assert_eq!(odd.plane0, (0, 6 * 2 + 5));
        assert_eq!(odd.plane1, Some((18, 6 + 6)));
        // Data starting after a header.
        let off = g(false, Some(64), 16, 64 + 16 * 3, 4, 3).unwrap();
        assert_eq!(off.plane0, (64, 48));
    }

    #[test]
    fn geometry_rejects_short_and_bogus_buffers() {
        assert!(g(true, Some(0), 1920, 1920 * 1080, 1920, 1080).is_none()); // no UV plane
        assert!(g(false, Some(0), 16, 16 * 3 - 1, 4, 3).is_none()); // one byte short
        assert!(g(false, None, 16, 4096, 4, 3).is_none()); // scanline0 before buffer start
        assert!(g(false, Some(4096), 16, 4096, 4, 3).is_none()); // starts past the end
        assert!(g(false, Some(0), 0, 4096, 4, 3).is_none());
        assert!(g(false, Some(0), 8, 4096, 4, 3).is_none()); // pitch < row bytes
        assert!(g(true, Some(0), 4, 4096, 0, 3).is_none()); // empty picture
        let _ = g(false, Some(0), i32::MAX, usize::MAX, 4, 3); // must not overflow-panic
        assert!(g(false, Some(usize::MAX - 2), 16, usize::MAX, 4, 3).is_none());
    }

    #[test]
    fn geometry_bottom_up_rgb() {
        // 4x3 RGB32, rows stored bottom-up: scanline0 (top row) is the last row in memory.
        let b = g(false, Some(32), -16, 48, 4, 3).unwrap();
        assert!(b.bottom_up);
        assert_eq!(b.pitch, 16);
        assert_eq!(b.plane0, (0, 48));
        assert!(g(false, Some(16), -16, 48, 4, 3).is_none()); // would read before the start
        assert!(g(false, Some(40), -16, 48, 4, 3).is_none()); // top row runs past the end
        assert!(g(true, Some(32), -16, 48, 4, 3).is_none()); // NV12 is never bottom-up
    }

    #[test]
    fn fallback_pitch_prefers_stride_then_coded_width() {
        assert_eq!(fallback_pitch(Some(4352), 1088, 4), 4352);
        assert_eq!(fallback_pitch(Some(-4352), 1088, 4), 4352); // bottom-up
        assert_eq!(fallback_pitch(None, 1088, 4), 4352);
        assert_eq!(fallback_pitch(None, 1088, 1), 1088);
    }
}

/// Output pixel format of a [`VideoReader`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// NV12 (decoder native; used by the exporter).
    Nv12,
    /// BGRA via the MF video processor (used for verification / thumbnails).
    Bgra,
}

/// A decoded frame (keeps the MF sample alive).
pub struct DecodedFrame {
    /// Presentation time relative to the first frame (100 ns).
    pub pts: i64,
    sample: IMFSample,
}

/// Locked planes of a frame.
pub struct Planes<'a> {
    /// First plane (Y for NV12, BGRA rows otherwise), top row first.
    pub plane0: &'a [u8],
    /// Interleaved UV plane (NV12 only).
    pub plane1: &'a [u8],
    /// Bytes per row (positive).
    pub pitch: usize,
}

/// Sequential video decoder with seeking.
pub struct VideoReader {
    reader: IMFSourceReader,
    /// Visible width.
    pub width: u32,
    /// Visible height.
    pub height: u32,
    /// Pixel format delivered.
    pub format: PixelFormat,
    /// BT.709 (true) or BT.601 YUV matrix.
    pub bt709: bool,
    /// Full-range (0..255) YUV.
    pub full_range: bool,
    /// DXVA (hardware) decode via a D3D11 device manager.
    pub hardware: bool,
    /// Duration (100 ns) if known.
    pub duration: i64,
    first_pts: Option<i64>,
    stride: i32,
}

const STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

impl VideoReader {
    /// Open the first video stream of `path`.
    pub fn open(path: &Path, format: PixelFormat, allow_hw: bool) -> Result<Self> {
        let mgr = if allow_hw && format == PixelFormat::Nv12 {
            d3d11_manager()
        } else {
            None
        };
        match Self::open_with(path, format, mgr.as_ref()) {
            Ok(r) => Ok(r),
            Err(_) if mgr.is_some() => Self::open_with(path, format, None),
            Err(e) => Err(e),
        }
    }

    fn open_with(
        path: &Path,
        format: PixelFormat,
        mgr: Option<&IMFDXGIDeviceManager>,
    ) -> Result<Self> {
        unsafe {
            let a = attrs(4)?;
            a.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            if let Some(m) = mgr {
                a.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, m)?;
            }
            if format == PixelFormat::Bgra {
                a.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
            }
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &a)?;
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(STREAM, true)?;
            let t = MFCreateMediaType()?;
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            let sub = if format == PixelFormat::Nv12 {
                MFVideoFormat_NV12
            } else {
                MFVideoFormat_RGB32
            };
            t.SetGUID(&MF_MT_SUBTYPE, &sub)?;
            reader.SetCurrentMediaType(STREAM, None, &t)?;
            let duration = reader
                .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
                .map(|v| v.Anonymous.Anonymous.Anonymous.hVal)
                .unwrap_or(0);
            let mut r = Self {
                reader,
                width: 0,
                height: 0,
                format,
                bt709: true,
                full_range: false,
                hardware: mgr.is_some(),
                duration,
                first_pts: None,
                stride: 0,
            };
            r.apply_type()?;
            Ok(r)
        }
    }

    /// (Re)read the current output type: size (display aperture), YUV matrix,
    /// range and pitch. Called at open and whenever the decoder reports
    /// `MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED`.
    fn apply_type(&mut self) -> Result<()> {
        unsafe {
            let cur = self.reader.GetCurrentMediaType(STREAM)?;
            let fs = cur.GetUINT64(&MF_MT_FRAME_SIZE)?;
            let (mut width, mut height) = ((fs >> 32) as u32, fs as u32);
            let coded_width = width;
            let mut blob = [0u8; 16];
            if cur
                .GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut blob, None)
                .is_ok()
            {
                let cx = u32::from_le_bytes([blob[8], blob[9], blob[10], blob[11]]);
                let cy = u32::from_le_bytes([blob[12], blob[13], blob[14], blob[15]]);
                if cx > 0 && cy > 0 && cx <= width && cy <= height {
                    width = cx;
                    height = cy;
                }
            }
            if width == 0 || height == 0 {
                return Err(RenderError::Media(
                    "decoder reported an empty frame size".into(),
                ));
            }
            self.bt709 = cur
                .GetUINT32(&MF_MT_YUV_MATRIX)
                .map(|m| m != MFVideoTransferMatrix_BT601.0 as u32)
                .unwrap_or(true);
            self.full_range = cur
                .GetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE)
                .map(|r| r == MFNominalRange_0_255.0 as u32)
                .unwrap_or(false);
            let bpp = if self.format == PixelFormat::Nv12 {
                1
            } else {
                4
            };
            self.stride = fallback_pitch(
                cur.GetUINT32(&MF_MT_DEFAULT_STRIDE).ok().map(|s| s as i32),
                coded_width,
                bpp,
            ) as i32;
            self.width = width;
            self.height = height;
            Ok(())
        }
    }

    /// Next frame, `None` at end of stream.
    pub fn read(&mut self) -> Result<Option<DecodedFrame>> {
        loop {
            let mut flags = 0u32;
            let mut ts = 0i64;
            let mut sample = None;
            unsafe {
                self.reader.ReadSample(
                    STREAM,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut ts),
                    Some(&mut sample),
                )?;
            }
            if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                return Err(RenderError::Media("source reader error".into()));
            }
            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                // Size / pitch may differ from here on: never keep stale geometry.
                self.apply_type()?;
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Ok(None);
            }
            let Some(sample) = sample else { continue };
            let first = *self.first_pts.get_or_insert(ts);
            return Ok(Some(DecodedFrame {
                pts: ts - first,
                sample,
            }));
        }
    }

    /// Seek to `t` (100 ns, relative to the first frame). Decoding resumes at
    /// the preceding key frame; callers skip frames until they reach `t`.
    pub fn seek(&mut self, t: i64) -> Result<()> {
        if self.first_pts.is_none() {
            // Establish the time origin (first frame) before jumping.
            let _ = self.read()?;
        }
        let pos = (t + self.first_pts.unwrap_or(0)).max(0);
        let pv = PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_I8,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 { hVal: pos },
                }),
            },
        };
        unsafe { self.reader.SetCurrentPosition(&GUID::zeroed(), &pv) }?;
        Ok(())
    }

    /// Lock a frame's buffer and hand its planes to `f`. Every slice is derived
    /// from the locked buffer length via [`buffer_geometry`]; unexpected layouts
    /// are errors, never out-of-bounds reads.
    pub fn with_planes<R>(
        &self,
        frame: &DecodedFrame,
        f: impl FnOnce(Planes<'_>) -> R,
    ) -> Result<R> {
        let (w, h) = (self.width, self.height);
        let nv12 = self.format == PixelFormat::Nv12;
        let bad = |pitch: i32, len: usize| {
            RenderError::Media(format!(
                "unexpected frame buffer layout (pitch {pitch}, {len} bytes, {w}x{h})"
            ))
        };
        unsafe {
            let buf = frame.sample.ConvertToContiguousBuffer()?;
            if let Ok(b2) = buf.cast::<IMF2DBuffer2>() {
                let (mut line0, mut pitch, mut start, mut len) =
                    (std::ptr::null_mut(), 0i32, std::ptr::null_mut(), 0u32);
                b2.Lock2DSize(
                    MF2DBuffer_LockFlags_Read,
                    &mut line0,
                    &mut pitch,
                    &mut start,
                    &mut len,
                )?;
                let off = (line0 as usize).checked_sub(start as usize);
                let res = if start.is_null() {
                    None
                } else {
                    buffer_geometry(nv12, off, pitch, len as usize, w, h).map(|g| {
                        let all = std::slice::from_raw_parts(start as *const u8, len as usize);
                        emit_planes(all, g, w as usize * if nv12 { 1 } else { 4 }, h as usize, f)
                    })
                };
                b2.Unlock2D()?;
                return res.ok_or_else(|| bad(pitch, len as usize));
            }
            // Plain system-memory buffer: rows at the media type's pitch.
            let mut p = std::ptr::null_mut();
            let mut cur = 0u32;
            buf.Lock(&mut p, None, Some(&mut cur))?;
            let res = if p.is_null() {
                None
            } else {
                buffer_geometry(nv12, Some(0), self.stride, cur as usize, w, h).map(|g| {
                    let all = std::slice::from_raw_parts(p as *const u8, cur as usize);
                    emit_planes(all, g, w as usize * if nv12 { 1 } else { 4 }, h as usize, f)
                })
            };
            buf.Unlock()?;
            res.ok_or_else(|| bad(self.stride, cur as usize))
        }
    }
}

/// Build [`Planes`] from a validated geometry (bottom-up buffers are copied
/// into a top-down scratch buffer).
fn emit_planes<R>(
    all: &[u8],
    g: Geometry,
    row_bytes: usize,
    h: usize,
    f: impl FnOnce(Planes<'_>) -> R,
) -> R {
    if g.bottom_up {
        let mut tmp = vec![0u8; g.pitch * (h - 1) + row_bytes];
        for r in 0..h {
            let s = g.row(r, h);
            tmp[r * g.pitch..r * g.pitch + row_bytes].copy_from_slice(&all[s..s + row_bytes]);
        }
        return f(Planes {
            plane0: &tmp,
            plane1: &[],
            pitch: g.pitch,
        });
    }
    let (o0, l0) = g.plane0;
    let plane1 = g.plane1.map_or(&[][..], |(o, l)| &all[o..o + l]);
    f(Planes {
        plane0: &all[o0..o0 + l0],
        plane1,
        pitch: g.pitch,
    })
}

/// Decode the frame shown at `t_ms` as tight RGBA8 (MF video processor path,
/// independent of the exporter's NV12 + shader path). For tests / verification.
pub fn decode_frame_rgba(path: &Path, t_ms: f64) -> Result<(u32, u32, Vec<u8>)> {
    let mut r = VideoReader::open(path, PixelFormat::Bgra, false)?;
    let target = (t_ms * 10_000.0) as i64;
    if target > 0 {
        r.seek(target)?;
    }
    let mut best: Option<DecodedFrame> = None;
    while let Some(f) = r.read()? {
        if f.pts > target + 5_000 && best.is_some() {
            break;
        }
        best = Some(f);
    }
    let f = best.ok_or_else(|| RenderError::Media("no frames".into()))?;
    let (w, h) = (r.width as usize, r.height as usize);
    r.with_planes(&f, |p| {
        let mut out = vec![0u8; w * h * 4];
        for y in 0..h {
            let row = &p.plane0[y * p.pitch..y * p.pitch + w * 4];
            for x in 0..w {
                let s = &row[x * 4..x * 4 + 4];
                out[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[s[2], s[1], s[0], 255]);
            }
        }
        (w as u32, h as u32, out)
    })
}

/// Like [`decode_frame_rgba`] but through the NV12 path with a CPU BT.709
/// conversion (no MF video processor). For cross-checking verification.
pub fn decode_frame_rgba_nv12(path: &Path, t_ms: f64) -> Result<(u32, u32, Vec<u8>)> {
    let mut r = VideoReader::open(path, PixelFormat::Nv12, false)?;
    let target = (t_ms * 10_000.0) as i64;
    if target > 0 {
        r.seek(target)?;
    }
    let mut best: Option<DecodedFrame> = None;
    while let Some(f) = r.read()? {
        if f.pts > target + 5_000 && best.is_some() {
            break;
        }
        best = Some(f);
    }
    let f = best.ok_or_else(|| RenderError::Media("no frames".into()))?;
    let (w, h, full) = (r.width as usize, r.height as usize, r.full_range);
    r.with_planes(&f, |p| {
        let mut out = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let yy = p.plane0[y * p.pitch + x] as f32;
                let o = (y / 2) * p.pitch + (x / 2) * 2;
                let (u, v) = (p.plane1[o] as f32, p.plane1[o + 1] as f32);
                let (l, cb, cr) = if full {
                    (yy / 255.0, (u - 128.0) / 255.0, (v - 128.0) / 255.0)
                } else {
                    (
                        (yy - 16.0) / 219.0,
                        (u - 128.0) / 224.0,
                        (v - 128.0) / 224.0,
                    )
                };
                let rgb = [
                    l + 1.5748 * cr,
                    l - 0.187_324 * cb - 0.468_124 * cr,
                    l + 1.8556 * cb,
                ];
                for c in 0..3 {
                    out[(y * w + x) * 4 + c] = (rgb[c].clamp(0.0, 1.0) * 255.0).round() as u8;
                }
                out[(y * w + x) * 4 + 3] = 255;
            }
        }
        (w as u32, h as u32, out)
    })
}

/// H.264 (+ optional AAC) MP4 writer.
pub struct Mp4Writer {
    writer: IMFSinkWriter,
    video: u32,
    audio: Option<u32>,
    width: u32,
    height: u32,
    fps: u32,
    frames: u64,
    audio_frames: u64,
    /// A hardware encoder MFT is in use.
    pub hardware: bool,
}

fn ctx(what: &'static str) -> impl Fn(windows::core::Error) -> RenderError {
    move |e| {
        RenderError::Media(format!(
            "{what}: {} (0x{:08X})",
            e.message(),
            e.code().0 as u32
        ))
    }
}

fn hns_for_frame(i: u64, fps: u32) -> i64 {
    (i * 10_000_000 / fps as u64) as i64
}

/// Video codec of an MP4 output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    /// H.264 / AVC.
    H264,
    /// AV1 (needs an encoder MFT, see [`encoder_caps`]).
    Av1,
    /// VP9 (needs an encoder MFT).
    Vp9,
}

impl VideoCodec {
    fn subtype(self) -> GUID {
        match self {
            Self::H264 => MFVideoFormat_H264,
            Self::Av1 => SUBTYPE_AV01,
            Self::Vp9 => SUBTYPE_VP90,
        }
    }
}

impl Mp4Writer {
    /// Create `path` (MP4 container regardless of extension).
    pub fn create(
        path: &Path,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        audio: bool,
        allow_hw: bool,
    ) -> Result<Self> {
        Self::create_codec(
            path,
            width,
            height,
            fps,
            bitrate,
            audio,
            allow_hw,
            VideoCodec::H264,
        )
    }

    /// Like [`Self::create`] with an explicit video codec. Only codecs that
    /// [`encoder_caps`] reports should be requested; AV1/VP9 need a hardware
    /// or extension encoder MFT and the MP4 sink must accept them.
    #[allow(clippy::too_many_arguments)]
    pub fn create_codec(
        path: &Path,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        audio: bool,
        allow_hw: bool,
        codec: VideoCodec,
    ) -> Result<Self> {
        if allow_hw {
            if let Ok(w) = Self::create_with(path, width, height, fps, bitrate, audio, true, codec)
            {
                return Ok(w);
            }
            let _ = std::fs::remove_file(path);
        }
        Self::create_with(path, width, height, fps, bitrate, audio, false, codec)
    }

    #[allow(clippy::too_many_arguments)]
    fn create_with(
        path: &Path,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        audio: bool,
        hw: bool,
        codec: VideoCodec,
    ) -> Result<Self> {
        unsafe {
            let a = attrs(4)?;
            a.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, hw as u32)?;
            a.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &a)?;

            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, &codec.subtype())?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
            out.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
            out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            if codec == VideoCodec::H264 {
                out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            }
            let video = writer.AddStream(&out)?;

            let inp = MFCreateMediaType()?;
            inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            inp.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            inp.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
            inp.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
            inp.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            inp.SetUINT32(&MF_MT_DEFAULT_STRIDE, width)?;
            inp.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
            inp.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
            if writer.SetInputMediaType(video, &inp, None).is_err() {
                inp.DeleteItem(&MF_MT_YUV_MATRIX)?;
                inp.DeleteItem(&MF_MT_VIDEO_NOMINAL_RANGE)?;
                writer.SetInputMediaType(video, &inp, None)?;
            }

            let audio = if audio {
                let ao = MFCreateMediaType()?;
                ao.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                ao.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
                ao.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                ao.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, crate::audio::RATE)?;
                ao.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, crate::audio::CHANNELS as u32)?;
                ao.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24_000)?; // 192 kbit/s
                ao.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?;
                let s = writer.AddStream(&ao)?;
                let ai = MFCreateMediaType()?;
                ai.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                ai.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
                ai.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                ai.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, crate::audio::RATE)?;
                ai.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, crate::audio::CHANNELS as u32)?;
                ai.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
                ai.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, crate::audio::RATE * 4)?;
                writer.SetInputMediaType(s, &ai, None)?;
                Some(s)
            } else {
                None
            };
            writer.BeginWriting()?;
            let hardware = hw && Self::uses_hardware(&writer, video);
            Ok(Self {
                writer,
                video,
                audio,
                width,
                height,
                fps,
                frames: 0,
                audio_frames: 0,
                hardware,
            })
        }
    }

    fn uses_hardware(w: &IMFSinkWriter, stream: u32) -> bool {
        let Ok(ex) = w.cast::<IMFSinkWriterEx>() else {
            return false;
        };
        for i in 0..8 {
            let mut cat = GUID::zeroed();
            let mut t = None;
            if unsafe { ex.GetTransformForStream(stream, i, Some(&mut cat), &mut t) }.is_err() {
                break;
            }
            if cat == MFT_CATEGORY_VIDEO_ENCODER
                && let Some(t) = t
                && let Ok(a) = unsafe { t.GetAttributes() }
            {
                return unsafe { a.GetStringLength(&MFT_ENUM_HARDWARE_URL_Attribute) }.is_ok();
            }
        }
        false
    }

    fn sample(data: &[u8], time: i64, dur: i64) -> Result<IMFSample> {
        unsafe {
            let buf = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
            buf.Unlock()?;
            buf.SetCurrentLength(data.len() as u32)?;
            let s = MFCreateSample()?;
            s.AddBuffer(&buf)?;
            s.SetSampleTime(time)?;
            s.SetSampleDuration(dur)?;
            Ok(s)
        }
    }

    /// Bytes of one tight NV12 frame.
    pub fn nv12_len(&self) -> usize {
        (self.width * self.height * 3 / 2) as usize
    }

    /// Append one NV12 frame (tight rows, Y then interleaved UV).
    ///
    /// The frame goes in an `IMF2DBuffer` allocated by Media Foundation, so the
    /// encoder sees the real pitch. With a flat buffer, hardware encoders assume
    /// a 16-aligned pitch and widths like 1080 come out sheared.
    pub fn write_video(&mut self, nv12: &[u8]) -> Result<()> {
        let t0 = hns_for_frame(self.frames, self.fps);
        let t1 = hns_for_frame(self.frames + 1, self.fps);
        let (w, h) = (self.width as usize, self.height as usize);
        if nv12.len() < w * h * 3 / 2 {
            return Err(RenderError::Media("short NV12 frame".into()));
        }
        let s = unsafe {
            let buf =
                MFCreate2DMediaBuffer(self.width, self.height, MFVideoFormat_NV12.data1, false)
                    .map_err(ctx("MFCreate2DMediaBuffer"))?;
            let b2 = buf.cast::<IMF2DBuffer2>().map_err(ctx("IMF2DBuffer2"))?;
            let (mut line0, mut pitch, mut start, mut len) =
                (std::ptr::null_mut(), 0i32, std::ptr::null_mut(), 0u32);
            b2.Lock2DSize(
                MF2DBuffer_LockFlags_Write,
                &mut line0,
                &mut pitch,
                &mut start,
                &mut len,
            )
            .map_err(ctx("Lock2DSize"))?;
            let ap = pitch.unsigned_abs() as usize;
            let avail = len as usize - (line0 as usize - start as usize);
            let rows_alloc = ((avail / ap.max(1)) * 2 / 3).max(h);
            let ok = pitch > 0 && ap >= w && ap * (rows_alloc + h / 2) <= avail;
            if ok {
                for r in 0..h {
                    std::ptr::copy_nonoverlapping(nv12.as_ptr().add(r * w), line0.add(r * ap), w);
                }
                let uv = &nv12[w * h..];
                for r in 0..h / 2 {
                    std::ptr::copy_nonoverlapping(
                        uv.as_ptr().add(r * w),
                        line0.add(ap * rows_alloc + r * ap),
                        w,
                    );
                }
            }
            b2.Unlock2D()?;
            if !ok {
                return Err(RenderError::Media(format!(
                    "unexpected NV12 buffer layout (pitch {pitch}, {len} bytes)"
                )));
            }
            // A fresh 2D buffer reports length 0; the writer rejects empty samples.
            buf.SetCurrentLength(b2.GetContiguousLength()?)?;
            let s = MFCreateSample()?;
            s.AddBuffer(&buf)?;
            s.SetSampleTime(t0)?;
            s.SetSampleDuration(t1 - t0)?;
            s
        };
        unsafe { self.writer.WriteSample(self.video, &s) }.map_err(ctx("WriteSample(video)"))?;
        self.frames += 1;
        Ok(())
    }

    /// Append interleaved stereo i16 PCM.
    pub fn write_audio(&mut self, pcm: &[i16]) -> Result<()> {
        let Some(stream) = self.audio else {
            return Ok(());
        };
        let rate = crate::audio::RATE as u64;
        for chunk in pcm.chunks(2 * 4800) {
            let frames = (chunk.len() / 2) as u64;
            let t0 = (self.audio_frames * 10_000_000 / rate) as i64;
            let t1 = ((self.audio_frames + frames) * 10_000_000 / rate) as i64;
            let bytes: Vec<u8> = chunk.iter().flat_map(|s| s.to_le_bytes()).collect();
            let s = Self::sample(&bytes, t0, t1 - t0)?;
            unsafe { self.writer.WriteSample(stream, &s) }?;
            self.audio_frames += frames;
        }
        Ok(())
    }

    /// Audio frames written so far.
    pub fn audio_frames(&self) -> u64 {
        self.audio_frames
    }

    /// Finalize the file.
    pub fn finish(self) -> Result<()> {
        unsafe { self.writer.Finalize() }?;
        Ok(())
    }
}

/// Video codecs that have an encoder MFT registered on this machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EncoderCaps {
    /// H.264 (the exporter's only codec).
    pub h264: bool,
    /// VP9 (WebM capable).
    pub vp9: bool,
    /// AV1 (WebM capable).
    pub av1: bool,
}

impl EncoderCaps {
    /// Whether a WebM-capable encoder exists.
    pub fn webm(&self) -> bool {
        self.vp9 || self.av1
    }
}

const SUBTYPE_VP90: GUID = GUID::from_u128(0x30395056_0000_0010_8000_00aa00389b71);
const SUBTYPE_AV01: GUID = GUID::from_u128(0x31305641_0000_0010_8000_00aa00389b71);

/// Friendly names of every video encoder MFT whose output type is `subtype`
/// (hardware and software). Needs Media Foundation started (see [`MfGuard`]).
fn encoder_names(subtype: GUID) -> Vec<String> {
    let out = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtype,
    };
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut n = 0u32;
    // SAFETY: MFTEnumEx fills `list` with `n` activates that we release and free.
    unsafe {
        if MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_ALL,
            None,
            Some(&out),
            &mut list,
            &mut n,
        )
        .is_err()
            || list.is_null()
        {
            return Vec::new();
        }
        let mut names = Vec::new();
        for i in 0..n as usize {
            if let Some(act) = (*list.add(i)).take() {
                let mut buf = [0u16; 256];
                let mut len = 0u32;
                if act
                    .GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buf, Some(&mut len))
                    .is_ok()
                {
                    names.push(String::from_utf16_lossy(&buf[..(len as usize).min(255)]));
                } else {
                    names.push("(unnamed)".into());
                }
            }
        }
        windows::Win32::System::Com::CoTaskMemFree(Some(list as *const _));
        names
    }
}

/// Which encoders Windows offers on this machine (initializes MF itself).
pub fn encoder_caps() -> EncoderCaps {
    let Ok(_g) = MfGuard::new() else {
        return EncoderCaps::default();
    };
    EncoderCaps {
        h264: !encoder_names(MFVideoFormat_H264).is_empty(),
        vp9: !encoder_names(SUBTYPE_VP90).is_empty(),
        av1: !encoder_names(SUBTYPE_AV01).is_empty(),
    }
}

/// Names of the H.264 / VP9 / AV1 encoder MFTs found (diagnostics).
pub fn encoder_report() -> Vec<(&'static str, Vec<String>)> {
    let Ok(_g) = MfGuard::new() else {
        return Vec::new();
    };
    vec![
        ("H.264", encoder_names(MFVideoFormat_H264)),
        ("VP9", encoder_names(SUBTYPE_VP90)),
        ("AV1", encoder_names(SUBTYPE_AV01)),
    ]
}

#[cfg(test)]
mod caps_tests {
    use super::*;

    #[test]
    fn encoder_enumeration_finds_h264_and_is_consistent() {
        let caps = encoder_caps();
        assert!(caps.h264, "Windows ships an H.264 encoder MFT");
        let report = encoder_report();
        assert_eq!(report.len(), 3);
        // The capability flags agree with the enumerated names.
        assert_eq!(caps.h264, !report[0].1.is_empty());
        assert_eq!(caps.vp9, !report[1].1.is_empty());
        assert_eq!(caps.av1, !report[2].1.is_empty());
        assert_eq!(caps.webm(), caps.vp9 || caps.av1);
        eprintln!("encoders: {report:?}");
    }
}
