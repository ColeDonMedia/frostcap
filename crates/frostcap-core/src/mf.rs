//! Media Foundation H.264 MP4 writer (`IMFSinkWriter`) shared by the screen and
//! webcam recorders. Uses the OS encoder MFT (hardware when available); no codecs
//! are bundled.
//!
//! Two input modes:
//! * [`WriterInput::D3D11Bgra`]: BGRA `ID3D11Texture2D` samples on a device that was
//!   created with `D3D11_CREATE_DEVICE_VIDEO_SUPPORT`; colour conversion and encode
//!   stay on the GPU (sink writer + DXGI device manager).
//! * [`WriterInput::Nv12`]: system-memory NV12 samples (webcam source reader output).
//!
//! The sink writer throttles `WriteSample` when the encoder falls behind, which
//! keeps memory bounded on long recordings.

use std::path::Path;

use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFAttributes, IMFDXGIDeviceManager, IMFMediaType, IMFSample, IMFSinkWriter,
    MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
    MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_SINK_WRITER_D3D_MANAGER,
    MF_TRANSCODE_CONTAINERTYPE, MFCreateAttributes, MFCreateDXGIDeviceManager,
    MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateSample, MFCreateSinkWriterFromURL,
    MFMediaType_Video, MFSTARTUP_FULL, MFShutdown, MFStartup, MFTranscodeContainerType_FMPEG4,
    MFVideoFormat_ARGB32, MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
    eAVEncH264VProfile_High,
};
use windows::core::{GUID, HSTRING, Interface};

use crate::error::{Error, Result};

/// `MF_VERSION` (MF_SDK_VERSION << 16 | MF_API_VERSION) for Windows 7+.
const MF_VERSION: u32 = 0x0002_0070;

/// RAII Media Foundation startup.
pub(crate) struct MfRuntime;
impl MfRuntime {
    pub(crate) fn new() -> Result<Self> {
        // SAFETY: balanced by MFShutdown in Drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }?;
        Ok(Self)
    }
}
impl Drop for MfRuntime {
    fn drop(&mut self) {
        // SAFETY: paired with MFStartup.
        let _ = unsafe { MFShutdown() };
    }
}

/// Pixel input of the writer.
pub enum WriterInput<'a> {
    /// GPU BGRA textures on this device.
    D3D11Bgra(&'a ID3D11Device),
    /// System-memory NV12 samples.
    Nv12,
}

/// Encoder settings.
#[derive(Clone, Copy, Debug)]
pub struct WriterConfig {
    /// Width (even).
    pub width: u32,
    /// Height (even).
    pub height: u32,
    /// Nominal frame rate.
    pub fps: u32,
    /// Average bitrate (bits/s).
    pub bitrate: u32,
}

impl WriterConfig {
    /// Default bitrate heuristic (~0.08 bits/pixel/frame, clamped 2-60 Mbps).
    pub fn with_auto_bitrate(width: u32, height: u32, fps: u32) -> Self {
        let bitrate = (u64::from(width) * u64::from(height) * u64::from(fps) / 12)
            .clamp(2_000_000, 60_000_000) as u32;
        Self {
            width,
            height,
            fps,
            bitrate,
        }
    }
    /// Nominal frame duration in 100 ns units.
    pub fn frame_hns(&self) -> i64 {
        10_000_000 / i64::from(self.fps.max(1))
    }
}

/// An open MP4 file receiving H.264 video.
pub struct H264Writer {
    writer: IMFSinkWriter,
    stream: u32,
    cfg: WriterConfig,
    _manager: Option<IMFDXGIDeviceManager>,
    finished: bool,
    samples: u64,
    last_time: Option<i64>,
    /// Latest sample, written once the next one tells its real duration.
    pending: Option<(IMFSample, i64, i64)>,
    _rt: MfRuntime,
}

// SAFETY: the sink writer is free-threaded (MTA); all access goes through &mut self.
unsafe impl Send for H264Writer {}

fn pack(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

fn video_type(subtype: &GUID, cfg: &WriterConfig) -> Result<IMFMediaType> {
    // SAFETY: plain attribute setters on a fresh media type.
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(cfg.width, cfg.height))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(cfg.fps, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        Ok(t)
    }
}

