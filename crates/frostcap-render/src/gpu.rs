//! wgpu (DX12 only) compositor: NV12 upload -> RGB + mips -> one composite pass
//! (background, shadow, window frame, zoomed video, ripples, cursor, webcam,
//! captions, keys) -> NV12 (BT.709 limited) or RGBA readback, pipelined.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Wake, Waker};

use crate::error::{RenderError, Result};
use crate::model::Frame;
use crate::scene::{MAX_ANNS, MAX_CURSORS, MAX_RIPPLES, MAX_STOPS, Scene};
use crate::text::Overlay;

struct ThreadWaker(std::thread::Thread);
impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Minimal executor for wgpu's native futures (they resolve immediately).
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park();
    }
}

/// Records the first uncaptured wgpu error. wgpu's default handler panics, which
/// in a `panic = "abort"` release build would kill the whole app (and any
/// recording running in it); we record it and fail the export instead.
#[derive(Clone, Default)]
pub struct ErrorSink(Arc<std::sync::Mutex<Option<String>>>);

impl ErrorSink {
    /// Keep `msg` unless an earlier error is already stored.
    pub fn record(&self, msg: String) {
        let mut g = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        g.get_or_insert(msg);
    }

    /// `Err` (sticky) once any error was recorded.
    pub fn check(&self) -> Result<()> {
        let g = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match g.as_ref() {
            Some(m) => Err(RenderError::Gpu(m.clone())),
            None => Ok(()),
        }
    }
}

/// Reject texture sizes the device cannot create (before wgpu would panic).
pub fn check_dims(what: &str, (w, h): (u32, u32), max: u32) -> Result<()> {
    if w == 0 || h == 0 || w > max || h > max {
        return Err(RenderError::Gpu(format!(
            "{what} size {w}x{h} is outside 1..={max} (GPU texture limit)"
        )));
    }
    Ok(())
}

/// Shader module, pipelines and sampler: built once per device.
pub struct Pipelines {
    sampler: wgpu::Sampler,
    p_conv: wgpu::RenderPipeline,
    p_down: wgpu::RenderPipeline,
    p_comp: wgpu::RenderPipeline,
    p_y: wgpu::RenderPipeline,
    p_uv: wgpu::RenderPipeline,
}

/// GPU device. Cheap to clone (the device, queue and pipelines are shared).
#[derive(Clone)]
pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    errors: ErrorSink,
    pipes: Arc<std::sync::OnceLock<Pipelines>>,
    /// Adapter name.
    pub adapter: String,
    /// True when running on the WARP software rasterizer.
    pub software: bool,
    /// Policy this device was created under (cache key).
    policy: (bool, bool),
}

static SHARED: std::sync::Mutex<Option<Gpu>> = std::sync::Mutex::new(None);

/// Serialises DX12 device creation. Creating devices on several threads at once
/// crashed the process (access violation in the D3D12/WARP runtime). Only
/// creation is locked; dropping a device takes no lock.
static DEVICE_LIFECYCLE: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl ErrorSink {
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Gpu {
    /// A process-wide cached device (created on first use, replaced once it has
    /// recorded an error). Reusing it skips adapter/device creation and shader
    /// compilation on every export after the first. `FROSTCAP_NO_GPU_CACHE=1`
    /// disables the cache (A/B measurements).
    pub fn shared(allow_software: bool, force_software: bool) -> Result<Self> {
        if std::env::var_os("FROSTCAP_NO_GPU_CACHE").is_some_and(|v| v == "1") {
            return Self::new(allow_software, force_software);
        }
        let mut g = SHARED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(c) = g.as_ref()
            && c.errors.check().is_ok()
            && c.policy == (allow_software, force_software)
        {
            return Ok(c.clone());
        }
        let fresh = Self::new(allow_software, force_software)?;
        *g = Some(fresh.clone());
        Ok(fresh)
    }

    /// Drop the cached device (frees GPU memory when the app is idle).
    pub fn release_shared() {
        *SHARED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Whether both handles use the same underlying device.
    pub fn same_device(&self, other: &Self) -> bool {
        self.errors.same(&other.errors)
    }

    /// Record an uncaptured-style error (tests / device-lost handling).
    pub fn record_error(&self, msg: String) {
        self.errors.record(msg);
    }

    /// No error has been recorded on this device.
    pub fn errors_ok(&self) -> bool {
        self.errors.check().is_ok()
    }

    /// The shared pipelines (built on first call).
    pub fn pipelines(&self) -> &Pipelines {
        self.pipes.get_or_init(|| self.build_pipelines())
    }

    fn build_pipelines(&self) -> Pipelines {
        let d = &self.device;
        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("frostcap shaders"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders.wgsl").into()),
        });
        let pipe = |fs: &str, fmt: wgpu::TextureFormat| {
            d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(fs),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_full"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: fmt,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        use wgpu::TextureFormat as F;
        Pipelines {
            p_conv: pipe("fs_nv12", F::Rgba8Unorm),
            p_down: pipe("fs_down", F::Rgba8Unorm),
            p_comp: pipe("fs_composite", F::Rgba8Unorm),
            p_y: pipe("fs_y", F::R8Unorm),
            p_uv: pipe("fs_uv", F::Rg8Unorm),
            sampler: d.create_sampler(&wgpu::SamplerDescriptor {
                label: None,
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            }),
        }
    }
}

impl Gpu {
    /// Create a DX12 device. Tries a hardware adapter first; falls back to the
    /// WARP software adapter when `allow_software` (or forced via
    /// `FROSTCAP_FORCE_WARP=1` / `force_software`).
    pub fn new(allow_software: bool, force_software: bool) -> Result<Self> {
        let _lifecycle = DEVICE_LIFECYCLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let force =
            force_software || std::env::var_os("FROSTCAP_FORCE_WARP").is_some_and(|v| v == "1");
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::DX12;
        desc.flags = wgpu::InstanceFlags::empty();
        desc.backend_options.dx12.shader_compiler = wgpu::Dx12Compiler::Fxc;
        let instance = wgpu::Instance::new(desc);
        let req = |fallback: bool| {
            block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: fallback,
                compatible_surface: None,
                apply_limit_buckets: false,
            }))
        };
        let adapter = if force {
            req(true)
        } else {
            match req(false) {
                Ok(a) if a.get_info().device_type != wgpu::DeviceType::Cpu => Ok(a),
                Ok(a) if allow_software => Ok(a),
                _ if allow_software => req(true),
                Ok(_) => Err(RenderError::Gpu(
                    "only a software adapter is available".into(),
                ))?,
                Err(e) => Err(RenderError::Gpu(format!("no DX12 adapter: {e}")))?,
            }
        }
        .map_err(|e| RenderError::Gpu(format!("no DX12 adapter: {e}")))?;
        let info = adapter.get_info();
        let limits = wgpu::Limits {
            max_texture_dimension_2d: adapter.limits().max_texture_dimension_2d.min(16384),
            ..wgpu::Limits::downlevel_defaults()
        };
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("frostcap-render"),
            required_limits: limits,
            ..Default::default()
        }))
        .map_err(|e| RenderError::Gpu(format!("request_device: {e}")))?;
        let errors = ErrorSink::default();
        {
            let sink = errors.clone();
            device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| sink.record(e.to_string())));
        }
        Ok(Self {
            device,
            queue,
            errors,
            pipes: Arc::new(std::sync::OnceLock::new()),
            adapter: format!("{} ({:?})", info.name, info.device_type),
            software: info.device_type == wgpu::DeviceType::Cpu,
            policy: (allow_software, force_software),
        })
    }
}

/// What the compositor reads back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readback {
    /// Tight NV12 (for the H.264 encoder).
    Nv12,
    /// Tight RGBA8 (GIF, tests).
    Rgba,
}

/// A video input: NV12 planes -> RGBA texture with a mip chain.
struct VideoTex {
    w: u32,
    h: u32,
    y: wgpu::Texture,
    uv: wgpu::Texture,
    rgba: wgpu::Texture,
    levels: u32,
    conv_bg: wgpu::BindGroup,
    conv_u: wgpu::Buffer,
    mip_views: Vec<wgpu::TextureView>,
    mip_bgs: Vec<wgpu::BindGroup>,
    full_view: wgpu::TextureView,
    dirty_nv12: bool,
    dirty_mips: bool,
}

struct Slot {
    tag: u64,
    bufs: Vec<wgpu::Buffer>,
    ready: Arc<AtomicBool>,
    submission: wgpu::SubmissionIndex,
}

