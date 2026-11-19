//! GPU side of the HDR -> SDR conversion ([`crate::hdr`] has the maths): a tiny
//! full-screen-triangle pixel shader that draws an scRGB (`R16G16B16A16_FLOAT`)
//! or plain BGRA source rectangle into a region of a BGRA render target, plus HDR
//! detection of a monitor (DXGI output colour space) and its SDR white level.

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BUFFER_DESC, D3D11_COMPARISON_NEVER, D3D11_CULL_NONE,
    D3D11_FILL_SOLID, D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_FILTER_MIN_MAG_MIP_POINT,
    D3D11_RASTERIZER_DESC, D3D11_SAMPLER_DESC, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_VIEWPORT, ID3D11Buffer, ID3D11Device, ID3D11DeviceContext,
    ID3D11PixelShader, ID3D11RasterizerState, ID3D11RenderTargetView, ID3D11SamplerState,
    ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput6};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFOEXW};
use windows::core::{Interface, PCSTR, s};

use crate::error::{Error, Result};
use crate::project::Rect;

/// Pixel shader mirroring [`crate::hdr::tonemap`].
pub const HLSL: &str = r"
Texture2D src : register(t0);
SamplerState smp : register(s0);
cbuffer C : register(b0) { float4 uv_rect; float scale; float hdr; float2 pad; };
struct VSOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
VSOut vs(uint id : SV_VertexID) {
    float2 p = float2((id << 1) & 2, id & 2);
    VSOut o;
    o.pos = float4(p * float2(2, -2) + float2(-1, 1), 0, 1);
    o.uv = p;
    return o;
}
float3 oetf(float3 l) {
    l = saturate(l);
    return lerp(1.055 * pow(l, 1.0 / 2.4) - 0.055, 12.92 * l, l <= 0.0031308);
}
float4 ps(VSOut i) : SV_Target {
    float2 uv = lerp(uv_rect.xy, uv_rect.zw, i.uv);
    float3 c = src.Sample(smp, uv).rgb;
    if (hdr > 0.5) {
        c = (isfinite(c) ? max(c, 0.0) : 0.0) * scale;
        c = c / max(max(c.r, max(c.g, c.b)), 1.0);
        c = oetf(c);
    }
    return float4(c, 1.0);
}
";

fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: valid source buffer and out params; blobs are read before they drop.
    unsafe {
        let r = D3DCompile(
            HLSL.as_ptr().cast(),
            HLSL.len(),
            s!("frostcap-hdr"),
            None,
            None,
            entry,
            target,
            0,
            0,
            &mut code,
            Some(&mut errors),
        );
        if let Err(e) = r {
            let msg = errors.map_or_else(String::new, |b| {
                String::from_utf8_lossy(std::slice::from_raw_parts(
                    b.GetBufferPointer().cast::<u8>(),
                    b.GetBufferSize(),
                ))
                .into_owned()
            });
            return Err(Error::Capture(format!("HLSL compile failed: {e} {msg}")));
        }
        let code = code.ok_or_else(|| Error::Capture("HLSL compile returned no code".into()))?;
        Ok(
            std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize())
                .to_vec(),
        )
    }
}

/// Draws source rectangles into a BGRA render target, converting HDR when asked.
pub struct Converter {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    linear: ID3D11SamplerState,
    raster: ID3D11RasterizerState,
    cbuf: ID3D11Buffer,
}

// SAFETY: free-threaded D3D11 objects on a multithread-protected device; the
// capture code serializes use behind its own mutex.
unsafe impl Send for Converter {}

