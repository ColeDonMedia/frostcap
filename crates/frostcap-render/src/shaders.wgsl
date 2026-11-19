// Frostcap export compositor. All colour math is done on sRGB-encoded values
// (like Canvas 2D, which the editor preview uses), stored in Rgba8Unorm.

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> VsOut {
    // One oversized triangle covering the target.
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    var o: VsOut;
    o.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    o.uv = vec2<f32>(x, y);
    return o;
}

// ---------------------------------------------------------------- NV12 -> RGB

struct ConvU { p: vec4<f32> }; // x: bt709 (1/0), y: full range (1/0)
@group(0) @binding(0) var<uniform> cu: ConvU;
@group(0) @binding(1) var c_samp: sampler;
@group(0) @binding(2) var c_y: texture_2d<f32>;
@group(0) @binding(3) var c_uv: texture_2d<f32>;

@fragment
fn fs_nv12(in: VsOut) -> @location(0) vec4<f32> {
    let yv = textureSampleLevel(c_y, c_samp, in.uv, 0.0).r;
    let c = textureSampleLevel(c_uv, c_samp, in.uv, 0.0).rg;
    var y: f32;
    var cb: f32;
    var cr: f32;
    if (cu.p.y > 0.5) {
        y = yv;
        cb = c.x - 128.0 / 255.0;
        cr = c.y - 128.0 / 255.0;
    } else {
        y = (yv * 255.0 - 16.0) / 219.0;
        cb = (c.x * 255.0 - 128.0) / 224.0;
        cr = (c.y * 255.0 - 128.0) / 224.0;
    }
    var rgb: vec3<f32>;
    if (cu.p.x > 0.5) {
        rgb = vec3<f32>(y + 1.5748 * cr, y - 0.187324 * cb - 0.468124 * cr, y + 1.8556 * cb);
    } else {
        rgb = vec3<f32>(y + 1.402 * cr, y - 0.344136 * cb - 0.714136 * cr, y + 1.772 * cb);
    }
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}

// ---------------------------------------------------------------- mip downsample

@group(0) @binding(0) var d_samp: sampler;
@group(0) @binding(1) var d_src: texture_2d<f32>;

@fragment
fn fs_down(in: VsOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(d_src, d_samp, in.uv, 0.0);
}

// ---------------------------------------------------------------- RGB -> NV12 planes (BT.709 limited)

@group(0) @binding(0) var e_src: texture_2d<f32>;

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment
fn fs_y(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureLoad(e_src, vec2<i32>(in.pos.xy), 0).rgb;
    return vec4<f32>((16.0 + 219.0 * luma(c)) / 255.0, 0.0, 0.0, 1.0);
}

@fragment
fn fs_uv(in: VsOut) -> @location(0) vec4<f32> {
    let dim = vec2<i32>(textureDimensions(e_src)) - vec2<i32>(1);
    let p = vec2<i32>(in.pos.xy) * 2;
    let c = (textureLoad(e_src, min(p, dim), 0).rgb + textureLoad(e_src, min(p + vec2<i32>(1, 0), dim), 0).rgb
        + textureLoad(e_src, min(p + vec2<i32>(0, 1), dim), 0).rgb + textureLoad(e_src, min(p + vec2<i32>(1, 1), dim), 0).rgb) * 0.25;
    let y = luma(c);
    let cb = (c.b - y) / 1.8556;
    let cr = (c.r - y) / 1.5748;
    return vec4<f32>((128.0 + 224.0 * cb) / 255.0, (128.0 + 224.0 * cr) / 255.0, 0.0, 1.0);
}

// ---------------------------------------------------------------- composite

