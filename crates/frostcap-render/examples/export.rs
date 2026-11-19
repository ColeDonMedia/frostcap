//! Export a project folder to MP4 (or GIF) and optionally verify the result.
//!
//! ```text
//! cargo run --release -p frostcap-render --example export -- <project.frostcap> <out.mp4|gif>
//!     [--aspect 16:9|9:16|1:1|source] [--size WxH] [--fps N] [--gif]
//!     [--demo-edits] [--verify] [--sw-codecs]
//! ```
//! `--demo-edits` adds (in memory only) a 2x zoom over the middle half of the
//! recording, the "frost" gradient background, a minimal frame and a caption.
//! `--verify` parses the MP4 boxes and samples decoded pixels.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use frostcap_render::model::{Gradient, ZoomSegment};
use frostcap_render::{
    Aspect, ExportOptions, ExportPreset, OutputFormat, RenderProject, math, mf, mp4info,
};

fn fail(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pos: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let val = |f: &str| {
        args.iter()
            .position(|a| a == f)
            .and_then(|i| args.get(i + 1))
    };
    let (Some(dir), Some(out)) = (pos.first(), pos.get(1)) else {
        eprintln!(
            "usage: export <project-dir> <out> [--aspect A] [--size WxH] [--fps N] [--gif] [--demo-edits] [--verify]"
        );
        std::process::exit(2);
    };
    let dir = PathBuf::from(dir);
    let out = PathBuf::from(out);
    let mut p = RenderProject::load(&dir).unwrap_or_else(|e| fail(e.to_string()));
    if flag("--demo-edits") {
        let d = p.duration_ms;
        p.zooms = vec![ZoomSegment {
            start_ms: d * 0.25,
            end_ms: d * 0.75,
            cx: 0.5,
            cy: 0.5,
            scale: 2.0,
            easing: frostcap_render::model::Easing::EaseInOutCubic,
            transition_ms: 400.0,
        }];
        p.style.background = Gradient {
            angle_deg: 135.0,
            stops: ["#4ed2fb", "#389af5", "#0752f1", "#1151ef"]
                .iter()
                .map(|c| frostcap_render::model::parse_color(c).unwrap())
                .collect(),
        };
        p.style.frame = frostcap_render::model::Frame::Minimal;
        p.style.padding = 8.0;
        p.style.radius = 16.0;
        p.style.shadow = 60.0;
        p.captions = vec![frostcap_render::model::Cue {
            start: 0.0,
            end: d,
            text: "Frostcap export test".into(),
        }];
    }
    let aspect = match val("--aspect").map(String::as_str) {
        Some("9:16") => Aspect::Portrait,
        Some("1:1") => Aspect::Square,
        Some("source") => Aspect::Source,
        _ => Aspect::Landscape,
    };
    let (width, height) = val("--size")
        .and_then(|s| s.split_once('x'))
        .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
        .unwrap_or((0, 0));
    let preset = ExportPreset {
        aspect,
        width,
        height,
        fps: val("--fps").and_then(|f| f.parse().ok()).unwrap_or(60),
        format: if flag("--gif") {
            OutputFormat::Gif
        } else {
            OutputFormat::Mp4
        },
    };
    let opts = ExportOptions {
        hardware_codecs: !flag("--sw-codecs"),
        ..Default::default()
    };
    let cancel = AtomicBool::new(false);
    let mut last = String::new();
    let t0 = std::time::Instant::now();
    let mut stage_at: Vec<(&'static str, f64)> = Vec::new();
    let stats = frostcap_render::export_project(&p, &preset, &out, &opts, &cancel, &mut |v, s| {
        let line = format!("{:>3}% {}", (v * 100.0) as u32 / 10 * 10, s.as_str());
        if line != last {
            println!("{line}");
            last = line;
        }
        if stage_at.last().is_none_or(|l| l.0 != s.as_str()) {
            stage_at.push((s.as_str(), t0.elapsed().as_secs_f64()));
        }
    })
    .unwrap_or_else(|e| fail(e.to_string()));
    println!("{}", serde_json::to_string_pretty(&stats).unwrap());
    let timeline: Vec<String> = stage_at
        .iter()
        .map(|(s, t)| format!("{s}@{t:.2}s"))
        .collect();
    println!("stages: {}", timeline.join(" "));
    if let Some(r) = stage_at.iter().find(|s| s.0 == "rendering") {
        let end = stage_at
            .iter()
            .find(|s| s.0 == "finalizing")
            .map_or(stats.elapsed_s, |s| s.1);
        println!(
            "render loop: {} frames in {:.2}s = {:.1} fps ({:.2}x realtime excl. setup)",
            stats.frames,
            end - r.1,
            stats.frames as f64 / (end - r.1),
            stats.duration_ms / 1000.0 / (end - r.1)
        );
    }

    if !flag("--verify") {
        return;
    }
    if stats.format == OutputFormat::Gif {
        let b = std::fs::read(&out).unwrap();
        if &b[0..6] != b"GIF89a" {
            fail("not a GIF89a".into());
        }
        println!("verify: GIF89a ok, {} bytes", b.len());
        return;
    }
    let info = mp4info::parse_file(&out).unwrap_or_else(|e| fail(e.to_string()));
    println!("mp4: brand={} duration={:.3}s", info.brand, info.duration_s);
    for t in &info.tracks {
        println!(
            "  track {} {} {:.3}s samples={} size={:?} audio={:?}",
            t.handler, t.codec, t.duration_s, t.samples, t.size, t.audio
        );
    }
    let want = p.output_ms() / 1000.0;
    let video = info
        .tracks
        .iter()
        .find(|t| t.handler == "vide")
        .unwrap_or_else(|| fail("no video track".into()));
    if video.codec != "avc1" {
        fail(format!("video codec {}", video.codec));
    }
    if video.size != Some((stats.width as u16, stats.height as u16)) {
        fail(format!("video size {:?}", video.size));
    }
    if (video.duration_s - want).abs() > 2.0 / stats.fps as f64 + 0.02 {
        fail(format!("video duration {} vs {want}", video.duration_s));
    }
    if video.samples as u64 != stats.frames {
        fail(format!(
            "video samples {} vs frames {}",
            video.samples, stats.frames
        ));
    }
    if stats.audio {
        let a = info
            .tracks
            .iter()
            .find(|t| t.handler == "soun")
            .unwrap_or_else(|| fail("no audio track".into()));
        if a.codec != "mp4a" || a.audio != Some((2, 48000)) {
            fail(format!("audio {} {:?}", a.codec, a.audio));
        }
        if (a.duration_s - want).abs() > 0.1 {
            fail(format!("audio duration {} vs {want}", a.duration_s));
        }
    }

    // Pixels: decode with the MF video processor (independent of our NV12 path).
    let _g = mf::MfGuard::new().unwrap();
    let (w, h) = (stats.width as f64, stats.height as f64);
    let lay = math::layout(w, h, p.canvas.0 as f64, p.canvas.1 as f64, &p);
    let t_plain = p
        .zooms
        .first()
        .map_or(100.0, |z| (z.start_ms / 2.0).max(50.0));
    let decode = |path: &std::path::Path, t: f64| {
        // NV12 planes + CPU BT.709 by default; the MF video-processor RGB32 path
        // (--vp-verify) misreads widths that are not multiples of 16.
        if !flag("--vp-verify") {
            mf::decode_frame_rgba_nv12(path, t)
        } else {
            mf::decode_frame_rgba(path, t)
        }
    };
    let (fw, fh, rgba) = decode(&out, t_plain).unwrap_or_else(|e| fail(e.to_string()));
    println!("  decoded frame {fw}x{fh} at {t_plain:.0} ms");
    if let Some(png) = val("--dump-png") {
        write_png(std::path::Path::new(png), fw, fh, &rgba);
        println!("  wrote {png}");
    }
    let px = |x: f64, y: f64| {
        let o = ((y as usize).min(fh as usize - 1) * fw as usize
            + (x as usize).min(fw as usize - 1))
            * 4;
        [rgba[o] as f64, rgba[o + 1] as f64, rgba[o + 2] as f64]
    };
    let mut worst = 0.0f64;
    let probes = [
        (4.0, 4.0),
        (w - 5.0, 4.0),
        (4.0, h - 5.0),
        (w - 5.0, h - 5.0),
        (lay.window.x / 2.0, h / 2.0),
        (w / 2.0, lay.window.y / 3.0),
    ];
    let shadow = frostcap_render::scene::build(&p, w, h, p.source_time(t_plain), false).shadow;
    for (x, y) in probes {
        // Skip probes the window shadow can reach (beyond 3 sigma it is < 0.3 %).
        if let Some(s) = shadow {
            let dx = (s.rect.x - x).max(x - s.rect.x1()).max(0.0);
            let dy = (s.rect.y - y).max(y - s.rect.y1()).max(0.0);
            if dx.hypot(dy) < 3.0 * s.sigma {
                println!(
                    "  bg probe ({x:.0},{y:.0}) skipped: inside the drop shadow (3 sigma = {:.0}px)",
                    3.0 * s.sigma
                );
                continue;
            }
        }
        let got = px(x, y);
        let exp = math::gradient_color(&p.style.background, w, h, x + 0.5, y + 0.5);
        let err = (0..3)
            .map(|i| (got[i] - exp[i] * 255.0).abs())
            .fold(0.0, f64::max);
        println!(
            "  bg probe ({x:.0},{y:.0}) got {got:?} expected {:?} err {err:.1}",
            exp.map(|c| (c * 255.0).round())
        );
        worst = worst.max(err);
    }
    if worst > 12.0 {
        fail(format!(
            "background/padding mismatch, worst channel error {worst:.1}"
        ));
    }
    // Inside the window the video is shown, which differs from the gradient there.
    let c = (
        lay.content.x + lay.content.w / 2.0,
        lay.content.y + lay.content.h / 2.0,
    );
    let got = px(c.0, c.1);
    let exp = math::gradient_color(&p.style.background, w, h, c.0, c.1);
    let diff = (0..3)
        .map(|i| (got[i] - exp[i] * 255.0).abs())
        .fold(0.0, f64::max);
    println!(
        "  video probe {c:?} got {got:?} (gradient would be {:?})",
        exp.map(|v| (v * 255.0).round())
    );
    if diff < 20.0 {
        fail("centre looks like background, video missing?".into());
    }
    if let Some(z) = p.zooms.first() {
        // Mid-zoom (2x around the centre) the content fills the whole frame.
        let src_mid = (z.start_ms + z.end_ms) / 2.0;
        let mut acc = 0.0;
        let mut t_out = 0.0;
        // Find output time of src_mid (first piece containing it).
        for pc in &p.pieces {
            if src_mid >= pc.src_start && src_mid < pc.src_end {
                t_out = acc + (src_mid - pc.src_start) / pc.speed;
                break;
            }
            acc += pc.out_len();
        }
        let (_, _, zr) = mf::decode_frame_rgba(&out, t_out).unwrap_or_else(|e| fail(e.to_string()));
        let o = (4 * fw as usize + 4) * 4;
        let corner = [zr[o] as f64, zr[o + 1] as f64, zr[o + 2] as f64];
        let exp = math::gradient_color(&p.style.background, w, h, 4.5, 4.5);
        let err = (0..3)
            .map(|i| (corner[i] - exp[i] * 255.0).abs())
            .fold(0.0, f64::max);
        println!(
            "  zoom probe t_out={t_out:.0}ms corner {corner:?} vs gradient {:?} diff {err:.1}",
            exp.map(|v| (v * 255.0).round())
        );
        // Only meaningful when the zoomed video covers the corner (not the case
        // for e.g. a 2x zoom of landscape content in a 9:16 frame).
        let zv = frostcap_render::scene::build(&p, w, h, src_mid, false).video;
        let covers = zv.x <= 4.0 && zv.y <= 4.0 && zv.x1() >= 5.0 && zv.y1() >= 5.0;
        if covers && err < 20.0 {
            fail("zoomed frame still shows the background in the corner".into());
        }
        if !covers {
            println!(
                "  (zoomed video does not cover the corner at this aspect; corner probe informational)"
            );
        }
        // Stronger: output pixels inside the zoomed content must match the source
        // pixels found through the inverse zoom transform (7x7 patch means).
        let xf = math::zoom_transform(w, h, &lay, &p, src_mid);
        let (sw, sh, srgb) =
            mf::decode_frame_rgba(&p.screen, src_mid).unwrap_or_else(|e| fail(e.to_string()));
        let patch = |img: &[u8], iw: u32, ih: u32, cx: f64, cy: f64, r: f64| {
            let mut s = [0.0f64; 3];
            let mut n = 0.0;
            for dy in -3..=3 {
                for dx in -3..=3 {
                    let x = (cx + dx as f64 * r).clamp(0.0, iw as f64 - 1.0) as usize;
                    let y = (cy + dy as f64 * r).clamp(0.0, ih as f64 - 1.0) as usize;
                    let o = (y * iw as usize + x) * 4;
                    for c in 0..3 {
                        s[c] += img[o + c] as f64;
                    }
                    n += 1.0;
                }
            }
            s.map(|v| v / n)
        };
        // Mean patch difference between the zoomed output and the source mapped
        // through transform `x` (samples on a 7x7 grid of output points).
        let metric = |x: &math::ZoomXf| {
            let mut total = 0.0f64;
            let mut count = 0.0f64;
            for gy in 1..8 {
                for gx in 1..8 {
                    let (ox, oy) = (w * gx as f64 / 8.0, h * gy as f64 / 8.0);
                    let cxp = (ox - w / 2.0) / x.s + x.tx;
                    let cyp = (oy - h / 2.0) / x.s + x.ty;
                    let u = (cxp - lay.content.x) / lay.content.w;
                    let v = (cyp - lay.content.y) / lay.content.h;
                    if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                        continue;
                    }
                    let scale = sw as f64 / (lay.content.w * x.s);
                    let a = patch(&zr, fw, fh, ox, oy, 1.0);
                    let b = patch(&srgb, sw, sh, u * sw as f64, v * sh as f64, scale);
                    total += (0..3).map(|c| (a[c] - b[c]).abs()).sum::<f64>() / 3.0;
                    count += 1.0;
                }
            }
            total / count.max(1.0)
        };
        let mean = metric(&xf);
        let control = metric(&math::ZoomXf::identity(w, h));
        println!(
            "  zoom transform check: s={:.3} focus=({:.1},{:.1}) mean |out - src| = {mean:.1}/255; control with the unzoomed mapping = {control:.1}/255",
            xf.s, xf.tx, xf.ty
        );
        if mean > 16.0 {
            fail(format!(
                "zoomed content does not match the source through the zoom transform ({mean:.1})"
            ));
        }
        if control < mean * 3.0 + 3.0 {
            fail(format!(
                "zoom check is not discriminative (control {control:.1} vs {mean:.1})"
            ));
        }
    }
    println!("verify: OK");
}

/// Minimal PNG writer (stored deflate blocks, no compression) for inspection.
fn write_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) {
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend(crc32(&body).to_be_bytes());
    }
    let mut raw = Vec::with_capacity((w as usize * 4 + 1) * h as usize);
    for y in 0..h as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * w as usize * 4..(y + 1) * w as usize * 4]);
    }
    let mut z = vec![0x78, 0x01];
    for (i, block) in raw.chunks(65535).enumerate() {
        let last = (i + 1) * 65535 >= raw.len();
        z.push(last as u8);
        z.extend((block.len() as u16).to_le_bytes());
        z.extend((!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    std::fs::write(path, out).expect("write png");
}