impl Converter {
    /// Compile the shaders and create the fixed pipeline state.
    pub fn new(device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<Self> {
        let vs_code = compile(s!("vs"), s!("vs_4_0"))?;
        let ps_code = compile(s!("ps"), s!("ps_4_0"))?;
        // SAFETY: plain object creation with valid descriptors and out params.
        unsafe {
            let mut vs = None;
            device.CreateVertexShader(&vs_code, None, Some(&mut vs))?;
            let mut ps = None;
            device.CreatePixelShader(&ps_code, None, Some(&mut ps))?;
            let mut sampler = None;
            device.CreateSamplerState(
                &D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_MIP_POINT,
                    AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                    ComparisonFunc: D3D11_COMPARISON_NEVER,
                    MaxLOD: f32::MAX,
                    ..Default::default()
                },
                Some(&mut sampler),
            )?;
            let mut linear = None;
            device.CreateSamplerState(
                &D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                    AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                    ComparisonFunc: D3D11_COMPARISON_NEVER,
                    MaxLOD: f32::MAX,
                    ..Default::default()
                },
                Some(&mut linear),
            )?;
            let mut raster = None;
            device.CreateRasterizerState(
                &D3D11_RASTERIZER_DESC {
                    FillMode: D3D11_FILL_SOLID,
                    CullMode: D3D11_CULL_NONE,
                    DepthClipEnable: true.into(),
                    ..Default::default()
                },
                Some(&mut raster),
            )?;
            let mut cbuf = None;
            device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: 32,
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut cbuf),
            )?;
            let missing = || Error::Capture("D3D11 object creation returned null".into());
            Ok(Self {
                device: device.clone(),
                context: context.clone(),
                vs: vs.ok_or_else(missing)?,
                ps: ps.ok_or_else(missing)?,
                sampler: sampler.ok_or_else(missing)?,
                linear: linear.ok_or_else(missing)?,
                raster: raster.ok_or_else(missing)?,
                cbuf: cbuf.ok_or_else(missing)?,
            })
        }
    }

    /// Draw `src_rect` (source pixels) of `src` into the `dst_w` x `dst_h` area of `dst` at
    /// (`dst_x`, `dst_y`); bilinear when that differs from the source size.
    /// With `hdr`, `src` is scRGB and `sdr_white_nits` is the display's SDR white level;
    /// otherwise pixels are copied as they are.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        src: &ID3D11Texture2D,
        src_rect: Rect,
        dst: &ID3D11Texture2D,
        (dst_x, dst_y): (u32, u32),
        (dst_w, dst_h): (u32, u32),
        hdr: bool,
        sdr_white_nits: f32,
    ) -> Result<()> {
        let mut sd = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: plain getter.
        unsafe { src.GetDesc(&mut sd) };
        let (sw, sh) = (sd.Width as f32, sd.Height as f32);
        let consts: [f32; 8] = [
            src_rect.x as f32 / sw,
            src_rect.y as f32 / sh,
            (src_rect.x + src_rect.width) as f32 / sw,
            (src_rect.y + src_rect.height) as f32 / sh,
            crate::hdr::SCRGB_WHITE_NITS / sdr_white_nits.max(crate::hdr::SCRGB_WHITE_NITS),
            if hdr { 1.0 } else { 0.0 },
            0.0,
            0.0,
        ];
        // SAFETY: pipeline setup on our own device; views are dropped after the draw
        // and the SRV slot is cleared so the source can be used elsewhere again.
        unsafe {
            let mut srv: Option<ID3D11ShaderResourceView> = None;
            self.device
                .CreateShaderResourceView(src, None, Some(&mut srv))?;
            let mut rtv: Option<ID3D11RenderTargetView> = None;
            self.device
                .CreateRenderTargetView(dst, None, Some(&mut rtv))?;
            let ctx = &self.context;
            ctx.UpdateSubresource(&self.cbuf, 0, None, consts.as_ptr().cast(), 0, 0);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cbuf.clone())]));
            let scaled = (dst_w, dst_h) != (src_rect.width as u32, src_rect.height as u32);
            let sampler = if scaled { &self.linear } else { &self.sampler };
            ctx.PSSetSamplers(0, Some(&[Some(sampler.clone())]));
            ctx.PSSetShaderResources(0, Some(&[srv]));
            ctx.RSSetState(&self.raster);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: dst_x as f32,
                TopLeftY: dst_y as f32,
                Width: dst_w as f32,
                Height: dst_h as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            ctx.OMSetRenderTargets(Some(&[rtv]), None);
            ctx.Draw(3, 0);
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(Some(&[None]), None);
        }
        Ok(())
    }
}

/// HDR state of one monitor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrInfo {
    /// Output colour space is HDR10 (`G2084`/BT.2020): Advanced Color is active.
    pub hdr: bool,
    /// SDR content white level in nits (80 when unknown).
    pub sdr_white_nits: f32,
}

impl Default for HdrInfo {
    fn default() -> Self {
        Self {
            hdr: false,
            sdr_white_nits: crate::hdr::SCRGB_WHITE_NITS,
        }
    }
}