struct U {
    size: vec4<f32>,        // W, H, unit*s, video lod
    grad: vec4<f32>,        // x0, y0, x1, y1
    gradn: vec4<f32>,       // stop count
    stops: array<vec4<f32>, 8>,
    shadow_rect: vec4<f32>, // x0, y0, x1, y1
    shadow: vec4<f32>,      // radius, sigma, alpha, enabled
    win: vec4<f32>,         // x0, y0, x1, y1
    win2: vec4<f32>,        // radius, bar h, frame kind
    vid: vec4<f32>,         // x0, y0, x1, y1
    rip: vec4<f32>,         // count, line width
    ripples: array<vec4<f32>, 8>, // x, y, r, k
    cur: vec4<f32>,         // count, px per cursor unit
    cursors: array<vec4<f32>, 4>, // x, y, alpha
    cam: vec4<f32>,         // x0, y0, x1, y1
    cam2: vec4<f32>,        // radius, shadow sigma, enabled, lod
    cam_uv: vec4<f32>,      // u0, v0, u1, v1
    cap: vec4<f32>,         // x0, y0, x1, y1
    keys: vec4<f32>,        // x0, y0, x1, y1
    keys2: vec4<f32>,       // alpha
    dev: vec4<f32>,         // laptop base h, base overhang, webcam border w, has background image
    cam_border: vec4<f32>,  // webcam border colour
    cam_fx: vec4<f32>,      // brightness, contrast, saturation, shadow strength
    key: vec4<f32>,         // key colour rgb, similarity
    key2: vec4<f32>,        // smoothness, spill, key enabled, mirror
    bgk: vec4<f32>,         // webcam bg kind, blur, gradient angle (rad)
    bgc1: vec4<f32>,
    bgc2: vec4<f32>,
    ann_n: vec4<f32>,       // count, text layer present
    ann: array<vec4<f32>, 32>, // per annotation: geom, colour+alpha, (kind, stroke, fill, radius), (strength, dim)
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var t_video: texture_2d<f32>;
@group(0) @binding(3) var t_cam: texture_2d<f32>;
@group(0) @binding(4) var t_cap: texture_2d<f32>;
@group(0) @binding(5) var t_keys: texture_2d<f32>;
@group(0) @binding(6) var t_bg: texture_2d<f32>;
@group(0) @binding(7) var t_cambg: texture_2d<f32>;
@group(0) @binding(8) var t_ann: texture_2d<f32>;

fn sd_tri(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>, c: vec2<f32>) -> f32 {
    let e0 = b - a;
    let e1 = c - b;
    let e2 = a - c;
    let v0 = p - a;
    let v1 = p - b;
    let v2 = p - c;
    let pq0 = v0 - e0 * clamp(dot(v0, e0) / dot(e0, e0), 0.0, 1.0);
    let pq1 = v1 - e1 * clamp(dot(v1, e1) / dot(e1, e1), 0.0, 1.0);
    let pq2 = v2 - e2 * clamp(dot(v2, e2) / dot(e2, e2), 0.0, 1.0);
    let s = sign(e0.x * e2.y - e0.y * e2.x);
    let d = min(min(vec2<f32>(dot(pq0, pq0), s * (v0.x * e0.y - v0.y * e0.x)),
                    vec2<f32>(dot(pq1, pq1), s * (v1.x * e1.y - v1.y * e1.x))),
                    vec2<f32>(dot(pq2, pq2), s * (v2.x * e2.y - v2.y * e2.x)));
    return -sqrt(d.x) * sign(d.y);
}

fn cbcr(c: vec3<f32>) -> vec2<f32> {
    let y = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    return vec2<f32>((c.b - y) / 1.8556, (c.r - y) / 1.5748);
}

fn over(dst: vec3<f32>, src: vec3<f32>, a: f32) -> vec3<f32> {
    return mix(dst, src, clamp(a, 0.0, 1.0));
}

fn cov(d: f32) -> f32 {
    return clamp(0.5 - d, 0.0, 1.0);
}

fn sd_rrect(p: vec2<f32>, lo: vec2<f32>, hi: vec2<f32>, r: f32) -> f32 {
    let c = (lo + hi) * 0.5;
    let hs = (hi - lo) * 0.5;
    let rr = min(r, min(hs.x, hs.y));
    let q = abs(p - c) - hs + vec2<f32>(rr);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - rr;
}

fn sd_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
    return length(pa - ba * h);
}