/// Per-export compositor.
pub struct Compositor {
    gpu: Gpu,
    w: u32,
    h: u32,
    readback: Readback,
    sampler: wgpu::Sampler,
    p_conv: wgpu::RenderPipeline,
    p_down: wgpu::RenderPipeline,
    p_comp: wgpu::RenderPipeline,
    p_y: wgpu::RenderPipeline,
    p_uv: wgpu::RenderPipeline,
    screen: VideoTex,
    cam: Option<VideoTex>,
    blank: wgpu::Texture,
    cap_tex: Option<(wgpu::Texture, [f32; 4])>,
    keys_tex: Option<(wgpu::Texture, [f32; 4])>,
    comp_bg: Option<wgpu::BindGroup>,
    bg_tex: Option<wgpu::Texture>,
    ann_tex: Option<wgpu::Texture>,
    cambg_tex: Option<wgpu::Texture>,
    uniforms: wgpu::Buffer,
    out: wgpu::Texture,
    y_out: wgpu::Texture,
    uv_out: wgpu::Texture,
    e_y_bg: wgpu::BindGroup,
    e_uv_bg: wgpu::BindGroup,
    free: Vec<Vec<wgpu::Buffer>>,
    pending: VecDeque<Slot>,
    ring: usize,
}

const UNIFORM_VEC4S: usize = 77;

fn align256(x: u32) -> u32 {
    x.div_ceil(256) * 256
}

fn tex(
    device: &wgpu::Device,
    w: u32,
    h: u32,
    fmt: wgpu::TextureFormat,
    levels: u32,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: fmt,
        usage,
        view_formats: &[],
    })
}

fn view(t: &wgpu::Texture) -> wgpu::TextureView {
    t.create_view(&wgpu::TextureViewDescriptor::default())
}

fn mip_view(t: &wgpu::Texture, level: u32) -> wgpu::TextureView {
    t.create_view(&wgpu::TextureViewDescriptor {
        base_mip_level: level,
        mip_level_count: Some(1),
        ..Default::default()
    })
}

fn pass<'a>(
    enc: &'a mut wgpu::CommandEncoder,
    target: &'a wgpu::TextureView,
) -> wgpu::RenderPass<'a> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