impl H264Writer {
    /// Create `path` (overwritten) and prepare the encoder.
    pub fn create(path: &Path, cfg: WriterConfig, input: WriterInput<'_>) -> Result<Self> {
        if cfg.width < 2 || cfg.height < 2 || cfg.width % 2 == 1 || cfg.height % 2 == 1 {
            return Err(Error::Invalid(format!(
                "encoder size must be even, got {}x{}",
                cfg.width, cfg.height
            )));
        }
        let rt = MfRuntime::new()?;
        // SAFETY: standard sink writer setup.
        unsafe {
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 4)?;
            let attrs = attrs.ok_or_else(|| Error::Capture("MFCreateAttributes".into()))?;
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            attrs.SetGUID(
                &MF_TRANSCODE_CONTAINERTYPE,
                &MFTranscodeContainerType_FMPEG4,
            )?;
            let mut manager = None;
            let in_subtype = match input {
                WriterInput::D3D11Bgra(device) => {
                    let mut token = 0u32;
                    let mut m: Option<IMFDXGIDeviceManager> = None;
                    MFCreateDXGIDeviceManager(&mut token, &mut m)?;
                    let m = m.ok_or_else(|| Error::Capture("MFCreateDXGIDeviceManager".into()))?;
                    m.ResetDevice(device, token)?;
                    attrs.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &m)?;
                    manager = Some(m);
                    MFVideoFormat_ARGB32
                }
                WriterInput::Nv12 => MFVideoFormat_NV12,
            };
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attrs)?;

            let out = video_type(&MFVideoFormat_H264, &cfg)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, cfg.bitrate)?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            let stream = writer.AddStream(&out)?;
            let inp = video_type(&in_subtype, &cfg)?;
            writer.SetInputMediaType(stream, &inp, None)?;
            writer.BeginWriting()?;
            Ok(Self {
                writer,
                stream,
                cfg,
                _manager: manager,
                finished: false,
                samples: 0,
                last_time: None,
                pending: None,
                _rt: rt,
            })
        }
    }

    /// Encoder configuration.
    pub fn config(&self) -> WriterConfig {
        self.cfg
    }

    /// Samples written so far.
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Presentation time of the last written sample (hns).
    pub fn last_time(&self) -> Option<i64> {
        self.last_time
    }

    /// Write a BGRA texture (exactly `width x height`) at `time` (hns, >= previous).
    pub fn write_texture(&mut self, tex: &ID3D11Texture2D, time: i64, duration: i64) -> Result<()> {
        // SAFETY: wraps our own texture in an MF buffer; MF keeps its own reference.
        let sample = unsafe {
            let buf = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, tex, 0, false)?;
            let len = buf.cast::<IMF2DBuffer>()?.GetContiguousLength()?;
            buf.SetCurrentLength(len)?;
            let s = MFCreateSample()?;
            s.AddBuffer(&buf)?;
            s
        };
        self.write_sample(&sample, time, duration)
    }

    /// Write an already-filled sample (e.g. NV12 from a source reader), re-stamped.
    pub fn write_sample(&mut self, sample: &IMFSample, time: i64, duration: i64) -> Result<()> {
        if self.finished {
            return Err(Error::Invalid("writer already finalized".into()));
        }
        let time = match self.last_time {
            Some(last) if time <= last => last + 1,
            _ => time.max(0),
        };
        // Variable frame rate: a sample lasts until the next one, so it is written
        // one step late with its real duration (the last one uses `duration`).
        if let Some((prev, prev_time, _)) = self.pending.take() {
            self.emit(&prev, prev_time, time - prev_time)?;
        }
        self.pending = Some((sample.clone(), time, duration));
        self.samples += 1;
        self.last_time = Some(time);
        Ok(())
    }

    fn emit(&mut self, sample: &IMFSample, time: i64, duration: i64) -> Result<()> {
        // SAFETY: plain COM calls on a sample we own.
        unsafe {
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(duration.max(1))?;
            self.writer.WriteSample(self.stream, sample)?;
        }
        Ok(())
    }

    /// Flush the encoder and write the MP4 index. Idempotent.
    pub fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        if self.samples == 0 {
            // Finalize fails with no samples; the caller deletes the empty file.
            return Err(Error::Capture("no samples were written".into()));
        }
        if let Some((s, t, d)) = self.pending.take() {
            self.emit(&s, t, d)?;
        }
        // SAFETY: finalize once.
        unsafe { self.writer.Finalize() }?;
        Ok(())
    }
}

impl Drop for H264Writer {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Media::MediaFoundation::MFCreateMemoryBuffer;

    fn nv12_sample(w: u32, h: u32, luma: u8) -> IMFSample {
        let len = w * h * 3 / 2;
        // SAFETY: fill a buffer we own, then wrap it in a sample.
        unsafe {
            let buf = MFCreateMemoryBuffer(len).unwrap();
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None).unwrap();
            std::ptr::write_bytes(p, luma, (w * h) as usize);
            std::ptr::write_bytes(p.add((w * h) as usize), 128, (len - w * h) as usize);
            buf.Unlock().unwrap();
            buf.SetCurrentLength(len).unwrap();
            let s = MFCreateSample().unwrap();
            s.AddBuffer(&buf).unwrap();
            s
        }
    }

    /// Variable frame rate (WGC only delivers frames on change): each sample must
    /// last until the next one, so a static screen still has the right duration.
    #[test]
    #[ignore = "needs a Media Foundation H.264 encoder"]
    fn sparse_samples_keep_their_real_durations() {
        crate::com::ensure_mta();
        let path = crate::test_tmp_dir().join(format!("vfr-{}.mp4", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let cfg = WriterConfig::with_auto_bitrate(320, 240, 30);
        let mut w = H264Writer::create(&path, cfg, WriterInput::Nv12).unwrap();
        for (i, t) in [0i64, 10_000_000, 20_000_000].iter().enumerate() {
            w.write_sample(
                &nv12_sample(320, 240, 40 * i as u8 + 20),
                *t,
                cfg.frame_hns(),
            )
            .unwrap();
        }
        w.finish().unwrap();
        let info = crate::mp4::inspect(&path).unwrap();
        assert!((2_000..2_100).contains(&info.duration_ms), "{info:?}");
    }

    /// A writer that is never finalized (process killed) must still leave a
    /// playable file: fragmented MP4 writes the init segment and each fragment
    /// as it goes.
    #[test]
    #[ignore = "needs a Media Foundation H.264 encoder"]
    fn unfinalized_file_is_playable() {
        crate::com::ensure_mta();
        let path = crate::test_tmp_dir().join(format!("crash-{}.mp4", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let cfg = WriterConfig::with_auto_bitrate(320, 240, 30);
        let mut w = H264Writer::create(&path, cfg, WriterInput::Nv12).unwrap();
        for i in 0..90i64 {
            let s = nv12_sample(320, 240, (i % 200) as u8 + 20);
            w.write_sample(&s, i * cfg.frame_hns(), cfg.frame_hns())
                .unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(1500));
        std::mem::forget(w); // simulate a crash: no Finalize
        let info = crate::mp4::inspect(&path).expect("parsable");
        assert!(info.playable(), "{info:?}");
        assert!(info.duration_ms >= 1000, "{info:?}");
    }
}
