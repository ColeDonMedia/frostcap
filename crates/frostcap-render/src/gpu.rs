//! wgpu (DX12 only) compositor: NV12 upload -> RGB + mips -> one composite pass
//! (background, shadow, window frame, zoomed video, ripples, cursor, webcam,
//! captions, keys) -> NV12 (BT.709 limited) or RGBA readback, pipelined.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Wake, Waker};

use crate::error::{RenderError, Result};
use crate::model::Frame;
use crate::scene::{MAX_CURSORS, MAX_RIPPLES, MAX_STOPS, Scene};
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

/// GPU device.
pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    errors: ErrorSink,
    /// Adapter name.
    pub adapter: String,
    /// True when running on the WARP software rasterizer.
    pub software: bool,
}

impl Gpu {
    /// Create a DX12 device. Tries a hardware adapter first; falls back to the
    /// WARP software adapter when `allow_software` (or forced via
    /// `FROSTCAP_FORCE_WARP=1` / `force_software`).
    pub fn new(allow_software: bool, force_software: bool) -> Result<Self> {
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
            adapter: format!("{} ({:?})", info.name, info.device_type),
            software: info.device_type == wgpu::DeviceType::Cpu,
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

const UNIFORM_VEC4S: usize = 36;

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
        let p_conv = pipe("fs_nv12", F::Rgba8Unorm);
        let p_down = pipe("fs_down", F::Rgba8Unorm);
        let p_comp = pipe("fs_composite", F::Rgba8Unorm);
        let p_y = pipe("fs_y", F::R8Unorm);
        let p_uv = pipe("fs_uv", F::Rg8Unorm);
        let sampler = d.create_sampler(&wgpu::SamplerDescriptor {
            label: None,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
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
        };
        v4(s.radius, s.bar_h, kind, 0.0);
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
    use crate::model::RenderProject;
    use crate::scene;

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
        let Ok(gpu) = Gpu::new(true, true) else {
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
        let gpu = match Gpu::new(true, true) {
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
}