impl Compositor {
    /// `levels`: mip levels for the screen texture (1 = no mips).
    pub fn new(
        gpu: Gpu,
        out_w: u32,
        out_h: u32,
        src: (u32, u32),
        screen_levels: u32,
        cam: Option<(u32, u32)>,
        readback: Readback,
    ) -> Result<Self> {
        let max = gpu.device.limits().max_texture_dimension_2d;
        check_dims("output", (out_w, out_h), max)?;
        check_dims("screen source", src, max)?;
        if let Some(c) = cam {
            check_dims("webcam source", c, max)?;
        }
        let d = &gpu.device;
        use wgpu::TextureFormat as F;
        let pl = gpu.pipelines();
        let (p_conv, p_down, p_comp, p_y, p_uv, sampler) = (
            pl.p_conv.clone(),
            pl.p_down.clone(),
            pl.p_comp.clone(),
            pl.p_y.clone(),
            pl.p_uv.clone(),
            pl.sampler.clone(),
        );
        let video_tex = |w: u32, h: u32, levels: u32| -> VideoTex {
            use wgpu::TextureUsages as U;
            let full = 32 - w.max(h).max(1).leading_zeros();
            let levels = levels.clamp(1, full);
            let y = tex(d, w, h, F::R8Unorm, 1, U::TEXTURE_BINDING | U::COPY_DST);
            let uv = tex(
                d,
                w.div_ceil(2),
                h.div_ceil(2),
                F::Rg8Unorm,
                1,
                U::TEXTURE_BINDING | U::COPY_DST,
            );
            let rgba = tex(
                d,
                w,
                h,
                F::Rgba8Unorm,
                levels,
                U::TEXTURE_BINDING | U::RENDER_ATTACHMENT | U::COPY_DST,
            );
            let conv_u = d.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let conv_bg = d.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &p_conv.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: conv_u.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&view(&y)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&view(&uv)),
                    },
                ],
            });
            let mip_views: Vec<_> = (0..levels).map(|l| mip_view(&rgba, l)).collect();
            let mip_bgs = (1..levels as usize)
                .map(|l| {
                    d.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: None,
                        layout: &p_down.get_bind_group_layout(0),
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::Sampler(&sampler),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(&mip_views[l - 1]),
                            },
                        ],
                    })
                })
                .collect();
            VideoTex {
                w,
                h,
                full_view: view(&rgba),
                y,
                uv,
                rgba,
                levels,
                conv_bg,
                conv_u,
                mip_views,
                mip_bgs,
                dirty_nv12: false,
                dirty_mips: false,
            }
        };
        let screen = video_tex(src.0, src.1, screen_levels);
        let cam = cam.map(|(w, h)| video_tex(w, h, 16));
        use wgpu::TextureUsages as U;
        let blank = tex(d, 1, 1, F::Rgba8Unorm, 1, U::TEXTURE_BINDING | U::COPY_DST);
        gpu.queue.write_texture(
            blank.as_image_copy(),
            &[0, 0, 0, 0],
            wgpu::TexelCopyBufferLayout::default(),
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let uniforms = d.create_buffer(&wgpu::BufferDescriptor {
            label: Some("composite uniforms"),
            size: (UNIFORM_VEC4S * 16) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let out = tex(
            d,
            out_w,
            out_h,
            F::Rgba8Unorm,
            1,
            U::RENDER_ATTACHMENT | U::TEXTURE_BINDING | U::COPY_SRC,
        );
        let y_out = tex(
            d,
            out_w,
            out_h,
            F::R8Unorm,
            1,
            U::RENDER_ATTACHMENT | U::COPY_SRC,
        );
        let uv_out = tex(
            d,
            out_w / 2,
            out_h / 2,
            F::Rg8Unorm,
            1,
            U::RENDER_ATTACHMENT | U::COPY_SRC,
        );
        // Auto-derived layouts are exclusive to their pipeline: one group each.
        let out_bg = |p: &wgpu::RenderPipeline| {
            d.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &p.get_bind_group_layout(0),
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view(&out)),
                }],
            })
        };
        let e_y_bg = out_bg(&p_y);
        let e_uv_bg = out_bg(&p_uv);
        let ring = if gpu.software { 1 } else { 3 };
        gpu.errors.check()?;
        Ok(Self {
            gpu,
            w: out_w,
            h: out_h,
            readback,
            sampler,
            p_conv,
            p_down,
            p_comp,
            p_y,
            p_uv,
            screen,
            cam,
            blank,
            cap_tex: None,
            keys_tex: None,
            comp_bg: None,
            bg_tex: None,
            ann_tex: None,
            cambg_tex: None,
            uniforms,
            out,
            y_out,
            uv_out,
            e_y_bg,
            e_uv_bg,
            free: Vec::new(),
            pending: VecDeque::new(),
            ring,
        })
    }

    /// Adapter description.
    pub fn adapter(&self) -> &str {
        &self.gpu.adapter
    }

    /// Running on WARP.
    pub fn software(&self) -> bool {
        self.gpu.software
    }

    fn upload_nv12_into(
        q: &wgpu::Queue,
        v: &mut VideoTex,
        y: &[u8],
        uv: &[u8],
        pitch: usize,
        bt709: bool,
        full: bool,
    ) {
        let (w, h) = (v.w, v.h);
        let ext = |w, h| wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };
        let layout = wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(pitch as u32),
            rows_per_image: None,
        };
        let need_y = pitch * (h as usize - 1) + w as usize;
        q.write_texture(
            v.y.as_image_copy(),
            &y[..need_y.min(y.len())],
            layout,
            ext(w, h),
        );
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let need_uv = pitch * (ch as usize - 1) + cw as usize * 2;
        if uv.len() >= need_uv {
            q.write_texture(v.uv.as_image_copy(), &uv[..need_uv], layout, ext(cw, ch));
        }
        let p = [
            if bt709 { 1.0f32 } else { 0.0 },
            if full { 1.0 } else { 0.0 },
            0.0,
            0.0,
        ];
        q.write_buffer(&v.conv_u, 0, &f32_bytes(&p));
        v.dirty_nv12 = true;
        v.dirty_mips = true;
    }

    /// Upload the current screen frame (NV12 planes, `pitch` bytes per row).
    pub fn upload_screen_nv12(
        &mut self,
        y: &[u8],
        uv: &[u8],
        pitch: usize,
        bt709: bool,
        full: bool,
    ) {
        Self::upload_nv12_into(&self.gpu.queue, &mut self.screen, y, uv, pitch, bt709, full);
    }

    /// Upload the current webcam frame.
    pub fn upload_webcam_nv12(
        &mut self,
        y: &[u8],
        uv: &[u8],
        pitch: usize,
        bt709: bool,
        full: bool,
    ) {
        if let Some(c) = self.cam.as_mut() {
            Self::upload_nv12_into(&self.gpu.queue, c, y, uv, pitch, bt709, full);
        }
    }

    /// Upload a tight RGBA screen frame (tests / synthetic input).
    pub fn upload_screen_rgba(&mut self, rgba: &[u8]) {
        let v = &mut self.screen;
        self.gpu.queue.write_texture(
            v.rgba.as_image_copy(),
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(v.w * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: v.w,
                height: v.h,
                depth_or_array_layers: 1,
            },
        );
        v.dirty_nv12 = false;
        v.dirty_mips = true;
    }

    /// Upload a tight RGBA webcam frame (tests / synthetic input).
    pub fn upload_webcam_rgba(&mut self, rgba: &[u8]) {
        if let Some(v) = self.cam.as_mut() {
            self.gpu.queue.write_texture(
                v.rgba.as_image_copy(),
                rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(v.w * 4),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: v.w,
                    height: v.h,
                    depth_or_array_layers: 1,
                },
            );
            v.dirty_nv12 = false;
            v.dirty_mips = true;
        }
    }

    fn rgba_tex(&self, b: &[u8], w: u32, h: u32) -> wgpu::Texture {
        let t = tex(
            &self.gpu.device,
            w,
            h,
            wgpu::TextureFormat::Rgba8Unorm,
            1,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        self.gpu.queue.write_texture(
            t.as_image_copy(),
            b,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        t
    }

    /// Set (or clear) the annotation text layer: premultiplied RGBA8 of exactly
    /// the output size, composited inside the screen content.
    pub fn set_annotation_text(&mut self, rgba: Option<&[u8]>) {
        self.comp_bg = None;
        self.ann_tex = rgba
            .filter(|b| b.len() == (self.w * self.h * 4) as usize)
            .map(|b| self.rgba_tex(b, self.w, self.h));
    }

    /// Set (or clear) the webcam background image: premultiplied RGBA8, `side x side`.
    pub fn set_webcam_background(&mut self, img: Option<(&[u8], u32)>) {
        self.comp_bg = None;
        self.cambg_tex = img
            .filter(|(b, n)| *n > 0 && b.len() == (*n * *n * 4) as usize)
            .map(|(b, n)| self.rgba_tex(b, n, n));
    }

    /// Set (or clear) the background image: premultiplied RGBA8 of exactly the
    /// output size (see `background::compose`), drawn over the gradient.
    pub fn set_background(&mut self, rgba: Option<&[u8]>) {
        self.comp_bg = None;
        self.bg_tex = rgba
            .filter(|b| b.len() == (self.w * self.h * 4) as usize)
            .map(|b| {
                let t = tex(
                    &self.gpu.device,
                    self.w,
                    self.h,
                    wgpu::TextureFormat::Rgba8Unorm,
                    1,
                    wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                );
                self.gpu.queue.write_texture(
                    t.as_image_copy(),
                    b,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(self.w * 4),
                        rows_per_image: None,
                    },
                    wgpu::Extent3d {
                        width: self.w,
                        height: self.h,
                        depth_or_array_layers: 1,
                    },
                );
                t
            });
    }

    fn overlay_tex(&self, o: &Overlay) -> (wgpu::Texture, [f32; 4]) {
        use wgpu::TextureUsages as U;
        let t = tex(
            &self.gpu.device,
            o.w,
            o.h,
            wgpu::TextureFormat::Rgba8Unorm,
            1,
            U::TEXTURE_BINDING | U::COPY_DST,
        );
        self.gpu.queue.write_texture(
            t.as_image_copy(),
            &o.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(o.w * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: o.w,
                height: o.h,
                depth_or_array_layers: 1,
            },
        );
        let r = [
            o.x as f32,
            o.y as f32,
            (o.x + o.w as i32) as f32,
            (o.y + o.h as i32) as f32,
        ];
        (t, r)
    }

    /// Set (or clear) the caption sprite.
    pub fn set_caption(&mut self, o: Option<&Overlay>) {
        self.cap_tex = o.map(|o| self.overlay_tex(o));
        self.comp_bg = None;
    }

    /// Set (or clear) the key-overlay sprite.
    pub fn set_keys(&mut self, o: Option<&Overlay>) {
        self.keys_tex = o.map(|o| self.overlay_tex(o));
        self.comp_bg = None;
    }

    fn uniforms(&self, s: &Scene, keys_alpha: f64, cam_uv: [f32; 4], cam_lod: f32) -> Vec<f32> {
        let mut u: Vec<f32> = Vec::with_capacity(UNIFORM_VEC4S * 4);
        let mut v4 =
            |a: f64, b: f64, c: f64, d: f64| u.extend([a as f32, b as f32, c as f32, d as f32]);
        v4(s.w, s.h, s.unit_s, s.video_lod);
        v4(s.grad_line.0, s.grad_line.1, s.grad_line.2, s.grad_line.3);
        v4(s.stops.len() as f64, 0.0, 0.0, 0.0);
        for i in 0..MAX_STOPS {
            let c = s.stops.get(i).copied().unwrap_or([0.0, 0.0, 0.0, 1.0]);
            v4(c[0], c[1], c[2], c[3]);
        }
        match &s.shadow {
            Some(sh) => {
                v4(sh.rect.x, sh.rect.y, sh.rect.x1(), sh.rect.y1());
                v4(sh.radius, sh.sigma, sh.alpha, 1.0);
            }
            None => {
                v4(0.0, 0.0, 0.0, 0.0);
                v4(0.0, 1.0, 0.0, 0.0);
            }
        }
        v4(s.window.x, s.window.y, s.window.x1(), s.window.y1());
        let kind = match s.frame {
            Frame::None => 0.0,
            Frame::Minimal => 1.0,
            Frame::Windows => 2.0,
            Frame::MacOs => 3.0,
            Frame::Laptop => 4.0,
            Frame::Phone => 5.0,
        };
        v4(s.radius, s.bar_h, kind, s.bezel);
        v4(s.video.x, s.video.y, s.video.x1(), s.video.y1());
        v4(
            s.ripples.len().min(MAX_RIPPLES) as f64,
            s.ripple_lw,
            0.0,
            0.0,
        );
        for i in 0..MAX_RIPPLES {
            let r = s.ripples.get(i).copied().unwrap_or([0.0; 4]);
            v4(r[0], r[1], r[2], r[3]);
        }
        v4(
            s.cursors.len().min(MAX_CURSORS) as f64,
            s.cursor_scale.max(1e-3),
            0.0,
            0.0,
        );
        for i in 0..MAX_CURSORS {
            let c = s.cursors.get(i).copied().unwrap_or([0.0; 3]);
            v4(c[0], c[1], c[2], 0.0);
        }
        match (&s.webcam, &self.cam) {
            (Some(w), Some(_)) => {
                v4(w.rect.x, w.rect.y, w.rect.x1(), w.rect.y1());
                v4(w.radius, w.shadow_sigma.max(0.01), 1.0, cam_lod as f64);
            }
            _ => {
                v4(0.0, 0.0, 0.0, 0.0);
                v4(0.0, 1.0, 0.0, 0.0);
            }
        }
        v4(
            cam_uv[0] as f64,
            cam_uv[1] as f64,
            cam_uv[2] as f64,
            cam_uv[3] as f64,
        );
        let r = self.cap_tex.as_ref().map_or([0.0; 4], |c| c.1);
        v4(r[0] as f64, r[1] as f64, r[2] as f64, r[3] as f64);
        let r = self.keys_tex.as_ref().map_or([0.0; 4], |c| c.1);
        v4(r[0] as f64, r[1] as f64, r[2] as f64, r[3] as f64);
        v4(keys_alpha, 0.0, 0.0, 0.0);
        let border = s.webcam.as_ref().map_or(0.0, |w| w.border);
        let wc = s.webcam;
        v4(
            s.base_h,
            s.base_overhang,
            border,
            if self.bg_tex.is_some() { 1.0 } else { 0.0 },
        );
        let bc = s.webcam.as_ref().map_or([0.0; 4], |w| w.border_color);
        v4(bc[0], bc[1], bc[2], bc[3]);
        match wc {
            Some(w) => {
                v4(w.fx[0], w.fx[1], w.fx[2], w.fx[3]);
                let k = w.key.unwrap_or(crate::model::Key {
                    color: [0.0; 3],
                    similarity: 0.0,
                    smoothness: 0.0,
                    spill: 0.0,
                });
                v4(k.color[0], k.color[1], k.color[2], k.similarity);
                v4(
                    k.smoothness,
                    k.spill,
                    if w.key.is_some() { 1.0 } else { 0.0 },
                    if w.mirror { 1.0 } else { 0.0 },
                );
                v4(w.bg_kind, w.bg_blur, w.bg_angle.to_radians(), 0.0);
                v4(w.bg_c1[0], w.bg_c1[1], w.bg_c1[2], w.bg_c1[3]);
                v4(w.bg_c2[0], w.bg_c2[1], w.bg_c2[2], w.bg_c2[3]);
            }
            None => {
                v4(0.0, 1.0, 1.0, 0.4);
                for _ in 0..5 {
                    v4(0.0, 0.0, 0.0, 0.0);
                }
            }
        }
        v4(
            s.annotations.len().min(MAX_ANNS) as f64,
            if self.ann_tex.is_some() { 1.0 } else { 0.0 },
            0.0,
            0.0,
        );
        for i in 0..MAX_ANNS {
            match s.annotations.get(i) {
                Some(a) => {
                    v4(a.geom[0], a.geom[1], a.geom[2], a.geom[3]);
                    v4(a.color[0], a.color[1], a.color[2], a.alpha);
                    let kind = match a.kind {
                        crate::model::AnnKind::Arrow => 0.0,
                        crate::model::AnnKind::Text => 1.0,
                        crate::model::AnnKind::Rect => 2.0,
                        crate::model::AnnKind::Ellipse => 3.0,
                        crate::model::AnnKind::Highlight => 4.0,
                        crate::model::AnnKind::Spotlight => 5.0,
                        crate::model::AnnKind::Blur => 6.0,
                        crate::model::AnnKind::Pixelate => 7.0,
                    };
                    v4(kind, a.stroke, if a.fill { 1.0 } else { 0.0 }, a.radius);
                    v4(a.strength, a.dim, 0.0, 0.0);
                }
                None => {
                    for _ in 0..4 {
                        v4(0.0, 0.0, 0.0, 0.0);
                    }
                }
            }
        }
        debug_assert_eq!(u.len(), UNIFORM_VEC4S * 4);
        u
    }

    fn bind_group(&mut self) -> wgpu::BindGroup {
        if let Some(bg) = &self.comp_bg {
            return bg.clone();
        }
        let blank = view(&self.blank);
        let cam = self
            .cam
            .as_ref()
            .map_or_else(|| blank.clone(), |c| c.full_view.clone());
        let cap = self
            .cap_tex
            .as_ref()
            .map_or_else(|| blank.clone(), |c| view(&c.0));
        let keys = self
            .keys_tex
            .as_ref()
            .map_or_else(|| blank.clone(), |c| view(&c.0));
        let bgt = self.bg_tex.as_ref().map_or_else(|| blank.clone(), view);
        let annt = self.ann_tex.as_ref().map_or_else(|| blank.clone(), view);
        let cambg = self.cambg_tex.as_ref().map_or_else(|| blank.clone(), view);
        let bg = self
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("composite"),
                layout: &self.p_comp.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&self.screen.full_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&cam),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&cap),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&keys),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::TextureView(&bgt),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::TextureView(&cambg),
                    },
                    wgpu::BindGroupEntry {
                        binding: 8,
                        resource: wgpu::BindingResource::TextureView(&annt),
                    },
                ],
            });
        self.comp_bg = Some(bg.clone());
        bg
    }

    fn prepare_video(
        enc: &mut wgpu::CommandEncoder,
        v: &mut VideoTex,
        p_conv: &wgpu::RenderPipeline,
        p_down: &wgpu::RenderPipeline,
    ) {
        if v.dirty_nv12 {
            let mut rp = pass(enc, &v.mip_views[0]);
            rp.set_pipeline(p_conv);
            rp.set_bind_group(0, &v.conv_bg, &[]);
            rp.draw(0..3, 0..1);
            v.dirty_nv12 = false;
        }
        if v.dirty_mips {
            for l in 1..v.levels as usize {
                let mut rp = pass(enc, &v.mip_views[l]);
                rp.set_pipeline(p_down);
                rp.set_bind_group(0, &v.mip_bgs[l - 1], &[]);
                rp.draw(0..3, 0..1);
            }
            v.dirty_mips = false;
        }
        let _ = &v.rgba;
    }

    fn readback_layout(&self) -> Vec<(u32, u32, u32)> {
        // (row bytes, padded row bytes, rows)
        match self.readback {
            Readback::Nv12 => vec![
                (self.w, align256(self.w), self.h),
                (self.w / 2 * 2, align256(self.w / 2 * 2), self.h / 2),
            ],
            Readback::Rgba => vec![(self.w * 4, align256(self.w * 4), self.h)],
        }
    }

    /// Render one frame for `scene`. Returns frames that finished reading back
    /// (pipelined: up to `ring` frames are in flight), each `(tag, bytes)`.
    pub fn submit(
        &mut self,
        scene: &Scene,
        tag: u64,
        keys_alpha: f64,
        cam_uv: [f32; 4],
        cam_lod: f32,
    ) -> Result<Vec<(u64, Vec<u8>)>> {
        self.gpu.errors.check()?;
        let u = self.uniforms(scene, keys_alpha, cam_uv, cam_lod);
        self.gpu
            .queue
            .write_buffer(&self.uniforms, 0, &f32_bytes(&u));
        let bg = self.bind_group();
        let mut enc = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        Self::prepare_video(&mut enc, &mut self.screen, &self.p_conv, &self.p_down);
        if let Some(c) = self.cam.as_mut() {
            Self::prepare_video(&mut enc, c, &self.p_conv, &self.p_down);
        }
        let out_view = view(&self.out);
        {
            let mut rp = pass(&mut enc, &out_view);
            rp.set_pipeline(&self.p_comp);
            rp.set_bind_group(0, &bg, &[]);
            rp.draw(0..3, 0..1);
        }
        let layout = self.readback_layout();
        let bufs = self.free.pop().unwrap_or_else(|| {
            layout
                .iter()
                .map(|(_, padded, rows)| {
                    self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("readback"),
                        size: (*padded as u64) * (*rows as u64),
                        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    })
                })
                .collect()
        });
        let copy = |enc: &mut wgpu::CommandEncoder,
                    t: &wgpu::Texture,
                    b: &wgpu::Buffer,
                    padded: u32,
                    w: u32,
                    h: u32| {
            enc.copy_texture_to_buffer(
                t.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: b,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded),
                        rows_per_image: None,
                    },
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        };
        match self.readback {
            Readback::Nv12 => {
                let yv = view(&self.y_out);
                {
                    let mut rp = pass(&mut enc, &yv);
                    rp.set_pipeline(&self.p_y);
                    rp.set_bind_group(0, &self.e_y_bg, &[]);
                    rp.draw(0..3, 0..1);
                }
                let uvv = view(&self.uv_out);
                {
                    let mut rp = pass(&mut enc, &uvv);
                    rp.set_pipeline(&self.p_uv);
                    rp.set_bind_group(0, &self.e_uv_bg, &[]);
                    rp.draw(0..3, 0..1);
                }
                copy(&mut enc, &self.y_out, &bufs[0], layout[0].1, self.w, self.h);
                copy(
                    &mut enc,
                    &self.uv_out,
                    &bufs[1],
                    layout[1].1,
                    self.w / 2,
                    self.h / 2,
                );
            }
            Readback::Rgba => copy(&mut enc, &self.out, &bufs[0], layout[0].1, self.w, self.h),
        }
        let submission = self.gpu.queue.submit([enc.finish()]);
        let ready = Arc::new(AtomicBool::new(false));
        let n = bufs.len();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for b in &bufs {
            let (ready, count) = (ready.clone(), count.clone());
            b.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                if r.is_ok() && count.fetch_add(1, Ordering::AcqRel) + 1 == n {
                    ready.store(true, Ordering::Release);
                }
            });
        }
        self.pending.push_back(Slot {
            tag,
            bufs,
            ready,
            submission,
        });
        let mut done = Vec::new();
        while self.pending.len() >= self.ring {
            done.push(self.finish_one()?);
        }
        Ok(done)
    }

    fn finish_one(&mut self) -> Result<(u64, Vec<u8>)> {
        let slot = self.pending.pop_front().expect("pending slot");
        self.gpu
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(slot.submission.clone()),
                timeout: None,
            })
            .map_err(|e| RenderError::Gpu(format!("poll: {e}")))?;
        if !slot.ready.load(Ordering::Acquire) {
            return Err(RenderError::Gpu("readback map failed".into()));
        }
        let layout = self.readback_layout();
        let total: usize = layout
            .iter()
            .map(|(row, _, rows)| (*row * *rows) as usize)
            .sum();
        let mut out = Vec::with_capacity(total);
        for (b, (row, padded, rows)) in slot.bufs.iter().zip(&layout) {
            {
                let data = b
                    .slice(..)
                    .get_mapped_range()
                    .map_err(|e| RenderError::Gpu(format!("map: {e:?}")))?;
                for r in 0..*rows as usize {
                    let o = r * *padded as usize;
                    out.extend_from_slice(&data[o..o + *row as usize]);
                }
            }
            b.unmap();
        }
        self.free.push(slot.bufs);
        Ok((slot.tag, out))
    }

    /// Wait for every in-flight frame.
    pub fn flush(&mut self) -> Result<Vec<(u64, Vec<u8>)>> {
        self.gpu.errors.check()?;
        let mut v = Vec::new();
        while !self.pending.is_empty() {
            v.push(self.finish_one()?);
        }
        Ok(v)
    }
}