/// Query the DXGI output of `monitor`: HDR when its colour space is
/// `RGB_FULL_G2084_NONE_P2020`. Never fails (SDR is the safe answer).
pub fn monitor_hdr(monitor: usize) -> HdrInfo {
    let gdi_name = gdi_name_of(HMONITOR(monitor as *mut _)).unwrap_or_default();
    let hdr = dxgi_is_hdr(HMONITOR(monitor as *mut _)).unwrap_or(false);
    HdrInfo {
        hdr,
        sdr_white_nits: if hdr {
            crate::displaycfg::sdr_white_nits(&gdi_name).unwrap_or(crate::hdr::SCRGB_WHITE_NITS)
        } else {
            crate::hdr::SCRGB_WHITE_NITS
        },
    }
}

fn gdi_name_of(m: HMONITOR) -> Option<String> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: cbSize matches the extended struct; the pointer is valid for the call.
    unsafe { GetMonitorInfoW(m, (&raw mut info).cast()) }
        .as_bool()
        .then(|| {
            let n = info.szDevice.iter().position(|&c| c == 0).unwrap_or(32);
            String::from_utf16_lossy(&info.szDevice[..n])
        })
}

fn dxgi_is_hdr(monitor: HMONITOR) -> Option<bool> {
    // SAFETY: standard DXGI enumeration; all objects are released on drop.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                if output.GetDesc().ok()?.Monitor == monitor {
                    let o6: IDXGIOutput6 = output.cast().ok()?;
                    let d = o6.GetDesc1().ok()?;
                    return Some(d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020);
                }
                o += 1;
            }
            a += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::{f16_bits_to_f32, f32_to_f16_bits, to_u8, tonemap};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
        D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SUBRESOURCE_DATA, D3D11_USAGE_STAGING,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC,
    };

    fn tex(
        dev: &ID3D11Device,
        w: u32,
        h: u32,
        fmt: DXGI_FORMAT,
        init: Option<&[u8]>,
        pitch: u32,
    ) -> ID3D11Texture2D {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: fmt,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            ..Default::default()
        };
        let data = init.map(|b| D3D11_SUBRESOURCE_DATA {
            pSysMem: b.as_ptr().cast(),
            SysMemPitch: pitch,
            SysMemSlicePitch: 0,
        });
        let mut t = None;
        // SAFETY: valid descriptor, initial data outlives the call.
        unsafe { dev.CreateTexture2D(&desc, data.as_ref().map(std::ptr::from_ref), Some(&mut t)) }
            .unwrap();
        t.unwrap()
    }

    fn read_bgra(dev: &ID3D11Device, ctx: &ID3D11DeviceContext, t: &ID3D11Texture2D) -> Vec<u8> {
        let mut d = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: getters, staging copy and map of our own textures.
        unsafe {
            t.GetDesc(&mut d);
            d.Usage = D3D11_USAGE_STAGING;
            d.BindFlags = 0;
            d.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            let mut st = None;
            dev.CreateTexture2D(&d, None, Some(&mut st)).unwrap();
            let st = st.unwrap();
            ctx.CopyResource(&st, t);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m)).unwrap();
            let mut out = Vec::new();
            for y in 0..d.Height {
                let row = std::slice::from_raw_parts(
                    (m.pData as *const u8).add((y * m.RowPitch) as usize),
                    (d.Width * 4) as usize,
                );
                out.extend_from_slice(row);
            }
            ctx.Unmap(&st, 0);
            out
        }
    }

    /// The GPU shader must agree with the CPU reference (this runs on SDR machines
    /// too: the source is a synthetic scRGB texture).
    #[test]
    #[ignore = "needs a D3D11 GPU"]
    fn gpu_tonemap_matches_the_cpu_reference() {
        crate::com::ensure_mta();
        let (dev, ctx) = crate::screen::create_device().unwrap();
        let conv = Converter::new(&dev, &ctx).unwrap();
        let px: Vec<[f32; 3]> = vec![
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.2158, 0.2158, 0.2158],
            [3.0, 3.0, 3.0],
            [4.0, 2.0, 1.0],
            [0.5, 0.25, 0.1],
            [-0.5, 0.2, 7.0],
            [0.02, 0.9, 0.4],
        ];
        for white in [80.0f32, 240.0] {
            let mut bytes = Vec::new();
            for p in &px {
                for c in [p[0], p[1], p[2], 1.0] {
                    bytes.extend_from_slice(&f32_to_f16_bits(c).to_le_bytes());
                }
            }
            let n = px.len() as u32;
            let src = tex(
                &dev,
                n,
                1,
                DXGI_FORMAT_R16G16B16A16_FLOAT,
                Some(&bytes),
                n * 8,
            );
            let dst = tex(&dev, n, 1, DXGI_FORMAT_B8G8R8A8_UNORM, None, 0);
            let r = Rect {
                x: 0,
                y: 0,
                width: n as i32,
                height: 1,
            };
            conv.draw(&src, r, &dst, (0, 0), (n, 1), true, white)
                .unwrap();
            let out = read_bgra(&dev, &ctx, &dst);
            for (i, p) in px.iter().enumerate() {
                // The CPU reference sees half-float rounded inputs, like the GPU.
                let h = p.map(|v| f16_bits_to_f32(f32_to_f16_bits(v)));
                let want = tonemap(h, white).map(to_u8);
                let got = [out[i * 4 + 2], out[i * 4 + 1], out[i * 4]];
                for c in 0..3 {
                    assert!(
                        (i32::from(got[c]) - i32::from(want[c])).abs() <= 2,
                        "pixel {i} white {white}: gpu {got:?} cpu {want:?}"
                    );
                }
            }
        }
    }

    #[test]
    #[ignore = "needs a D3D11 GPU"]
    fn sdr_passthrough_copies_pixels_and_supports_offsets() {
        crate::com::ensure_mta();
        let (dev, ctx) = crate::screen::create_device().unwrap();
        let conv = Converter::new(&dev, &ctx).unwrap();
        let src_px: Vec<u8> = (0..4u8)
            .flat_map(|i| [10 + i, 20 + i, 30 + i, 255])
            .collect();
        let src = tex(&dev, 4, 1, DXGI_FORMAT_B8G8R8A8_UNORM, Some(&src_px), 16);
        let dst = tex(&dev, 8, 2, DXGI_FORMAT_B8G8R8A8_UNORM, None, 0);
        let r = Rect {
            x: 1,
            y: 0,
            width: 2,
            height: 1,
        };
        conv.draw(&src, r, &dst, (5, 1), (2, 1), false, 80.0)
            .unwrap();
        let out = read_bgra(&dev, &ctx, &dst);
        let at = |x: usize, y: usize| &out[(y * 8 + x) * 4..(y * 8 + x) * 4 + 3];
        assert_eq!(at(5, 1), &[11, 21, 31]);
        assert_eq!(at(6, 1), &[12, 22, 32]);
        assert_eq!(at(4, 1), &[0, 0, 0], "outside the viewport is untouched");
    }

    #[test]
    #[ignore = "needs a D3D11 GPU"]
    fn downscaling_averages_neighbouring_pixels() {
        crate::com::ensure_mta();
        let (dev, ctx) = crate::screen::create_device().unwrap();
        let conv = Converter::new(&dev, &ctx).unwrap();
        // 4x2 source: left half 0, right half 200; scaled to 2x1 each output pixel is
        // the mean of a 2x2 block.
        let mut px = Vec::new();
        for y in 0..2u8 {
            for x in 0..4u8 {
                let v = if x < 2 { 0 } else { 200 };
                let v = if y == 1 && x == 1 { 100 } else { v };
                px.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let src = tex(&dev, 4, 2, DXGI_FORMAT_B8G8R8A8_UNORM, Some(&px), 16);
        let dst = tex(&dev, 2, 1, DXGI_FORMAT_B8G8R8A8_UNORM, None, 0);
        let r = Rect {
            x: 0,
            y: 0,
            width: 4,
            height: 2,
        };
        conv.draw(&src, r, &dst, (0, 0), (2, 1), false, 80.0)
            .unwrap();
        let out = read_bgra(&dev, &ctx, &dst);
        assert!((i32::from(out[0]) - 25).abs() <= 2, "left {}", out[0]);
        assert!((i32::from(out[4]) - 200).abs() <= 2, "right {}", out[4]);
    }

    #[test]
    #[ignore = "needs a desktop"]
    fn this_machines_monitors_report_a_sane_hdr_state() {
        for m in crate::sources::list_monitors().unwrap() {
            let handle = crate::sources::monitor_handle(&m.id).unwrap();
            let h = monitor_hdr(handle);
            assert!(h.sdr_white_nits >= 80.0, "{h:?}");
            eprintln!("{} hdr={} sdr_white={} nits", m.id, h.hdr, h.sdr_white_nits);
        }
    }
}