fn erf2(x: vec2<f32>) -> vec2<f32> {
    let s = sign(x);
    let a = abs(x);
    var r = 1.0 + (0.278393 + (0.230389 + 0.078108 * (a * a)) * a) * a;
    r = r * r;
    return s - s / (r * r);
}

fn erf1(x: f32) -> f32 {
    return erf2(vec2<f32>(x, 0.0)).x;
}

fn gauss(x: f32, sigma: f32) -> f32 {
    return exp(-(x * x) / (2.0 * sigma * sigma)) / (2.5066283 * sigma);
}

fn shadow_x(x: f32, y: f32, sigma: f32, corner: f32, half: vec2<f32>) -> f32 {
    let delta = min(half.y - corner - abs(y), 0.0);
    let curved = half.x - corner + sqrt(max(0.0, corner * corner - delta * delta));
    let integral = 0.5 + 0.5 * erf2((x + vec2<f32>(-curved, curved)) * (0.70710678 / sigma));
    return integral.y - integral.x;
}

// Gaussian-blurred rounded rect coverage (Evan Wallace's closed form).
fn rrect_shadow(lo: vec2<f32>, hi: vec2<f32>, p0: vec2<f32>, sigma: f32, r: f32) -> f32 {
    let center = (lo + hi) * 0.5;
    let half = (hi - lo) * 0.5;
    let corner = min(r, min(half.x, half.y));
    let p = p0 - center;
    let low = p.y - half.y;
    let high = p.y + half.y;
    let start = clamp(-3.0 * sigma, low, high);
    let end = clamp(3.0 * sigma, low, high);
    let step = (end - start) / 4.0;
    var y = start + step * 0.5;
    var v = 0.0;
    for (var i = 0; i < 4; i = i + 1) {
        v = v + shadow_x(p.x, p.y - y, sigma, corner, half) * gauss(y, sigma) * step;
        y = y + step;
    }
    return v;
}

const CURSOR = array<vec2<f32>, 7>(
    vec2<f32>(0.0, 0.0), vec2<f32>(0.0, 22.0), vec2<f32>(5.5, 17.0), vec2<f32>(9.5, 26.0),
    vec2<f32>(13.0, 24.5), vec2<f32>(9.0, 15.5), vec2<f32>(16.0, 15.5));

fn sd_cursor(p: vec2<f32>) -> f32 {
    var d = dot(p - CURSOR[0], p - CURSOR[0]);
    var s = 1.0;
    var j = 6;
    for (var i = 0; i < 7; i = i + 1) {
        let vi = CURSOR[i];
        let vj = CURSOR[j];
        let e = vj - vi;
        let w = p - vi;
        let b = w - e * clamp(dot(w, e) / dot(e, e), 0.0, 1.0);
        d = min(d, dot(b, b));
        let c1 = p.y >= vi.y;
        let c2 = p.y < vj.y;
        let c3 = e.x * w.y > e.y * w.x;
        if ((c1 && c2 && c3) || (!c1 && !c2 && !c3)) {
            s = -s;
        }
        j = i;
    }
    return s * sqrt(d);
}

fn srgb(r: f32, g: f32, b: f32) -> vec3<f32> {
    return vec3<f32>(r, g, b) / 255.0;
}