fn f32_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Frame, RenderProject};
    use crate::scene;

    /// Every test that touches a GPU device holds this while it does. Separate
    /// WARP devices working at the same time crash the WARP runtime on the CI
    /// runners (0xc0000005). The app normally has one device (the cached
    /// [`Gpu::shared`] one; only a replacement after a GPU error briefly makes a
    /// second), so the tests never overlap devices either.
    static GPU_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn gpu_serial() -> std::sync::MutexGuard<'static, ()> {
        GPU_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The cached WARP device, like the app's one cached device.
    fn warp() -> Result<Gpu> {
        Gpu::shared(true, true)
    }

    #[test]
    fn texture_dimensions_are_checked_against_the_device_limit() {
        assert!(check_dims("screen", (1920, 1080), 8192).is_ok());
        assert!(check_dims("screen", (8192, 8192), 8192).is_ok());
        for bad in [
            (0, 1080),
            (1920, 0),
            (8193, 100),
            (100, 8193),
            (u32::MAX, u32::MAX),
        ] {
            let e = check_dims("screen", bad, 8192).unwrap_err().to_string();
            assert!(e.contains("screen"), "{e}");
        }
    }

    #[test]
    fn uncaptured_gpu_errors_are_recorded_not_panicked() {
        let sink = ErrorSink::default();
        assert!(sink.check().is_ok());
        sink.record("out of memory".into());
        sink.record("second".into()); // the first error is kept
        let e = sink.check().unwrap_err().to_string();
        assert!(e.contains("out of memory") && !e.contains("second"), "{e}");
        // Sticky: once the device is in error every later check fails too.
        assert!(sink.check().is_err());
    }

    /// NV12 readback (encoder path): pure red maps to BT.709 limited-range
    /// Y=63, Cb=102, Cr=240 (rounded), in a tight W*H*3/2 buffer.
    #[test]
    fn warp_nv12_readback_bt709_limited() {
        let _gpu = gpu_serial();
        let Ok(gpu) = warp() else {
            eprintln!("no WARP adapter; skipped");
            return;
        };
        let (w, h) = (64u32, 32u32);
        let mut p = RenderProject::empty_for_tests();
        p.canvas = (16, 16);
        p.style.padding = 0.0;
        p.style.radius = 0.0;
        p.style.shadow = 0.0;
        p.style.background.stops = vec![[1.0, 0.0, 0.0, 1.0]];
        let mut c = Compositor::new(gpu, w, h, (16, 16), 1, None, Readback::Nv12).unwrap();
        c.upload_screen_rgba(&[255u8, 0, 0, 255].repeat(16 * 16));
        let s = scene::build(&p, w as f64, h as f64, 0.0, false);
        let mut frames = c.submit(&s, 1, 0.0, [0.0, 0.0, 1.0, 1.0], 0.0).unwrap();
        frames.extend(c.flush().unwrap());
        let (_, nv12) = frames.pop().unwrap();
        assert_eq!(nv12.len(), (w * h * 3 / 2) as usize);
        let (y, uv) = nv12.split_at((w * h) as usize);
        assert!(
            y.iter().all(|&v| (v as i32 - 63).abs() <= 1),
            "Y {:?}",
            &y[..4]
        );
        assert!(
            uv.chunks(2)
                .all(|c| (c[0] as i32 - 102).abs() <= 1 && (c[1] as i32 - 240).abs() <= 1),
            "UV {:?}",
            &uv[..4]
        );
    }

    /// Software (WARP) path: render a synthetic frame and check background
    /// gradient, padding, window and video content by pixel value.
    #[test]
    fn warp_composite_background_padding_video() {
        let _gpu = gpu_serial();
        let gpu = match warp() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("no WARP adapter: {e}; skipped");
                return;
            }
        };
        assert!(gpu.software);
        let (w, h) = (320u32, 180u32);
        let mut p = RenderProject::empty_for_tests();
        p.canvas = (64, 36);
        p.style.shadow = 0.0;
        let mut c = Compositor::new(gpu, w, h, (64, 36), 1, None, Readback::Rgba).unwrap();
        // Solid red source.
        let src: Vec<u8> = (0..64 * 36).flat_map(|_| [255u8, 0, 0, 255]).collect();
        c.upload_screen_rgba(&src);
        let s = scene::build(&p, w as f64, h as f64, 0.0, false);
        let mut frames = c.submit(&s, 7, 0.0, [0.0, 0.0, 1.0, 1.0], 0.0).unwrap();
        frames.extend(c.flush().unwrap());
        let (tag, rgba) = frames.pop().unwrap();
        assert_eq!(tag, 7);
        let px = |x: u32, y: u32| {
            let o = ((y * w + x) * 4) as usize;
            [rgba[o], rgba[o + 1], rgba[o + 2]]
        };
        // Corners show the gradient (#a1c4fd -> #c2e9fb at 135deg).
        let tl = px(0, 0);
        let br = px(w - 1, h - 1);
        let exp_tl = crate::math::gradient_color(&p.style.background, w as f64, h as f64, 0.5, 0.5);
        for i in 0..3 {
            assert!(
                (tl[i] as f64 - exp_tl[i] * 255.0).abs() <= 2.0,
                "tl {tl:?} vs {exp_tl:?}"
            );
        }
        assert!(
            br[0] > tl[0] && br[1] > tl[1],
            "gradient direction {tl:?} {br:?}"
        );
        // Padding: a pixel just inside the padding band is still background (not red).
        let pad = (0.08 * (w as f64).min(h as f64 * 1.78)) as u32;
        assert!(px(pad / 2, h / 2)[0] < 200);
        // Centre is the (red) video.
        let mid = px(w / 2, h / 2);
        assert!(mid[0] > 250 && mid[1] < 5 && mid[2] < 5, "{mid:?}");
    }

    /// Render one synthetic RGBA frame on WARP; `None` when no WARP adapter exists.
    fn warp_frame(
        p: &RenderProject,
        (w, h): (u32, u32),
        screen: [u8; 4],
        bg: Option<&[u8]>,
        cam: Option<[u8; 4]>,
    ) -> Option<Vec<u8>> {
        let _gpu = gpu_serial();
        let gpu = warp().ok()?;
        let (sw, sh) = p.canvas;
        let cam_size = cam.map(|_| (16u32, 16u32));
        let mut c = Compositor::new(gpu, w, h, (sw, sh), 1, cam_size, Readback::Rgba).unwrap();
        c.upload_screen_rgba(&screen.repeat((sw * sh) as usize));
        if let Some(col) = cam {
            c.upload_webcam_rgba(&col.repeat(16 * 16));
        }
        c.set_background(bg);
        let s = scene::build(p, w as f64, h as f64, 0.0, cam.is_some());
        let mut frames = c.submit(&s, 0, 0.0, [0.0, 0.0, 1.0, 1.0], 0.0).unwrap();
        frames.extend(c.flush().unwrap());
        Some(frames.pop().unwrap().1)
    }

    fn at(f: &[u8], w: u32, x: f64, y: f64) -> [i32; 3] {
        let o = ((y as u32 * w + x as u32) * 4) as usize;
        [f[o] as i32, f[o + 1] as i32, f[o + 2] as i32]
    }

    fn dist(a: [i32; 3], b: [i32; 3]) -> i32 {
        (0..3).map(|i| (a[i] - b[i]).abs()).max().unwrap()
    }

    fn plain_project() -> RenderProject {
        let mut p = RenderProject::empty_for_tests();
        p.canvas = (64, 36);
        p.style.shadow = 0.0;
        p.style.frame = Frame::None;
        p
    }

    #[test]
    fn warp_laptop_frame_draws_bezel_and_keyboard_base() {
        let (w, h) = (640u32, 360u32);
        let mut p = plain_project();
        p.style.frame = Frame::Laptop;
        let Some(f) = warp_frame(&p, (w, h), [255, 0, 0, 255], None, None) else {
            return;
        };
        // The plain background colour at a pixel (shadow is off in this project).
        let bgref = |x: f64, y: f64| {
            let c = crate::math::gradient_color(&p.style.background, w as f64, h as f64, x, y);
            [
                (c[0] * 255.0).round() as i32,
                (c[1] * 255.0).round() as i32,
                (c[2] * 255.0).round() as i32,
            ]
        };
        let lay = crate::math::layout(w as f64, h as f64, 64.0, 36.0, &p);
        // Content is the red video.
        let c = at(
            &f,
            w,
            lay.content.x + lay.content.w / 2.0,
            lay.content.y + lay.content.h / 2.0,
        );
        assert!(c[0] > 245 && c[1] < 10 && c[2] < 10, "{c:?}");
        // Just outside the content, inside the lid: dark bezel (not red, not background).
        let bez = at(
            &f,
            w,
            lay.window.x + lay.bezel / 2.0,
            lay.window.y + lay.window.h / 2.0,
        );
        assert!(bez[0] < 60 && bez[1] < 60 && bez[2] < 70, "bezel {bez:?}");
        // Below the lid, inside the base: a light metal grey, unlike the gradient there.
        let by = lay.window.y1() + lay.base_h / 2.0;
        let bx = lay.window.x + lay.window.w / 2.0;
        let base = at(&f, w, bx, by);
        assert!(dist(base, bgref(bx, by)) > 12, "base {base:?}");
        assert!(
            base[0] > 120 && (base[0] - base[2]).abs() < 40,
            "base {base:?}"
        );
        // The base overhangs the lid but stops there: past it, plain gradient again.
        let ox = lay.window.x - lay.base_overhang / 2.0;
        assert!(
            dist(at(&f, w, ox, by), bgref(ox, by)) > 12,
            "overhang not drawn"
        );
        let away = lay.window.x - lay.base_overhang - 6.0;
        assert!(dist(at(&f, w, away, by), bgref(away, by)) <= 3);
    }

    #[test]
    fn warp_phone_frame_has_bezel_and_camera_pill() {
        let (w, h) = (360u32, 640u32);
        let mut p = plain_project();
        p.style.frame = Frame::Phone;
        let Some(f) = warp_frame(&p, (w, h), [255, 0, 0, 255], None, None) else {
            return;
        };
        let lay = crate::math::layout(w as f64, h as f64, 64.0, 36.0, &p);
        let mid = at(
            &f,
            w,
            lay.content.x + lay.content.w / 2.0,
            lay.content.y + lay.content.h / 2.0,
        );
        assert!(mid[0] > 245 && mid[1] < 10, "{mid:?}");
        let bez = at(
            &f,
            w,
            lay.window.x + lay.bezel / 2.0,
            lay.window.y + lay.window.h / 2.0,
        );
        assert!(bez[0] < 40 && bez[1] < 40 && bez[2] < 50, "bezel {bez:?}");
        // The camera pill sits at the top centre of the screen, over the video.
        let pill = at(
            &f,
            w,
            lay.content.x + lay.content.w / 2.0,
            lay.content.y + 12.0 * lay.unit,
        );
        assert!(pill[0] < 60, "pill {pill:?}");
        // A corner of the window rect is rounded away (shows the gradient, not the bezel).
        let corner = at(&f, w, lay.window.x + 1.0, lay.window.y + 1.0);
        assert!(corner[0] > 100, "corner should be background: {corner:?}");
    }

    #[test]
    fn warp_webcam_border_is_drawn_inside_the_bubble() {
        let (w, h) = (320u32, 180u32);
        let mut p = plain_project();
        p.style.webcam_enabled = true;
        p.style.webcam_shape = crate::model::WebcamShape::Square;
        p.style.webcam_size = 0.4;
        p.style.webcam_margin = 0.05;
        p.style.webcam_border_width = 20.0;
        p.style.webcam_border_color = [1.0, 0.0, 0.0, 1.0];
        let Some(f) = warp_frame(&p, (w, h), [0, 0, 255, 255], None, Some([0, 255, 0, 255])) else {
            return;
        };
        let s = scene::build(&p, w as f64, h as f64, 0.0, true);
        let cam = s.webcam.unwrap();
        // Margin: bubble is 5 % of the height from the right/bottom edge.
        assert!((w as f64 - cam.rect.x1() - 9.0).abs() < 1e-6);
        let mid = at(
            &f,
            w,
            cam.rect.x + cam.rect.w / 2.0,
            cam.rect.y + cam.rect.h / 2.0,
        );
        assert!(mid[1] > 240 && mid[0] < 15, "centre is the camera: {mid:?}");
        let edge = at(
            &f,
            w,
            cam.rect.x + cam.border / 2.0 + 0.5,
            cam.rect.y + cam.rect.h / 2.0,
        );
        assert!(edge[0] > 240 && edge[1] < 15, "border colour: {edge:?}");
        let just_in = at(
            &f,
            w,
            cam.rect.x + cam.border + 3.0,
            cam.rect.y + cam.rect.h / 2.0,
        );
        assert!(
            just_in[1] > 240,
            "camera starts inside the border: {just_in:?}"
        );
        // No border -> the camera reaches the edge.
        p.style.webcam_border_width = 0.0;
        let f2 = warp_frame(&p, (w, h), [0, 0, 255, 255], None, Some([0, 255, 0, 255])).unwrap();
        let edge2 = at(&f2, w, cam.rect.x + 1.5, cam.rect.y + cam.rect.h / 2.0);
        assert!(edge2[1] > 240, "{edge2:?}");
    }

    #[test]
    fn warp_background_image_covers_the_gradient_and_letterbox_shows_through() {
        let (w, h) = (160u32, 90u32);
        let mut p = plain_project();
        p.style.padding = 20.0;
        // Fully blue background over the output.
        let blue: Vec<u8> = [0u8, 0, 255, 255].repeat((w * h) as usize);
        let Some(f) = warp_frame(&p, (w, h), [255, 0, 0, 255], Some(&blue), None) else {
            return;
        };
        let c = at(&f, w, 2.0, 2.0);
        assert!(c[2] > 250 && c[0] < 5, "image covers the gradient: {c:?}");
        let mid = at(&f, w, w as f64 / 2.0, h as f64 / 2.0);
        assert!(mid[0] > 250, "video stays on top of the image: {mid:?}");
        // Left half transparent (premultiplied zeros): the gradient shows there.
        let mut half = blue.clone();
        for y in 0..h {
            for x in 0..w / 2 {
                let o = ((y * w + x) * 4) as usize;
                half[o..o + 4].copy_from_slice(&[0, 0, 0, 0]);
            }
        }
        let g = warp_frame(&p, (w, h), [255, 0, 0, 255], Some(&half), None).unwrap();
        let left = at(&g, w, 2.0, 2.0);
        let right = at(&g, w, w as f64 - 3.0, 2.0);
        assert!(
            left[2] > 200 && left[0] > 120,
            "gradient in the letterbox: {left:?}"
        );
        assert!(
            right[2] > 250 && right[0] < 5,
            "image on the right: {right:?}"
        );
    }

    // ------------------------------------------------ annotations + webcam effects

    struct Inputs<'a> {
        screen: Vec<u8>,
        cam: Option<Vec<u8>>,
        ann_text: Option<&'a [u8]>,
        cam_bg: Option<(&'a [u8], u32)>,
    }

    fn render(p: &RenderProject, (w, h): (u32, u32), i: Inputs) -> Option<Vec<u8>> {
        let _gpu = gpu_serial();
        let gpu = warp().ok()?;
        let (sw, sh) = p.canvas;
        let cam_size = i.cam.as_ref().map(|_| (16u32, 16u32));
        let mut c = Compositor::new(gpu, w, h, (sw, sh), 4, cam_size, Readback::Rgba).unwrap();
        c.upload_screen_rgba(&i.screen);
        if let Some(cam) = &i.cam {
            c.upload_webcam_rgba(cam);
        }
        c.set_annotation_text(i.ann_text);
        c.set_webcam_background(i.cam_bg);
        let s = scene::build(p, w as f64, h as f64, 100.0, i.cam.is_some());
        let mut frames = c.submit(&s, 0, 0.0, [0.0, 0.0, 1.0, 1.0], 0.0).unwrap();
        frames.extend(c.flush().unwrap());
        Some(frames.pop().unwrap().1)
    }

    fn grey_screen() -> Vec<u8> {
        [100u8, 100, 100, 255].repeat(64 * 36)
    }

    fn stripes_screen() -> Vec<u8> {
        (0..64 * 36)
            .flat_map(|i| {
                let v = if i % 2 == 0 { 0u8 } else { 255 };
                [v, v, v, 255]
            })
            .collect()
    }

    /// Full-bleed project (no padding / frame / shadow) so the video fills the output.
    fn full_bleed() -> RenderProject {
        let mut p = plain_project();
        p.style.padding = 0.0;
        p.style.radius = 0.0;
        p
    }

    fn ann(kind: crate::model::AnnKind) -> crate::model::Ann {
        crate::model::Ann {
            id: "t".into(),
            kind,
            start: 0.0,
            end: 1000.0,
            x: 0.25,
            y: 0.25,
            w: 0.5,
            h: 0.5,
            x2: 0.9,
            y2: 0.5,
            color: [1.0, 0.0, 0.0],
            stroke: 40.0,
            fill: false,
            text: String::new(),
            font: 40.0,
            opacity: 1.0,
            strength: 1.0,
            dim: 0.6,
            radius: 0.0,
            fade: 0.0,
        }
    }

    fn annotated(a: crate::model::Ann) -> Option<Vec<u8>> {
        let mut p = full_bleed();
        p.annotations = vec![a];
        render(
            &p,
            (320, 180),
            Inputs {
                screen: if matches!(
                    p.annotations[0].kind,
                    crate::model::AnnKind::Blur | crate::model::AnnKind::Pixelate
                ) {
                    stripes_screen()
                } else {
                    grey_screen()
                },
                cam: None,
                ann_text: None,
                cam_bg: None,
            },
        )
    }

    #[test]
    fn warp_annotation_rect_ellipse_highlight() {
        use crate::model::AnnKind as K;
        let Some(f) = annotated(ann(K::Rect)) else {
            return;
        };
        let w = 320;
        // Stroke centred on the rect edge x = 80; thick enough to hit a pixel.
        assert!(
            at(&f, w, 81.0, 90.0)[0] > 240 && at(&f, w, 81.0, 90.0)[1] < 15,
            "edge"
        );
        assert_eq!(at(&f, w, 160.0, 90.0), [100, 100, 100], "unfilled centre");
        assert_eq!(at(&f, w, 20.0, 20.0), [100, 100, 100], "outside");
        let mut a = ann(K::Rect);
        a.fill = true;
        let f = annotated(a).unwrap();
        assert!(at(&f, w, 160.0, 90.0)[0] > 240, "filled centre");
        assert_eq!(at(&f, w, 20.0, 20.0), [100, 100, 100]);
        let mut a = ann(K::Ellipse);
        a.fill = true;
        let f = annotated(a).unwrap();
        assert!(at(&f, w, 160.0, 90.0)[0] > 240, "ellipse centre");
        assert_eq!(
            at(&f, w, 82.0, 47.0),
            [100, 100, 100],
            "ellipse leaves the bbox corner"
        );
        let mut a = ann(K::Highlight);
        a.opacity = 1.0;
        let f = annotated(a).unwrap();
        let c = at(&f, w, 160.0, 90.0);
        assert!(
            c[0] > 130 && c[0] < 190 && c[1] < 100 && c[1] > 40,
            "translucent marker {c:?}"
        );
        // Opacity scales every drawing kind.
        let mut a = ann(K::Rect);
        a.fill = true;
        a.opacity = 0.5;
        let c = at(&annotated(a).unwrap(), w, 160.0, 90.0);
        assert!(c[0] > 160 && c[0] < 190, "half opacity {c:?}");
    }

    #[test]
    fn warp_annotation_arrow_has_shaft_and_head() {
        use crate::model::AnnKind as K;
        let mut a = ann(K::Arrow);
        a.x = 0.1;
        a.y = 0.5;
        a.x2 = 0.9;
        a.y2 = 0.5;
        a.stroke = 24.0;
        let Some(f) = annotated(a) else {
            return;
        };
        let w = 320;
        assert!(at(&f, w, 100.0, 90.0)[0] > 240, "shaft");
        assert_eq!(
            at(&f, w, 100.0, 50.0),
            [100, 100, 100],
            "away from the shaft"
        );
        assert!(at(&f, w, 280.0, 90.0)[0] > 240, "head body");
        // The head is wider than the shaft: a point off-axis near the head base is covered.
        // Head: 14 px long from the tip at x=288; at x=277 it is ~6 px half-wide,
        // while the shaft is only 2 px half-wide.
        assert!(at(&f, w, 277.0, 85.5)[0] > 200, "head flares out");
        assert_eq!(
            at(&f, w, 300.0, 90.0),
            [100, 100, 100],
            "nothing beyond the tip"
        );
        assert_eq!(
            at(&f, w, 20.0, 90.0),
            [100, 100, 100],
            "nothing before the tail"
        );
    }

    #[test]
    fn warp_annotation_spotlight_dims_outside_only() {
        use crate::model::AnnKind as K;
        let Some(f) = annotated(ann(K::Spotlight)) else {
            return;
        };
        let w = 320;
        assert_eq!(at(&f, w, 160.0, 90.0), [100, 100, 100], "inside stays lit");
        let o = at(&f, w, 20.0, 20.0);
        assert!((o[0] - 40).abs() <= 2, "outside darkened by dim 0.6: {o:?}");
    }

    #[test]
    fn warp_annotation_blur_and_pixelate_sample_the_source() {
        use crate::model::AnnKind as K;
        let w = 320;
        let contrast = |f: &[u8], x: f64, y: f64| {
            (0..8)
                .map(|i| at(f, w, x + i as f64, y)[0])
                .fold((255, 0), |(lo, hi), v| (lo.min(v), hi.max(v)))
        };
        let Some(f) = annotated(ann(K::Blur)) else {
            return;
        };
        let (lo, hi) = contrast(&f, 120.0, 90.0);
        assert!(hi - lo < 90, "blurred inside the rect: {lo}..{hi}");
        let (lo, hi) = contrast(&f, 10.0, 20.0);
        assert!(hi - lo > 200, "sharp outside: {lo}..{hi}");
        let f = annotated(ann(K::Pixelate)).unwrap();
        let (a, b, c) = (
            at(&f, w, 81.0, 100.0),
            at(&f, w, 82.0, 100.0),
            at(&f, w, 83.0, 100.0),
        );
        assert!(
            a[0].abs_diff(b[0]) <= 3 && b[0].abs_diff(c[0]) <= 3,
            "one block: {a:?} {b:?} {c:?}"
        );
        let (lo, hi) = contrast(&f, 10.0, 20.0);
        assert!(hi - lo > 200, "sharp outside: {lo}..{hi}");
        // Different blocks differ or at least the block grid is coarser than the source stripes.
        let row: Vec<i32> = (80..120).map(|x| at(&f, w, x as f64, 100.0)[0]).collect();
        let changes = row.windows(2).filter(|p| p[0].abs_diff(p[1]) > 6).count();
        assert!(
            changes < 10,
            "pixelated row changes only at block edges: {changes}"
        );
    }

    #[test]
    fn warp_annotation_text_layer_is_clipped_to_the_screen_content() {
        let mut p = plain_project();
        p.style.padding = 20.0;
        let (w, h) = (320u32, 180u32);
        // A white square sprite, half of it over the padding (outside the content).
        let mut layer = vec![0u8; (w * h * 4) as usize];
        for y in 50..120u32 {
            for x in 90..160u32 {
                let o = ((y * w + x) * 4) as usize;
                layer[o..o + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let mut with = full_bleed();
        with.style.padding = 20.0;
        with.annotations = vec![ann(crate::model::AnnKind::Text)];
        let Some(f) = render(
            &with,
            (w, h),
            Inputs {
                screen: grey_screen(),
                cam: None,
                ann_text: Some(&layer),
                cam_bg: None,
            },
        ) else {
            return;
        };
        let lay = crate::math::layout(w as f64, h as f64, 64.0, 36.0, &with);
        let (cx, cy) = (lay.content.x, lay.content.y);
        assert!(
            at(&f, w, cx + 8.0, cy + 8.0)[0] > 250,
            "sprite inside the content"
        );
        assert!(
            at(&f, w, cx - 6.0, cy + 8.0)[0] < 240,
            "clipped outside the content"
        );
        let _ = p;
    }

    fn cam_split(left: [u8; 4], right: [u8; 4]) -> Vec<u8> {
        let mut v = Vec::new();
        for _y in 0..16 {
            for x in 0..16 {
                v.extend(if x < 8 { left } else { right });
            }
        }
        v
    }

    fn cam_project() -> RenderProject {
        let mut p = full_bleed();
        p.style.webcam_enabled = true;
        p.style.webcam_shape = crate::model::WebcamShape::Square;
        p.style.webcam_size = 0.5;
        p.style.webcam_margin = 0.0;
        p.style.webcam_corner = (true, true);
        p.style.webcam_shadow = 0.0;
        p
    }

    fn cam_render(p: &RenderProject, cam: Vec<u8>, extra: Option<(&[u8], u32)>) -> Vec<u8> {
        render(
            p,
            (320, 180),
            Inputs {
                screen: [255u8, 255, 255, 255].repeat(64 * 36),
                cam: Some(cam),
                ann_text: None,
                cam_bg: extra,
            },
        )
        .unwrap()
    }

    #[test]
    fn warp_webcam_mirror_and_colour_grading() {
        let mut p = cam_project();
        // Bubble is 90x90 at the top-left; left half red, right half blue.
        let cam = cam_split([255, 0, 0, 255], [0, 0, 255, 255]);
        if render(
            &p,
            (320, 180),
            Inputs {
                screen: grey_screen(),
                cam: Some(cam.clone()),
                ann_text: None,
                cam_bg: None,
            },
        )
        .is_none()
        {
            return;
        }
        let f = cam_render(&p, cam.clone(), None);
        assert!(at(&f, 320, 20.0, 45.0)[0] > 240 && at(&f, 320, 70.0, 45.0)[2] > 240);
        p.style.webcam_mirror = true;
        let f = cam_render(&p, cam.clone(), None);
        assert!(
            at(&f, 320, 20.0, 45.0)[2] > 240 && at(&f, 320, 70.0, 45.0)[0] > 240,
            "mirrored"
        );
        p.style.webcam_mirror = false;
        let grey = vec![[100u8, 100, 100, 255]; 256].concat();
        p.style.webcam_brightness = 0.2;
        let b = at(&cam_render(&p, grey.clone(), None), 320, 45.0, 45.0);
        assert!((b[0] - 151).abs() <= 3, "brightness {b:?}");
        p.style.webcam_brightness = 0.0;
        p.style.webcam_contrast = 0.0;
        let c = at(&cam_render(&p, grey.clone(), None), 320, 45.0, 45.0);
        assert!((c[0] - 128).abs() <= 3, "contrast 0 -> mid grey {c:?}");
        p.style.webcam_contrast = 1.0;
        p.style.webcam_saturation = 0.0;
        let s = at(&cam_render(&p, cam, None), 320, 20.0, 45.0);
        assert!(
            s[0].abs_diff(s[2]) <= 2 && s[0].abs_diff(s[1]) <= 2 && s[0] > 40 && s[0] < 80,
            "desaturated red {s:?}"
        );
    }

    #[test]
    fn warp_webcam_shadow_strength_is_configurable() {
        let mut p = cam_project();
        p.style.webcam_corner = (false, false);
        p.style.webcam_margin = 0.1;
        let cam = vec![[0u8, 200, 0, 255]; 256].concat();
        if render(
            &p,
            (320, 180),
            Inputs {
                screen: grey_screen(),
                cam: Some(cam.clone()),
                ann_text: None,
                cam_bg: None,
            },
        )
        .is_none()
        {
            return;
        }
        // Just outside the bubble's left edge.
        let s = scene::build(&p, 320.0, 180.0, 100.0, true).webcam.unwrap();
        let (x, y) = (s.rect.x - 3.0, s.rect.y + s.rect.h / 2.0);
        let none = at(&cam_render(&p, cam.clone(), None), 320, x, y);
        assert_eq!(none, [255, 255, 255], "no shadow at strength 0");
        p.style.webcam_shadow = 1.0;
        let lots = at(&cam_render(&p, cam, None), 320, x, y);
        assert!(lots[0] < 235, "shadow darkens: {lots:?}");
    }

    #[test]
    fn warp_webcam_chroma_key_and_background_replacement() {
        use crate::model::{Key, WebcamBg};
        let mut p = cam_project();
        let cam = cam_split([0, 255, 0, 255], [220, 40, 40, 255]);
        if render(
            &p,
            (320, 180),
            Inputs {
                screen: grey_screen(),
                cam: Some(cam.clone()),
                ann_text: None,
                cam_bg: None,
            },
        )
        .is_none()
        {
            return;
        }
        p.style.webcam_key = Some(Key {
            color: [0.0, 1.0, 0.0],
            similarity: 0.4,
            smoothness: 0.1,
            spill: 0.0,
        });
        // Keyed out, no replacement: the white screen shows through the green half.
        let f = cam_render(&p, cam.clone(), None);
        assert_eq!(at(&f, 320, 20.0, 45.0), [255, 255, 255], "green removed");
        let r = at(&f, 320, 70.0, 45.0);
        assert!(r[0] > 200 && r[1] < 70, "subject kept: {r:?}");
        // Solid colour.
        p.style.webcam_bg = WebcamBg::Color([0.0, 0.0, 1.0, 1.0]);
        let f = cam_render(&p, cam.clone(), None);
        let b = at(&f, 320, 20.0, 45.0);
        assert!(
            b[2] > 240 && b[0] < 15 && b[1] < 15,
            "blue replacement {b:?}"
        );
        assert!(at(&f, 320, 70.0, 45.0)[0] > 200, "subject unchanged");
        // Gradient (top -> bottom, red -> yellow at 180deg).
        p.style.webcam_bg = WebcamBg::Gradient(crate::model::Gradient {
            angle_deg: 180.0,
            stops: vec![[1.0, 0.0, 0.0, 1.0], [1.0, 1.0, 0.0, 1.0]],
        });
        let f = cam_render(&p, cam.clone(), None);
        let (top, bot) = (at(&f, 320, 20.0, 4.0), at(&f, 320, 20.0, 86.0));
        assert!(top[0] > 240 && top[1] < 40, "gradient start {top:?}");
        assert!(bot[0] > 240 && bot[1] > 200, "gradient end {bot:?}");
        // Image: a magenta square.
        let magenta: Vec<u8> = [255u8, 0, 255, 255].repeat(16);
        p.style.webcam_bg = WebcamBg::Image("x.png".into());
        let f = cam_render(&p, cam.clone(), Some((&magenta, 4)));
        let m = at(&f, 320, 20.0, 45.0);
        assert!(
            m[0] > 240 && m[2] > 240 && m[1] < 15,
            "image replacement {m:?}"
        );
        // Blur: the keyed-out region shows a blurred, desaturated camera frame.
        p.style.webcam_bg = WebcamBg::Blur(0.5);
        let f = cam_render(&p, cam, None);
        let g = at(&f, 320, 20.0, 45.0);
        assert!(
            g[0].abs_diff(g[1]) <= 6 && g[1].abs_diff(g[2]) <= 6 && g[1] > 120 && g[1] < 230,
            "blurred grey {g:?}"
        );
    }

    /// One small webcam frame on `gpu`; returns the RGBA readback.
    fn small_render(gpu: Gpu) -> Vec<u8> {
        let p = cam_project();
        let mut c =
            Compositor::new(gpu, 64, 36, p.canvas, 1, Some((16, 16)), Readback::Rgba).unwrap();
        c.upload_screen_rgba(&grey_screen());
        c.upload_webcam_rgba(&[0u8, 200, 0, 255].repeat(16 * 16));
        let s = scene::build(&p, 64.0, 36.0, 0.0, true);
        let mut frames = c.submit(&s, 0, 0.0, [0.0, 0.0, 1.0, 1.0], 0.0).unwrap();
        frames.extend(c.flush().unwrap());
        frames.pop().unwrap().1
    }

    /// Regression: creating DX12 (WARP) devices on several threads at once
    /// crashed the process (0xc0000005). `DEVICE_LIFECYCLE` serialises creation;
    /// every device made this way works. They are dropped one at a time.
    #[test]
    fn devices_created_on_many_threads_at_once_all_work() {
        let _gpu = gpu_serial();
        if Gpu::new(true, true).is_err() {
            eprintln!("no WARP adapter; skipped");
            return;
        }
        let handles: Vec<_> = (0..4)
            .map(|_| std::thread::spawn(|| Gpu::new(true, true)))
            .collect();
        let made: Vec<Gpu> = handles
            .into_iter()
            .map(|h| h.join().unwrap().unwrap())
            .collect();
        for gpu in made {
            assert_eq!(small_render(gpu).len(), 64 * 36 * 4);
        }
    }

    /// Two exports at once share the one cached device and render on it from
    /// different threads at the same time.
    #[test]
    fn the_shared_device_renders_on_many_threads_at_once() {
        let _gpu = gpu_serial();
        let Ok(first) = warp() else {
            eprintln!("no WARP adapter; skipped");
            return;
        };
        let handles: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    let gpu = warp().unwrap();
                    (0..6)
                        .map(|_| small_render(gpu.clone()).len())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for h in handles {
            assert!(h.join().unwrap().iter().all(|&n| n == 64 * 36 * 4));
        }
        assert!(warp().unwrap().same_device(&first), "no second device");
    }

    #[test]
    fn shared_device_is_reused_and_replaced_after_an_error() {
        let _gpu = gpu_serial();
        let Ok(a) = Gpu::shared(true, true) else {
            eprintln!("no WARP adapter; skipped");
            return;
        };
        let started = std::time::Instant::now();
        let b = Gpu::shared(true, true).unwrap();
        assert!(a.same_device(&b), "second call reuses the cached device");
        assert!(started.elapsed().as_millis() < 50, "cache hit is instant");
        // Pipelines are built once and shared too.
        assert!(std::ptr::eq(a.pipelines(), b.pipelines()));
        // A recorded GPU error poisons the device: the next caller gets a fresh one.
        a.record_error("device lost".into());
        let c = Gpu::shared(true, true).unwrap();
        assert!(!c.same_device(&a));
        assert!(c.errors_ok());
        // Different adapter policy (hardware allowed vs forced WARP) never mixes.
        assert!(Gpu::new(true, true).unwrap().software);
    }
}
