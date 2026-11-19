//! Export orchestration: decode -> composite -> encode, with progress, cancel and
//! atomic output (unique temp file in the destination folder, renamed over the
//! target on success, deleted on error/cancel).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::error::{RenderError, Result};
use crate::gifenc::GifWriter;
use crate::gpu::{Compositor, Gpu, Readback};
use crate::mf::{DecodedFrame, MfGuard, Mp4Writer, PixelFormat, VideoReader};
use crate::model::RenderProject;
use crate::text::{Overlay, TextRenderer};
use crate::{
    ExportOptions, ExportPreset, ExportStats, OutputFormat, Resolved, Stage, audio, math, scene,
};

/// Deletes the temp file unless committed.
struct TempOut {
    path: PathBuf,
    keep: bool,
}

impl Drop for TempOut {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn temp_path(out: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let stem = out
        .file_stem()
        .map_or_else(|| "export".into(), |s| s.to_string_lossy().into_owned());
    let ext = out
        .extension()
        .map_or_else(|| "mp4".into(), |s| s.to_string_lossy().into_owned());
    out.with_file_name(format!(
        ".{stem}.{}-{nanos}.partial.{ext}",
        std::process::id()
    ))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Refuse to write over any input of the project.
fn check_out_path(p: &RenderProject, out: &Path) -> Result<()> {
    let mut inputs = vec![p.screen.clone(), p.dir.join("project.json")];
    inputs.extend(p.webcam.clone());
    inputs.extend(p.mic.as_ref().map(|a| a.path.clone()));
    inputs.extend(p.system.as_ref().map(|a| a.path.clone()));
    if inputs.iter().any(|i| same_file(i, out)) {
        return Err(RenderError::Settings(format!(
            "output {} would overwrite a project input",
            out.display()
        )));
    }
    if out.is_dir() {
        return Err(RenderError::Settings(format!(
            "output {} is a folder",
            out.display()
        )));
    }
    Ok(())
}

/// Sequential frame source with look-ahead and seeking.
struct Stream {
    reader: VideoReader,
    cur: Option<DecodedFrame>,
    next: Option<DecodedFrame>,
    eof: bool,
    started: bool,
}

const TOL: i64 = 5_000; // 0.5 ms
const FAR: i64 = 20_000_000; // 2 s: seek instead of decoding through

impl Stream {
    fn new(reader: VideoReader) -> Self {
        Self {
            reader,
            cur: None,
            next: None,
            eof: false,
            started: false,
        }
    }

    fn pull(&mut self) -> Result<()> {
        self.next = if self.eof { None } else { self.reader.read()? };
        if self.next.is_none() {
            self.eof = true;
        }
        Ok(())
    }

    /// Make `cur` the frame shown at `t` (100 ns). Returns true if it changed.
    fn advance(&mut self, t: i64) -> Result<bool> {
        if !self.started {
            self.started = true;
            self.pull()?;
        }
        let before = self.cur.as_ref().map(|f| f.pts);
        if let Some(c) = &self.cur {
            let next_far = self.next.as_ref().is_none_or(|n| t > n.pts + FAR);
            if t + TOL < c.pts || (t > c.pts + FAR && next_far) {
                self.reader.seek(t)?;
                self.cur = None;
                self.eof = false;
                self.pull()?;
            }
        }
        while self.next.as_ref().is_some_and(|n| n.pts <= t + TOL) {
            self.cur = self.next.take();
            self.pull()?;
        }
        if self.cur.is_none() {
            self.cur = self.next.take();
            self.pull()?;
        }
        Ok(self.cur.as_ref().map(|f| f.pts) != before)
    }

    /// Copy the current frame into a tight NV12 buffer (rows of `pitch` bytes).
    fn copy_tight(&self) -> Result<Option<Nv12>> {
        let Some(f) = &self.cur else { return Ok(None) };
        let (w, h) = (self.reader.width as usize, self.reader.height as usize);
        self.reader
            .with_planes(f, |pl| pack_nv12(pl.plane0, pl.plane1, pl.pitch, w, h))?
            .map(Some)
            .ok_or_else(|| {
                RenderError::Media("decoded NV12 planes are shorter than the frame".into())
            })
    }
}

/// Copy NV12 planes (rows `src_pitch` apart) into a tight frame whose rows are
/// `ceil(w/2)*2` bytes. `None` if any row would fall outside the planes.
fn pack_nv12(y_src: &[u8], uv_src: &[u8], src_pitch: usize, w: usize, h: usize) -> Option<Nv12> {
    let pitch = w.div_ceil(2) * 2;
    if src_pitch < w || w == 0 || h == 0 {
        return None;
    }
    let mut y = vec![0u8; pitch * h];
    for r in 0..h {
        let s = y_src.get(r * src_pitch..r * src_pitch + w)?;
        y[r * pitch..r * pitch + w].copy_from_slice(s);
    }
    let mut uv = vec![0u8; pitch * h.div_ceil(2)];
    for r in 0..h.div_ceil(2) {
        let s = uv_src.get(r * src_pitch..r * src_pitch + pitch)?;
        uv[r * pitch..(r + 1) * pitch].copy_from_slice(s);
    }
    Some(Nv12 { y, uv, pitch })
}

/// Per-output-frame times (100 ns) on a stream whose t = 0 sits at
/// `offset_ms` on the video clock. Deterministic: depends only on the project.
fn frame_times(p: &RenderProject, fps: u32, n: u64, offset_ms: f64) -> Vec<i64> {
    (0..n)
        .map(|i| {
            ((p.source_time(i as f64 * 1000.0 / fps as f64) - offset_ms) * 10_000.0).round() as i64
        })
        .collect()
}

/// A decoded frame copied out of Media Foundation.
struct Nv12 {
    y: Vec<u8>,
    uv: Vec<u8>,
    pitch: usize,
}

/// Stream properties reported by the decoder thread.
#[derive(Clone, Copy)]
struct StreamInfo {
    width: u32,
    height: u32,
    bt709: bool,
    full_range: bool,
    hardware: bool,
}

enum Msg {
    Info(StreamInfo),
    /// One per output frame; `None` = same picture as before.
    Frame(Option<Nv12>),
    Err(RenderError),
}

/// Decoder worker thread: owns COM/MF and the source reader, walks the frame
/// schedule ahead of the compositor (bounded queue) and hands over tight NV12.
struct Decoder {
    rx: Option<std::sync::mpsc::Receiver<Msg>>,
    stop: std::sync::Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Decoder {
    fn spawn(path: PathBuf, hardware: bool, times: Vec<i64>) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Msg>(6);
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let join = std::thread::spawn(move || {
            let run = || -> Result<()> {
                let _mf = MfGuard::new()?;
                let reader = VideoReader::open(&path, PixelFormat::Nv12, hardware)?;
                let info = StreamInfo {
                    width: reader.width,
                    height: reader.height,
                    bt709: reader.bt709,
                    full_range: reader.full_range,
                    hardware: reader.hardware,
                };
                if tx.send(Msg::Info(info)).is_err() {
                    return Ok(());
                }
                let mut s = Stream::new(reader);
                for t in times {
                    if stop2.load(Ordering::Relaxed) {
                        break;
                    }
                    let changed = s.advance(t)?;
                    if (s.reader.width, s.reader.height) != (info.width, info.height) {
                        return Err(RenderError::Media(format!(
                            "video size changed mid-stream ({}x{} -> {}x{}); not supported",
                            info.width, info.height, s.reader.width, s.reader.height
                        )));
                    }
                    let f = if changed { s.copy_tight()? } else { None };
                    if tx.send(Msg::Frame(f)).is_err() {
                        break;
                    }
                }
                Ok(())
            };
            if let Err(e) = run() {
                let _ = tx.send(Msg::Err(e));
            }
        });
        Self {
            rx: Some(rx),
            stop,
            join: Some(join),
        }
    }

    fn recv(&mut self) -> Result<Msg> {
        match self.rx.as_ref().map(|r| r.recv()) {
            Some(Ok(Msg::Err(e))) => Err(e),
            Some(Ok(m)) => Ok(m),
            _ => Err(RenderError::Media("decoder thread ended early".into())),
        }
    }

    fn info(&mut self) -> Result<StreamInfo> {
        match self.recv()? {
            Msg::Info(i) => Ok(i),
            _ => Err(RenderError::Media("decoder protocol".into())),
        }
    }

    fn next(&mut self) -> Result<Option<Nv12>> {
        match self.recv()? {
            Msg::Frame(f) => Ok(f),
            _ => Err(RenderError::Media("decoder protocol".into())),
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.rx = None; // unblocks a pending send
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

enum Sink {
    Mp4(Mp4Writer),
    Gif(GifWriter),
}

enum EncMsg {
    Frame(u64, Vec<u8>),
    /// Everything was sent: finalize the container. Without it (error/cancel)
    /// the writer is dropped unfinalized and the temp file deleted.
    Finish,
}

/// Encoder worker thread: owns COM/MF and the sink writer (or the GIF writer),
/// interleaves audio ~0.5 s ahead of video.
struct Encoder {
    tx: Option<std::sync::mpsc::SyncSender<EncMsg>>,
    ready: std::sync::mpsc::Receiver<Result<bool>>,
    join: Option<std::thread::JoinHandle<Result<()>>>,
}

impl Encoder {
    fn spawn(
        path: PathBuf,
        r: Resolved,
        pcm: Option<Vec<i16>>,
        hardware: bool,
        out_ms: f64,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<EncMsg>(4);
        let (rtx, ready) = std::sync::mpsc::channel::<Result<bool>>();
        let join = std::thread::spawn(move || -> Result<()> {
            let _mf = match MfGuard::new() {
                Ok(g) => g,
                Err(e) => {
                    let _ = rtx.send(Err(e));
                    return Ok(());
                }
            };
            let created = match r.format {
                OutputFormat::Mp4 => Mp4Writer::create(
                    &path,
                    r.width,
                    r.height,
                    r.fps,
                    r.bitrate,
                    pcm.is_some(),
                    hardware,
                )
                .map(Sink::Mp4),
                OutputFormat::Gif => GifWriter::create(&path, r.width, r.height).map(Sink::Gif),
            };
            let mut sink = match created {
                Ok(s) => s,
                Err(e) => {
                    let _ = rtx.send(Err(e));
                    return Ok(());
                }
            };
            let _ = rtx.send(Ok(matches!(&sink, Sink::Mp4(m) if m.hardware)));
            let total_audio = pcm.as_ref().map_or(0, |a| (a.len() / 2) as u64);
            let fps = r.fps as u64;
            let mut finished = false;
            for msg in rx {
                let (tag, data) = match msg {
                    EncMsg::Frame(t, d) => (t, d),
                    EncMsg::Finish => {
                        finished = true;
                        break;
                    }
                };
                match &mut sink {
                    Sink::Mp4(m) => {
                        m.write_video(&data)?;
                        if let Some(a) = &pcm {
                            let rate = audio::RATE as u64;
                            let want = ((tag + 1) * rate / fps + rate / 2).min(total_audio);
                            let have = m.audio_frames();
                            if want > have {
                                m.write_audio(&a[(have * 2) as usize..(want * 2) as usize])?;
                            }
                        }
                    }
                    Sink::Gif(g) => g.push(&data, tag as f64 * 1000.0 / r.fps as f64)?,
                }
            }
            if !finished {
                return Err(RenderError::Cancelled);
            }
            match sink {
                Sink::Mp4(mut m) => {
                    if let Some(a) = &pcm {
                        let have = m.audio_frames();
                        if total_audio > have {
                            m.write_audio(&a[(have * 2) as usize..])?;
                        }
                    }
                    m.finish()
                }
                Sink::Gif(g) => g.finish(out_ms),
            }
        });
        Self {
            tx: Some(tx),
            ready,
            join: Some(join),
        }
    }

    /// Error of the worker after it stopped.
    fn worker_error(&mut self) -> RenderError {
        self.tx = None;
        match self.join.take().map(|j| j.join()) {
            Some(Ok(Err(e))) => e,
            Some(Err(_)) => RenderError::Media("encoder thread panicked".into()),
            _ => RenderError::Media("encoder thread stopped".into()),
        }
    }

    /// Wait until the writer exists; returns whether a hardware encoder is used.
    fn ready(&mut self) -> Result<bool> {
        match self.ready.recv() {
            Ok(r) => r,
            Err(_) => Err(self.worker_error()),
        }
    }

    fn send(&mut self, tag: u64, data: Vec<u8>) -> Result<()> {
        let ok = self
            .tx
            .as_ref()
            .is_some_and(|tx| tx.send(EncMsg::Frame(tag, data)).is_ok());
        if ok { Ok(()) } else { Err(self.worker_error()) }
    }

    /// Finalize the file and wait for the worker.
    fn finish(mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(EncMsg::Finish);
        }
        match self.join.take().map(|j| j.join()) {
            Some(Ok(r)) => r,
            _ => Err(RenderError::Media("encoder thread panicked".into())),
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.tx = None; // no Finish: the worker drops the writer unfinalized
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Export a project folder. See [`export_project`].
pub fn export(
    project_dir: &Path,
    preset: &ExportPreset,
    out_path: &Path,
    opts: &ExportOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64, Stage),
) -> Result<ExportStats> {
    let p = RenderProject::load(project_dir)?;
    export_project(&p, preset, out_path, opts, cancel, progress)
}

/// Bake `p` into `out_path` (MP4 or GIF per `preset.format`).
///
/// A panic inside the pipeline is turned into an error (in unwinding builds;
/// release builds use `panic = "abort"`, where the code relies on checked
/// slicing instead).
pub fn export_project(
    p: &RenderProject,
    preset: &ExportPreset,
    out_path: &Path,
    opts: &ExportOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64, Stage),
) -> Result<ExportStats> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        export_inner(p, preset, out_path, opts, cancel, progress)
    }))
    .unwrap_or_else(|panic| {
        let msg = panic
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        Err(RenderError::Media(format!(
            "internal error during export: {msg}"
        )))
    })
}

/// Real duration (ms) of the first video stream, or 0 when it cannot be read.
fn source_duration_ms(path: &Path) -> f64 {
    let mf = MfGuard::new().ok().and_then(|_g| {
        VideoReader::open(path, PixelFormat::Nv12, false)
            .ok()
            .map(|r| r.duration as f64 / 10_000.0)
    });
    // Fragmented recordings carry no duration in the moov: sum the fragments.
    mf.filter(|d| *d > 0.0)
        .or_else(|| crate::mp4info::stream_duration_ms(path))
        .unwrap_or(0.0)
}

fn export_inner(
    p: &RenderProject,
    preset: &ExportPreset,
    out_path: &Path,
    opts: &ExportOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64, Stage),
) -> Result<ExportStats> {
    let started = Instant::now();
    let trace_on = std::env::var_os("FROSTCAP_EXPORT_TRACE").is_some();
    let trace = |what: &str| {
        if trace_on {
            eprintln!("[export] {:>7.3}s {what}", started.elapsed().as_secs_f64());
        }
    };
    progress(0.0, Stage::Preparing);
    let check = || {
        if cancel.load(Ordering::Relaxed) {
            Err(RenderError::Cancelled)
        } else {
            Ok(())
        }
    };
    check_out_path(p, out_path)?;
    if !p.screen.is_file() {
        return Err(RenderError::Project(format!(
            "missing {}",
            p.screen.display()
        )));
    }
    // The decoder's real duration is the ground truth: never size frame lists or
    // audio buffers from an overstated `durationMs`.
    let real_ms = source_duration_ms(&p.screen);
    trace(&format!(
        "source duration {real_ms:.1} ms (project claims {:.1})",
        p.duration_ms
    ));
    let mut clamped = p.clone();
    clamped.clamp_to_source(real_ms);
    let p = &clamped;
    let out_ms = p.output_ms();
    if out_ms <= 0.0 {
        return Err(RenderError::Project(
            "nothing to export (empty timeline)".into(),
        ));
    }
    let r = preset.resolve(p.canvas);
    if let Some(parent) = out_path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| RenderError::io(parent, e))?;
    }
    let (w, h, fps) = (r.width, r.height, r.fps);
    let n_frames = ((out_ms * fps as f64 / 1000.0).ceil() as u64).max(1);

    // Decoders open on their own threads while audio is mixed and the GPU
    // device is created.
    let mut screen = Decoder::spawn(
        p.screen.clone(),
        opts.hardware_codecs,
        frame_times(p, fps, n_frames, 0.0),
    );
    let mut cam = match &p.webcam {
        Some(path) if p.style.webcam_enabled && path.is_file() => Some(Decoder::spawn(
            path.clone(),
            opts.hardware_codecs,
            frame_times(p, fps, n_frames, p.webcam_offset_ms),
        )),
        _ => None,
    };
    let pcm = if r.format == OutputFormat::Mp4 {
        progress(0.01, Stage::Audio);
        audio::render(p)?
    } else {
        None
    };
    check()?;
    trace("audio mixed");
    // GPU device + pipelines build on a worker thread while the decoder
    // finishes opening and the encoder is created.
    let (gpu_tx, gpu_rx) = std::sync::mpsc::channel::<((u32, u32), u32, Option<(u32, u32)>)>();
    let readback = if r.format == OutputFormat::Gif {
        Readback::Rgba
    } else {
        Readback::Nv12
    };
    let allow_sw = opts.allow_software_gpu;
    let gpu_job = std::thread::spawn(move || -> Result<Compositor> {
        let gpu = Gpu::new(allow_sw, false)?;
        let (src, levels, cam_size) = gpu_rx
            .recv()
            .map_err(|_| RenderError::Gpu("setup aborted".into()))?;
        Compositor::new(gpu, w, h, src, levels, cam_size, readback)
    });
    let screen_info = screen.info()?;
    let cam_info = match cam.as_mut() {
        Some(c) => Some(c.info()?),
        None => None,
    };
    let hardware_decode = screen_info.hardware;
    let src = (screen_info.width, screen_info.height);
    trace("decoder opened");
    // Enough mips for the unzoomed (smallest) video size.
    let lay = math::layout(w as f64, h as f64, src.0 as f64, src.1 as f64, p);
    let max_lod = (src.0 as f64 / lay.content.w.max(1.0)).log2().max(0.0);
    let levels = max_lod.ceil() as u32 + 2;
    let _ = gpu_tx.send((src, levels, cam_info.map(|c| (c.width, c.height))));

    let tmp = TempOut {
        path: temp_path(out_path),
        keep: false,
    };
    let has_audio = pcm.is_some();
    let mut enc = Encoder::spawn(tmp.path.clone(), r, pcm, opts.hardware_codecs, out_ms);
    let needs_text = (p.style.captions_enabled && !p.captions.is_empty())
        || (p.style.show_keys && !p.keys.is_empty());
    let text = if needs_text {
        TextRenderer::load_system()
    } else {
        None
    };
    let hardware_encode = enc.ready()?;
    trace("text + encoder ready");
    let mut comp = gpu_job
        .join()
        .map_err(|_| RenderError::Gpu("GPU setup thread panicked".into()))??;
    let adapter = comp.adapter().to_owned();
    let software_gpu = comp.software();
    trace("compositor (pipelines)");
    let (mut t_dec, mut t_gpu, mut t_enc) = (0.0f64, 0.0f64, 0.0f64);

    let mut cur_caption: Option<usize> = None;
    let mut caption_overlay: Option<Overlay> = None;
    let mut cur_keys = String::new();
    for i in 0..n_frames {
        check()?;
        let lap = Instant::now();
        let t_out = i as f64 * 1000.0 / fps as f64;
        let t_src = p.source_time(t_out);
        if let Some(f) = screen.next()? {
            let si = &screen_info;
            comp.upload_screen_nv12(&f.y, &f.uv, f.pitch, si.bt709, si.full_range);
        }
        let mut cam_uv = [0.0f32, 0.0, 1.0, 1.0];
        let mut cam_lod = 0.0f32;
        if let (Some(c), Some(ci)) = (cam.as_mut(), cam_info) {
            if let Some(f) = c.next()? {
                comp.upload_webcam_nv12(&f.y, &f.uv, f.pitch, ci.bt709, ci.full_range);
            }
            // Cover-fit the camera into the square bubble.
            let (cw, ch) = (ci.width as f32, ci.height as f32);
            let a = cw / ch.max(1.0);
            if a >= 1.0 {
                let u0 = (1.0 - 1.0 / a) / 2.0;
                cam_uv = [u0, 0.0, 1.0 - u0, 1.0];
            } else {
                let v0 = (1.0 - a) / 2.0;
                cam_uv = [0.0, v0, 1.0, 1.0 - v0];
            }
            let bubble = (p.style.webcam_size * h as f64) as f32;
            cam_lod = (ch.min(cw) / bubble.max(1.0)).log2().max(0.0);
        }
        let s = scene::build(p, w as f64, h as f64, t_src, cam.is_some());
        t_dec += lap.elapsed().as_secs_f64();

        if let Some(t) = &text {
            if s.caption != cur_caption {
                cur_caption = s.caption;
                caption_overlay = s.caption.and_then(|ci| {
                    let st = &p.style;
                    t.caption(
                        &p.captions[ci].text,
                        w as f64,
                        h as f64,
                        st.caption_font,
                        st.caption_top,
                        st.caption_bg,
                    )
                });
                comp.set_caption(caption_overlay.as_ref());
            }
            let key_text = s.keys.as_ref().map_or("", |k| k.text.as_str());
            if key_text != cur_keys {
                cur_keys = key_text.to_owned();
                let unit = h as f64 / 1080.0;
                let mut bottom = h as f64 - 48.0 * unit;
                if let Some(c) = caption_overlay.as_ref().filter(|_| !p.style.caption_top) {
                    bottom = bottom.min(c.y as f64 - 4.0 * unit);
                }
                let o = (!cur_keys.is_empty())
                    .then(|| t.keys(&cur_keys, w as f64, h as f64, bottom))
                    .flatten();
                comp.set_keys(o.as_ref());
            }
        }
        let keys_alpha = s.keys.as_ref().map_or(0.0, |k| k.alpha);
        let lap = Instant::now();
        let done = comp.submit(&s, i, keys_alpha, cam_uv, cam_lod)?;
        t_gpu += lap.elapsed().as_secs_f64();
        let lap = Instant::now();
        for (tag, data) in done {
            enc.send(tag, data)?;
        }
        t_enc += lap.elapsed().as_secs_f64();
        if i % 4 == 0 || i + 1 == n_frames {
            progress(
                0.03 + 0.95 * (i + 1) as f64 / n_frames as f64,
                Stage::Rendering,
            );
        }
    }
    for (tag, data) in comp.flush()? {
        enc.send(tag, data)?;
    }
    check()?;
    trace(&format!(
        "frames submitted: decode wait+upload {t_dec:.2}s, gpu submit/wait {t_gpu:.2}s, encoder queue wait {t_enc:.2}s"
    ));
    progress(0.985, Stage::Finalizing);
    enc.finish()?;
    trace("encoder finalized");
    drop(screen);
    drop(cam);
    drop(comp);
    let mut tmp = tmp;
    std::fs::rename(&tmp.path, out_path).map_err(|e| RenderError::io(out_path, e))?;
    tmp.keep = true;
    let bytes = std::fs::metadata(out_path).map(|m| m.len()).unwrap_or(0);
    let elapsed_s = started.elapsed().as_secs_f64();
    progress(1.0, Stage::Done);
    Ok(ExportStats {
        out_path: out_path.to_path_buf(),
        bytes,
        frames: n_frames,
        duration_ms: out_ms,
        elapsed_s,
        realtime_factor: (out_ms / 1000.0) / elapsed_s.max(1e-6),
        adapter,
        software_gpu,
        hardware_decode,
        hardware_encode,
        audio: has_audio,
        width: w,
        height: h,
        fps,
        format: r.format,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Piece;

    fn scratch(name: &str) -> PathBuf {
        let base = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
        let d = base
            .join("test-tmp")
            .join(format!("render-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fake_project(dir: &Path) -> RenderProject {
        std::fs::write(dir.join("screen.mp4"), b"not an mp4").unwrap();
        RenderProject {
            dir: dir.to_path_buf(),
            screen: dir.join("screen.mp4"),
            ..RenderProject::empty_for_tests()
        }
    }

    #[test]
    fn refuses_to_overwrite_inputs() {
        let d = scratch("inputs");
        let p = fake_project(&d);
        let never = AtomicBool::new(false);
        let r = export_project(
            &p,
            &ExportPreset::default(),
            &p.screen,
            &ExportOptions::default(),
            &never,
            &mut |_, _| {},
        );
        assert!(matches!(r, Err(RenderError::Settings(_))), "{r:?}");
        assert_eq!(std::fs::read(&p.screen).unwrap(), b"not an mp4");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cancel_leaves_no_files() {
        let d = scratch("cancel");
        let p = fake_project(&d);
        let out = d.join("out").join("x.mp4");
        let cancel = AtomicBool::new(true);
        let r = export_project(
            &p,
            &ExportPreset::default(),
            &out,
            &ExportOptions::default(),
            &cancel,
            &mut |_, _| {},
        );
        assert!(matches!(r, Err(RenderError::Cancelled)), "{r:?}");
        let left: Vec<_> = std::fs::read_dir(d.join("out"))
            .map(|r| r.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "{left:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn pack_nv12_copies_rows_and_rejects_short_planes() {
        // 3x2 picture, source pitch 5: Y rows "abc..", "def.."; UV row "uvUV.".
        let y = b"abc..def";
        let uv = b"uvUV";
        let n = pack_nv12(y, uv, 5, 3, 2).unwrap();
        assert_eq!(n.pitch, 4);
        assert_eq!(&n.y, b"abc\0def\0");
        assert_eq!(&n.uv, b"uvUV");
        assert!(pack_nv12(&y[..7], uv, 5, 3, 2).is_none()); // last Y row cut short
        assert!(pack_nv12(y, &uv[..3], 5, 3, 2).is_none()); // UV row cut short
        assert!(pack_nv12(y, uv, 2, 3, 2).is_none()); // pitch below width
    }

    #[test]
    fn frame_schedule_follows_cuts_speed_and_offset() {
        let mut p = RenderProject::empty_for_tests();
        p.pieces = vec![
            Piece {
                src_start: 1000.0,
                src_end: 1100.0,
                speed: 1.0,
            },
            Piece {
                src_start: 0.0,
                src_end: 100.0,
                speed: 2.0,
            },
        ];
        // 150 ms of output at 20 fps = 3 frames.
        let t = frame_times(&p, 20, 3, 0.0);
        assert_eq!(t, vec![10_000_000, 10_500_000, 0]);
        // Webcam offset shifts the clock (webcam starts 250 ms after video t=0).
        assert_eq!(frame_times(&p, 20, 1, 250.0), vec![7_500_000]);
    }
}