@fragment
fn fs_composite(in: VsOut) -> @location(0) vec4<f32> {
    let p = in.pos.xy;
    let us = u.size.z;

    // 1. background gradient
    let g0 = u.grad.xy;
    let gd = u.grad.zw - g0;
    let n = i32(u.gradn.x);
    var col: vec3<f32>;
    if (n <= 1) {
        col = u.stops[0].rgb * u.stops[0].a;
    } else {
        let t = clamp(dot(p - g0, gd) / max(dot(gd, gd), 1e-6), 0.0, 1.0);
        let f = t * f32(n - 1);
        let i = min(i32(floor(f)), n - 2);
        let c = mix(u.stops[i], u.stops[i + 1], f - f32(i));
        col = c.rgb * c.a;
    }

    // 1b. background image (premultiplied, output-sized)
    if (u.dev.w > 0.5) {
        let b = textureLoad(t_bg, vec2<i32>(p), 0);
        col = col * (1.0 - b.a) + b.rgb;
    }

    // 2. window shadow
    if (u.shadow.w > 0.5) {
        let s = rrect_shadow(u.shadow_rect.xy, u.shadow_rect.zw, p, u.shadow.y, u.shadow.x);
        col = over(col, vec3<f32>(0.0), u.shadow.z * s);
    }

    // 3. window: base fill, then (clipped) title bar + video
    let wlo = u.win.xy;
    let whi = u.win.zw;
    let kind = i32(u.win2.z + 0.5);
    let bezel = u.win2.w;
    if (kind == 4) {
        // laptop keyboard base: a metal slab under the lid, wider than the lid
        let blo = vec2<f32>(wlo.x - u.dev.y, whi.y - 10.0 * us);
        let bhi = vec2<f32>(whi.x + u.dev.y, whi.y + u.dev.x);
        let t = clamp((p.y - whi.y) / max(u.dev.x, 1e-3), 0.0, 1.0);
        let metal = mix(srgb(216.0, 219.0, 226.0), srgb(150.0, 155.0, 166.0), t);
        col = over(col, metal, cov(sd_rrect(p, blo, bhi, 8.0 * us)));
        let cx = (wlo.x + whi.x) * 0.5;
        let notch = sd_rrect(p, vec2<f32>(cx - 36.0 * us, whi.y - 2.0 * us), vec2<f32>(cx + 36.0 * us, whi.y + 5.0 * us), 4.0 * us);
        col = over(col, srgb(120.0, 125.0, 136.0), 0.6 * cov(notch));
    }
    let wc = cov(sd_rrect(p, wlo, whi, u.win2.x));
    if (wc > 0.0) {
        var body = srgb(17.0, 20.0, 28.0);
        if (kind == 4) {
            body = srgb(12.0, 13.0, 17.0);
        } else if (kind == 5) {
            body = srgb(8.0, 9.0, 12.0);
        }
        col = over(col, body, wc);
        let bar_h = u.win2.y;
        var inner = vec3<f32>(0.0);
        let uv = (p - u.vid.xy) / max(u.vid.zw - u.vid.xy, vec2<f32>(1e-3));
        let video = textureSampleLevel(t_video, samp, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)), u.size.w).rgb;
        inner = video;
        if (kind >= 4) {
            // device: the video sits inside the bezel
            let ir = select(4.0 * us, max(u.win2.x - bezel, 2.0 * us), kind == 5);
            inner = mix(body, video, cov(sd_rrect(p, u.vid.xy, u.vid.zw, ir)));
            let cx = (u.vid.x + u.vid.z) * 0.5;
            if (kind == 5) {
                let c = vec2<f32>(cx, u.vid.y + 12.0 * us);
                inner = over(inner, srgb(6.0, 6.0, 9.0), cov(sd_rrect(p, c - vec2<f32>(34.0 * us, 8.0 * us), c + vec2<f32>(34.0 * us, 8.0 * us), 8.0 * us)));
            } else {
                let c = vec2<f32>(cx, wlo.y + bezel * 0.5);
                inner = over(inner, srgb(44.0, 48.0, 64.0), cov(length(p - c) - 2.5 * us));
            }
        }
        if (bar_h > 0.0) {
            var bar = srgb(28.0, 32.0, 48.0);
            if (kind == 2) {
                bar = srgb(32.0, 32.0, 32.0);
            }
            let cy = wlo.y + bar_h * 0.5;
            if (kind == 1) {
                let cx = (wlo.x + whi.x) * 0.5;
                let d = sd_rrect(p, vec2<f32>(cx - 40.0 * us, cy - 3.0 * us), vec2<f32>(cx + 40.0 * us, cy + 3.0 * us), 3.0 * us);
                bar = over(bar, vec3<f32>(1.0), 0.14 * cov(d));
            } else if (kind == 3) {
                let lights = array<vec3<f32>, 3>(srgb(255.0, 95.0, 87.0), srgb(254.0, 188.0, 46.0), srgb(40.0, 200.0, 64.0));
                for (var i = 0; i < 3; i = i + 1) {
                    let c = vec2<f32>(wlo.x + 18.0 * us + f32(i) * 18.0 * us, cy);
                    bar = over(bar, lights[i], cov(length(p - c) - 5.5 * us));
                }
            } else if (kind == 2) {
                let x0 = whi.x - 20.0 * us;
                let hw = 0.6 * us;
                var d = sd_segment(p, vec2<f32>(x0 - 5.0 * us, cy - 5.0 * us), vec2<f32>(x0 + 5.0 * us, cy + 5.0 * us));
                d = min(d, sd_segment(p, vec2<f32>(x0 + 5.0 * us, cy - 5.0 * us), vec2<f32>(x0 - 5.0 * us, cy + 5.0 * us)));
                d = min(d, sd_segment(p, vec2<f32>(x0 - 100.0 * us, cy), vec2<f32>(x0 - 90.0 * us, cy)));
                let sq = abs(sd_rrect(p, vec2<f32>(x0 - 50.0 * us, cy - 5.0 * us), vec2<f32>(x0 - 40.0 * us, cy + 5.0 * us), 0.0));
                d = min(d, sq);
                bar = over(bar, srgb(200.0, 200.0, 200.0), cov(d - hw));
            }
            let in_bar = clamp(wlo.y + bar_h - p.y + 0.5, 0.0, 1.0);
            inner = mix(video, bar, in_bar);
        }
        col = over(col, inner, wc);
    }

    // 3b. annotations over the screen content (after the zoom transform)
    let vclip = cov(sd_rrect(p, u.vid.xy, u.vid.zw, 0.0));
    if (vclip > 0.0) {
        let vuv_scale = max(u.vid.zw - u.vid.xy, vec2<f32>(1e-3));
        for (var i = 0; i < i32(u.ann_n.x); i = i + 1) {
            let g = u.ann[i * 4];
            let cl = u.ann[i * 4 + 1];
            let m = u.ann[i * 4 + 2];
            let q = u.ann[i * 4 + 3];
            let kind = i32(m.x + 0.5);
            let sw = m.y;
            let al = cl.w * vclip;
            if (kind == 0) {
                let a = g.xy;
                let b = g.zw;
                let dv = b - a;
                let len = max(length(dv), 1e-3);
                let dir = dv / len;
                let perp = vec2<f32>(-dir.y, dir.x);
                let hl = min(max(sw * 3.5, 14.0 * us), len);
                let hw = hl * 0.55;
                let base = b - dir * hl;
                let ds = sd_segment(p, a, b - dir * hl * 0.8) - sw * 0.5;
                let dt = sd_tri(p, b, base + perp * hw, base - perp * hw);
                col = over(col, cl.rgb, al * cov(min(ds, dt)));
            } else if (kind == 2 || kind == 3 || kind == 4 || kind == 5 || kind == 6 || kind == 7) {
                var d: f32;
                if (kind == 3) {
                    let c = (g.xy + g.zw) * 0.5;
                    let r = max((g.zw - g.xy) * 0.5, vec2<f32>(1e-3));
                    d = (length((p - c) / r) - 1.0) * min(r.x, r.y);
                } else {
                    d = sd_rrect(p, g.xy, g.zw, m.w);
                }
                if (kind == 2 || kind == 3) {
                    var a = cov(abs(d) - sw * 0.5);
                    if (m.z > 0.5) {
                        a = cov(d);
                    }
                    col = over(col, cl.rgb, al * a);
                } else if (kind == 4) {
                    col = over(col, cl.rgb, al * 0.4 * cov(d));
                } else if (kind == 5) {
                    col = over(col, vec3<f32>(0.0), al * q.y * (1.0 - cov(d)));
                } else if (kind == 6) {
                    let uvb = (p - u.vid.xy) / vuv_scale;
                    let bl = textureSampleLevel(t_video, samp, clamp(uvb, vec2<f32>(0.0), vec2<f32>(1.0)), u.size.w + 1.0 + q.x * 5.0).rgb;
                    col = over(col, bl, al * cov(d));
                } else {
                    let block = mix(3.0, 40.0, q.x) * us;
                    let qp = g.xy + (floor((p - g.xy) / block) + vec2<f32>(0.5)) * block;
                    let uvq = (qp - u.vid.xy) / vuv_scale;
                    let px = textureSampleLevel(t_video, samp, clamp(uvq, vec2<f32>(0.0), vec2<f32>(1.0)), u.size.w + log2(max(block, 1.0))).rgb;
                    col = over(col, px, al * cov(d));
                }
            }
        }
        if (u.ann_n.y > 0.5) {
            let s = textureLoad(t_ann, vec2<i32>(p), 0);
            col = (col * (1.0 - s.a) + s.rgb) * vclip + col * (1.0 - vclip);
        }
    }

    // 4. click ripples (stroke, then fill on top, like the preview)
    let rip_col = srgb(78.0, 210.0, 251.0);
    for (var i = 0; i < i32(u.rip.x); i = i + 1) {
        let r = u.ripples[i];
        let d = length(p - r.xy) - r.z;
        col = over(col, rip_col, 0.9 * (1.0 - r.w) * cov(abs(d) - u.rip.y * 0.5));
        col = over(col, rip_col, 0.25 * (1.0 - r.w) * cov(d));
    }

    // 5. cursor (ghosts first, main last): shadow, white fill, dark outline
    let cs = u.cur.y;
    for (var i = 0; i < i32(u.cur.x); i = i + 1) {
        let c = u.cursors[i];
        let q = p - c.xy;
        if (q.x < -12.0 * cs - 8.0 || q.y < -12.0 * cs - 8.0 || q.x > 30.0 * cs + 8.0 || q.y > 40.0 * cs + 8.0) {
            continue;
        }
        let sd_sh = sd_cursor((q - vec2<f32>(0.0, 1.0)) / cs) * cs;
        let sh = 0.5 * (1.0 - erf1(sd_sh / (2.0 * 1.41421356)));
        col = over(col, vec3<f32>(0.0), 0.35 * c.z * sh);
        let sd = sd_cursor(q / cs) * cs;
        col = over(col, vec3<f32>(1.0), c.z * cov(sd));
        col = over(col, srgb(11.0, 13.0, 19.0), c.z * cov(abs(sd) - 0.8 * cs));
    }

    // 6. webcam bubble (not zoomed): colour grade, chroma key, background, border
    if (u.cam2.z > 0.5) {
        let sh = rrect_shadow(u.cam.xy, u.cam.zw, p, u.cam2.y, u.cam2.x);
        col = over(col, vec3<f32>(0.0), u.cam_fx.w * sh);
        let c = cov(sd_rrect(p, u.cam.xy, u.cam.zw, u.cam2.x));
        if (c > 0.0) {
            let l = clamp((p - u.cam.xy) / max(u.cam.zw - u.cam.xy, vec2<f32>(1e-3)), vec2<f32>(0.0), vec2<f32>(1.0));
            let lx = select(l.x, 1.0 - l.x, u.key2.w > 0.5);
            let cuv = mix(u.cam_uv.xy, u.cam_uv.zw, vec2<f32>(lx, l.y));
            let raw = textureSampleLevel(t_cam, samp, cuv, u.cam2.w).rgb;
            var cc = (raw - vec3<f32>(0.5)) * u.cam_fx.y + vec3<f32>(0.5) + vec3<f32>(u.cam_fx.x);
            let lum = dot(cc, vec3<f32>(0.2126, 0.7152, 0.0722));
            cc = clamp(mix(vec3<f32>(lum), cc, u.cam_fx.z), vec3<f32>(0.0), vec3<f32>(1.0));
            var ka = 1.0;
            var rgb = cc;
            var alpha = 1.0;
            if (u.key2.z > 0.5) {
                let thr = u.key.w * 0.5;
                let soft = u.key2.x * 0.5 + 0.001;
                let d = length(cbcr(raw) - cbcr(u.key.rgb));
                ka = smoothstep(thr, thr + soft, d);
                let sp = u.key2.y * (1.0 - smoothstep(thr, thr * 2.0 + soft, d));
                cc = mix(cc, vec3<f32>(dot(cc, vec3<f32>(0.2126, 0.7152, 0.0722))), sp);
                var bgc = vec3<f32>(0.0);
                var bga = 0.0;
                let kind = i32(u.bgk.x + 0.5);
                if (kind == 1) {
                    bgc = u.bgc1.rgb;
                    bga = u.bgc1.a;
                } else if (kind == 2) {
                    let dir = vec2<f32>(sin(u.bgk.z), -cos(u.bgk.z));
                    let t = clamp(dot(l - vec2<f32>(0.5), dir) + 0.5, 0.0, 1.0);
                    let gc = mix(u.bgc1, u.bgc2, t);
                    bgc = gc.rgb;
                    bga = gc.a;
                } else if (kind == 3) {
                    let im = textureSampleLevel(t_cambg, samp, l, 0.0);
                    bgc = im.rgb;
                    bga = im.a;
                } else if (kind == 4) {
                    let bl = textureSampleLevel(t_cam, samp, cuv, u.cam2.w + 1.0 + u.bgk.y * 5.0).rgb;
                    bgc = vec3<f32>(dot(bl, vec3<f32>(0.2126, 0.7152, 0.0722)));
                    bga = 1.0;
                }
                alpha = ka + bga * (1.0 - ka);
                rgb = (cc * ka + bgc * bga * (1.0 - ka)) / max(alpha, 1e-4);
            }
            let bw = u.dev.z;
            if (bw > 0.0) {
                let inside = cov(sd_rrect(p, u.cam.xy + vec2<f32>(bw), u.cam.zw - vec2<f32>(bw), max(u.cam2.x - bw, 0.0)));
                rgb = mix(mix(rgb, u.cam_border.rgb, u.cam_border.a), rgb, inside);
                alpha = max(alpha, (1.0 - inside) * u.cam_border.a);
            }
            col = over(col, rgb, c * alpha);
        }
    }

    // 7. captions + key overlay (premultiplied sprites)
    if (u.cap.z > u.cap.x && all(p >= u.cap.xy) && all(p < u.cap.zw)) {
        let s = textureSampleLevel(t_cap, samp, (p - u.cap.xy) / (u.cap.zw - u.cap.xy), 0.0);
        col = col * (1.0 - s.a) + s.rgb;
    }
    if (u.keys.z > u.keys.x && all(p >= u.keys.xy) && all(p < u.keys.zw)) {
        let s = textureSampleLevel(t_keys, samp, (p - u.keys.xy) / (u.keys.zw - u.keys.xy), 0.0) * u.keys2.x;
        col = col * (1.0 - s.a) + s.rgb;
    }
    return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
