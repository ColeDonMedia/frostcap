//! Real end-to-end check that every typed editor field reaches the exported file.
//!
//! `cargo run --release -p frostcap-render --example verify_edits -- <project-dir> [out-dir]`
//!
//! The project must have `screen.mp4` (and, for the webcam and mic checks,
//! `webcam.mp4` / `audio-mic.wav`). Each scenario takes the project, flips ONE
//! typed field (through a JSON round trip of `frostcap_project::Project`, exactly
//! what the app saves), exports a real MP4 and compares decoded frames / audio
//! with a control export of the same project. Exports are deterministic for equal
//! input, so "no change" must be a pixel-exact zero difference (checked by the
//! control). Exits non-zero on the first failed check.
#![allow(clippy::too_many_lines)]

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows only");
}

#[cfg(windows)]
fn main() {
    win::run();
}

#[cfg(windows)]
mod win {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;

    use super::write_tone;
    use frostcap_project::{
        CaptionCue, ClickEvent, Corner, CursorSample, KeyEvent, Modifiers, Project, TrimRange,
        WebcamShape, WindowFrame,
    };
    use frostcap_render::{
        Aspect, ExportOptions, ExportPreset, OutputFormat, RenderProject, audio, export_project,
        mf, mp4info,
    };

    type Frame = (u32, u32, Vec<u8>);

    fn fail(msg: String) -> ! {
        eprintln!("FAIL: {msg}");
        std::process::exit(1)
    }

    fn check(ok: bool, what: String) {
        if ok {
            println!("  ok   {what}");
        } else {
            fail(what);
        }
    }

    struct Ctx {
        dir: PathBuf,
        out: PathBuf,
    }

    impl Ctx {
        fn model(&self, p: &Project) -> RenderProject {
            // JSON round trip == what save_project / load does (typed fields only).
            let json = serde_json::to_string(p).unwrap();
            let p: Project = serde_json::from_str(&json).unwrap();
            RenderProject::from_project(&self.dir, &p).unwrap_or_else(|e| fail(e.to_string()))
        }

        fn export(&self, name: &str, p: &Project) -> PathBuf {
            let rp = self.model(p);
            let out = self.out.join(format!("{name}.mp4"));
            let preset = ExportPreset {
                aspect: Aspect::Landscape,
                width: 1280,
                height: 720,
                fps: 30,
                format: OutputFormat::Mp4,
            };
            let cancel = AtomicBool::new(false);
            let stats = export_project(
                &rp,
                &preset,
                &out,
                &ExportOptions::default(),
                &cancel,
                &mut |_, _| {},
            )
            .unwrap_or_else(|e| fail(format!("{name}: {e}")));
            println!(
                "  exported {name}: {} frames, {:.3}s, audio={}",
                stats.frames,
                stats.duration_ms / 1000.0,
                stats.audio
            );
            out
        }
    }

    fn frame(path: &Path, t_ms: f64) -> Frame {
        mf::decode_frame_rgba_nv12(path, t_ms).unwrap_or_else(|e| fail(e.to_string()))
    }

    /// (changed pixel count, bbox x0,y0,x1,y1) where any channel differs > 24.
    fn diff(a: &Frame, b: &Frame) -> (usize, Option<(u32, u32, u32, u32)>) {
        assert_eq!((a.0, a.1), (b.0, b.1));
        let (mut n, mut bb) = (0usize, None::<(u32, u32, u32, u32)>);
        for y in 0..a.1 {
            for x in 0..a.0 {
                let o = ((y * a.0 + x) * 4) as usize;
                let d = (0..3)
                    .map(|c| a.2[o + c].abs_diff(b.2[o + c]))
                    .max()
                    .unwrap();
                if d > 24 {
                    n += 1;
                    bb = Some(bb.map_or((x, y, x, y), |(x0, y0, x1, y1)| {
                        (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
                    }));
                }
            }
        }
        (n, bb)
    }

    fn px(f: &Frame, x: u32, y: u32) -> [u8; 3] {
        let o = ((y * f.0 + x) * 4) as usize;
        [f.2[o], f.2[o + 1], f.2[o + 2]]
    }

    fn rms(v: &[i16]) -> f64 {
        (v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / v.len().max(1) as f64).sqrt()
    }

    pub fn run() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let Some(dir) = args.first() else {
            eprintln!("usage: verify_edits <project-dir> [out-dir]");
            std::process::exit(2);
        };
        let dir = PathBuf::from(dir);
        let out = args
            .get(1)
            .map_or_else(|| PathBuf::from("target/smoke/edits-check"), PathBuf::from);
        std::fs::create_dir_all(&out).unwrap();
        let ctx = Ctx {
            dir: dir.clone(),
            out,
        };
        let _g = mf::MfGuard::new().unwrap();

        let mut base = Project::load(&dir).unwrap_or_else(|e| fail(e.to_string()));
        let d = base.duration_ms as f64;
        println!(
            "project {} : {:.3}s, canvas {}x{}",
            dir.display(),
            d / 1000.0,
            base.canvas.width,
            base.canvas.height
        );
        // Control configuration: no frame, no zoom, no captions, no webcam, nothing
        // that depends on the cursor or keys unless a scenario asks for it.
        base.edits.zoom_segments.clear();
        base.edits.trims.clear();
        base.edits.captions.clear();
        base.edits.splits.clear();
        base.edits.style.frame = WindowFrame::None;
        base.edits.style.show_keys = false;
        base.edits.style.webcam.enabled = false;
        base.cursor_baked = true;
        let (cw, ch) = (base.canvas.width as i32, base.canvas.height as i32);
        // Synthetic input metadata (editing metadata, nothing is injected anywhere):
        // a cursor sweeping across the canvas, and a Ctrl+S key press at 0.05 d.
        base.tracks.cursor = (0..=20)
            .map(|i| CursorSample {
                t_ms: (d * f64::from(i) / 20.0) as u64,
                x: cw / 5 + (cw * 3 / 5) * i / 20,
                y: ch / 3 + (ch / 3) * i / 20,
            })
            .collect();
        base.tracks.clicks = Vec::<ClickEvent>::new();
        base.tracks.keys = vec![KeyEvent {
            t_ms: (d * 0.05) as u64,
            vk: 0x53,
            label: "S".into(),
            modifiers: Modifiers {
                ctrl: true,
                ..Default::default()
            },
        }];
        base.edits.style.show_clicks = false;
        let t_in = d * 0.45;
        let t_out_a = d * 0.12;
        let t_out_b = d * 0.90;

        // ---------------------------------------------------------------- control
        println!("[control] two identical exports must be pixel-identical");
        let a = ctx.export("control-a", &base);
        let b = ctx.export("control-b", &base);
        let fa = frame(&a, t_in);
        let (n, _) = diff(&fa, &frame(&b, t_in));
        check(
            n == 0,
            format!("control: {n} pixels differ between identical exports (want 0)"),
        );

        // --------------------------------------------------------------- captions
        println!("[captions] cue {:.0}..{:.0} ms", d * 0.30, d * 0.60);
        let mut p = base.clone();
        p.edits.captions = vec![CaptionCue {
            start_ms: (d * 0.30) as u64,
            end_ms: (d * 0.60) as u64,
            text: "FROSTCAP CAPTION TEXT".into(),
        }];
        p.edits.style.captions.enabled = true;
        let cap = ctx.export("captions", &p);
        let (n_in, bb) = diff(&frame(&cap, t_in), &fa);
        check(
            n_in > 1500,
            format!(
                "caption text pixels present in the cue window: {n_in} changed pixels, bbox {bb:?}"
            ),
        );
        let (x0, y0, x1, y1) = bb.unwrap();
        let (fw, fh) = (fa.0, fa.1);
        check(
            y0 > fh / 2 && x0 > fw / 6 && x1 < fw * 5 / 6 && y1 < fh,
            format!(
                "caption sits in the bottom half, centred: bbox ({x0},{y0})-({x1},{y1}) of {fw}x{fh}"
            ),
        );
        for t in [t_out_a, t_out_b] {
            let (n, _) = diff(&frame(&cap, t), &frame(&a, t));
            check(
                n == 0,
                format!("caption absent outside the cue at {t:.0} ms: {n} changed pixels"),
            );
        }
        let mut top = p.clone();
        top.edits.style.captions.position = frostcap_project::CaptionPosition::Top;
        let capt = ctx.export("captions-top", &top);
        let (_, bbt) = diff(&frame(&capt, t_in), &fa);
        let (_, ty0, _, ty1) = bbt.unwrap();
        check(
            ty1 < fh / 2,
            format!("captions.position=top moves the text to the top half (y {ty0}..{ty1})"),
        );
        let mut off = p.clone();
        off.edits.style.captions.enabled = false;
        let capo = ctx.export("captions-off", &off);
        let (n, _) = diff(&frame(&capo, t_in), &fa);
        check(
            n == 0,
            format!("captions.enabled=false hides the cue: {n} changed pixels"),
        );
        let mut big = p.clone();
        big.edits.style.captions.font_size = 80.0;
        let capb = ctx.export("captions-big", &big);
        let (_, bbb) = diff(&frame(&capb, t_in), &fa);
        let (bx0, _, bx1, _) = bbb.unwrap();
        check(
            bx1 - bx0 > x1 - x0,
            format!(
                "captions.fontSize=80 is wider than 40: {} vs {} px",
                bx1 - bx0,
                x1 - x0
            ),
        );

        // ----------------------------------------------------------------- keys
        println!("[keys] Ctrl+S at {:.0} ms", d * 0.05);
        let mut p = base.clone();
        p.edits.style.show_keys = true;
        let keys = ctx.export("keys", &p);
        let t_key = d * 0.05 + 150.0;
        let (n, bb) = diff(&frame(&keys, t_key), &frame(&a, t_key));
        check(
            n > 800,
            format!("key overlay drawn just after the press: {n} changed pixels, bbox {bb:?}"),
        );
        let (n, _) = diff(&frame(&keys, t_out_b), &frame(&a, t_out_b));
        check(
            n == 0,
            format!("key overlay gone long after the press: {n} changed pixels"),
        );

        // --------------------------------------------------------------- cursor
        println!("[cursor] cursorBaked=false draws, visible=false hides, baked suppresses");
        let mut p = base.clone();
        p.cursor_baked = false;
        let cur = ctx.export("cursor-drawn", &p);
        let (n, bb) = diff(&frame(&cur, t_in), &fa);
        check(
            n > 100,
            format!("cursor drawn when cursorBaked=false: {n} changed pixels, bbox {bb:?}"),
        );
        let mut hid = p.clone();
        hid.edits.style.cursor.visible = false;
        let curh = ctx.export("cursor-hidden", &hid);
        let (n, _) = diff(&frame(&curh, t_in), &fa);
        check(
            n == 0,
            format!("cursor.visible=false hides it: {n} changed pixels vs control"),
        );
        let mut big = p.clone();
        big.edits.style.cursor_scale = 3.0;
        let curb = ctx.export("cursor-big", &big);
        let (_, bbb) = diff(&frame(&curb, t_in), &fa);
        let (cx0, cy0, cx1, cy1) = bb.unwrap();
        let (bx0, by0, bx1, by1) = bbb.unwrap();
        check(
            (bx1 - bx0) * (by1 - by0) > (cx1 - cx0) * (cy1 - cy0) * 2,
            format!(
                "cursorScale=3 is larger than 1: bbox area {} vs {}",
                (bx1 - bx0) * (by1 - by0),
                (cx1 - cx0) * (cy1 - cy0)
            ),
        );

        // --------------------------------------------------------------- webcam
        if dir.join("webcam.mp4").is_file() && base.media.webcam.is_some()
            || base.media.webcam_video.is_some()
        {
            println!("[webcam] bubble placement and shape");
            let mut boxes = Vec::new();
            for (corner, name) in [
                (Corner::TopLeft, "tl"),
                (Corner::TopRight, "tr"),
                (Corner::BottomLeft, "bl"),
                (Corner::BottomRight, "br"),
            ] {
                let mut p = base.clone();
                p.edits.style.webcam.enabled = true;
                p.edits.style.webcam.position = corner;
                p.edits.style.webcam.shape = WebcamShape::Square;
                p.edits.style.webcam.size = 0.3;
                let o = ctx.export(&format!("webcam-{name}"), &p);
                let (n, bb) = diff(&frame(&o, t_in), &fa);
                let (x0, y0, x1, y1) =
                    bb.unwrap_or_else(|| fail(format!("webcam {name}: bubble not drawn")));
                let (cxm, cym) = ((x0 + x1) / 2, (y0 + y1) / 2);
                let want = (
                    if name.ends_with('l') {
                        cxm < fw / 2
                    } else {
                        cxm > fw / 2
                    },
                    if name.starts_with('t') {
                        cym < fh / 2
                    } else {
                        cym > fh / 2
                    },
                );
                check(
                    want == (true, true),
                    format!(
                        "webcam {name}: {n} px, bbox ({x0},{y0})-({x1},{y1}) in the right quadrant of {fw}x{fh}"
                    ),
                );
                boxes.push(((x1 - x0), (y1 - y0)));
            }
            let (w0, h0) = boxes[0];
            check(
                (f64::from(h0) / f64::from(fh) - 0.3).abs() < 0.04
                    && (w0 as i32 - h0 as i32).abs() < 12,
                format!(
                    "webcam size 0.3 -> bubble {w0}x{h0}px = {:.2} of output height",
                    f64::from(h0) / f64::from(fh)
                ),
            );
            // Shape: the corner pixel of the bbox is covered by a square but not by a circle.
            let mut circle = base.clone();
            circle.edits.style.webcam.enabled = true;
            circle.edits.style.webcam.position = Corner::TopLeft;
            circle.edits.style.webcam.shape = WebcamShape::Circle;
            circle.edits.style.webcam.size = 0.3;
            let oc = ctx.export("webcam-circle", &circle);
            let fc = frame(&oc, t_in);
            let osq = ctx.out.join("webcam-tl.mp4");
            let fsq = frame(&osq, t_in);
            let (_, bbs) = diff(&fsq, &fa);
            let (x0, y0, _, _) = bbs.unwrap();
            let sq_corner = px(&fsq, x0 + 3, y0 + 3) != px(&fa, x0 + 3, y0 + 3);
            let ci_corner = px(&fc, x0 + 3, y0 + 3) != px(&fa, x0 + 3, y0 + 3);
            check(
                sq_corner && !ci_corner,
                format!(
                    "shape: bbox corner ({},{}) covered by square={sq_corner}, by circle={ci_corner} (want true/false)",
                    x0 + 3,
                    y0 + 3
                ),
            );
        } else {
            println!("[webcam] skipped: project has no webcam");
        }

        // ---------------------------------------------------------------- frame
        println!("[frame] window frame styles");
        let mut p = base.clone();
        p.edits.style.frame = WindowFrame::MacOs;
        let fm = ctx.export("frame-macos", &p);
        let (n, bb) = diff(&frame(&fm, t_in), &fa);
        check(
            n > 300,
            format!("frame=macos draws a title bar: {n} changed pixels, bbox {bb:?}"),
        );

        // --------------------------------------------------------------- trims
        println!("[trims] cut {:.0}..{:.0} ms", d * 0.25, d * 0.50);
        let mut p = base.clone();
        let cut = ((d * 0.50) as u64) - ((d * 0.25) as u64);
        p.edits.trims = vec![TrimRange {
            start_ms: (d * 0.25) as u64,
            end_ms: (d * 0.50) as u64,
        }];
        p.edits.splits = vec![(d * 0.80) as u64];
        let tr = ctx.export("trim", &p);
        let info = mp4info::parse_file(&tr).unwrap_or_else(|e| fail(e.to_string()));
        let v = info.tracks.iter().find(|t| t.handler == "vide").unwrap();
        let want = (d - cut as f64) / 1000.0;
        check(
            (v.duration_s - want).abs() < 0.1,
            format!(
                "trim shortens the output: {:.3}s (want {want:.3}s = {:.3}s - {:.3}s)",
                v.duration_s,
                d / 1000.0,
                cut as f64 / 1000.0
            ),
        );
        let mut sp = base.clone();
        sp.edits.splits = vec![(d * 0.80) as u64];
        let spo = ctx.export("splits", &sp);
        let (n, _) = diff(&frame(&spo, t_in), &fa);
        check(
            n == 0,
            format!("splits alone do not change the picture: {n} changed pixels"),
        );

        // ---------------------------------------------------------------- audio
        // ------------------------------------------------- overstated duration
        println!("[duration] project claims 3x the real length");
        let mut p = base.clone();
        p.duration_ms = (d * 3.0) as u64;
        let ov = ctx.export("overstated", &p);
        let info = mp4info::parse_file(&ov).unwrap_or_else(|e| fail(e.to_string()));
        let v = info.tracks.iter().find(|t| t.handler == "vide").unwrap();
        check(
            v.duration_s < d / 1000.0 + 0.2,
            format!(
                "export is clamped to the decoder's real duration: {:.3}s (claimed {:.3}s, real source {:.3}s)",
                v.duration_s,
                d * 3.0 / 1000.0,
                d / 1000.0
            ),
        );

        println!(
            "[audio] mic mute / gain (synthetic tone tracks written next to a copy of the project)"
        );
        {
            let adir = ctx.out.join("audio-proj");
            let _ = std::fs::remove_dir_all(&adir);
            std::fs::create_dir_all(&adir).unwrap();
            for f in ["screen.mp4", "webcam.mp4"] {
                if dir.join(f).is_file() {
                    std::fs::copy(dir.join(f), adir.join(f)).unwrap();
                }
            }
            write_tone(&adir.join("audio-mic.wav"), 440.0, 0.3, d);
            write_tone(&adir.join("audio-system.wav"), 880.0, 0.3, d);
            let track = |f: &str| frostcap_project::AudioTrack {
                file: f.into(),
                offset_ms: 0,
                sample_rate: 48_000,
                channels: 2,
                duration_ms: d as u64,
            };
            let mut base = base.clone();
            base.media.mic_audio = Some(track("audio-mic.wav"));
            base.media.system_audio = Some(track("audio-system.wav"));
            let actx = Ctx {
                dir: adir,
                out: ctx.out.clone(),
            };
            let render = |p: &Project| {
                audio::render(&actx.model(p))
                    .unwrap_or_else(|e| fail(e.to_string()))
                    .unwrap_or_else(|| fail("no audio rendered".into()))
            };
            let mut raw = base.clone();
            let s = &mut raw.edits.style.audio;
            s.noise_gate = false;
            s.high_pass = false;
            s.normalize = false;
            let with_mic = render(&raw);
            let mut muted = raw.clone();
            muted.edits.style.audio.mic_gain = 0.0;
            let no_mic = render(&muted);
            let mut only_mic = raw.clone();
            only_mic.edits.style.audio.system_gain = 0.0;
            let mic_only = render(&only_mic);
            let (r_all, r_nomic, r_mic) = (rms(&with_mic), rms(&no_mic), rms(&mic_only));
            println!(
                "  rms: mic+system={r_all:.1} system only (mic muted)={r_nomic:.1} mic only={r_mic:.1}"
            );
            check(
                r_mic > 50.0,
                format!("the project's mic track carries signal (rms {r_mic:.1})"),
            );
            check(
                r_nomic < r_all * 0.98 || r_all < 1.0,
                "muting the mic removes energy from the mix".into(),
            );
            // Mic muted with the system also muted -> exact digital silence.
            let mut none = muted.clone();
            none.edits.style.audio.system_gain = 0.0;
            check(
                rms(&render(&none)) == 0.0,
                "mic + system muted -> digital silence".into(),
            );
            let mut half = raw.clone();
            half.edits.style.audio.system_gain = 0.0;
            half.edits.style.audio.mic_gain = 0.5;
            let r_half = rms(&render(&half));
            check(
                (r_half / r_mic - 0.5).abs() < 0.03,
                format!("mic gain 0.5 halves the mic level: {:.3}", r_half / r_mic),
            );
            // And through a real export: the file still carries an audio track.
            let ao = actx.export("audio-muted-mic", &muted);
            let info = mp4info::parse_file(&ao).unwrap();
            check(
                info.tracks.iter().any(|t| t.handler == "soun"),
                "exported file has an audio track".into(),
            );
        }

        // ======================================================== editor v2 features
        v2::run(&ctx, &base, &a, &fa, t_in);
        println!("verify_edits: ALL OK");
    }

    /// Editor-v2 scenarios: speed, images, device frames, webcam margin/border/effects,
    /// annotations, batch export, GIF.
    mod v2 {
        use super::*;
        use frostcap_project::{
            Annotation, AnnotationKind, BackgroundFit, BackgroundImage, Rgba, SpeedSegment,
            VideoTrack, WebcamBackgroundKind,
        };
        use frostcap_render::{BatchJob, export_batch, math};

        fn lay(ctx: &Ctx, p: &Project) -> math::Layout {
            let rp = ctx.model(p);
            math::layout(
                1280.0,
                720.0,
                f64::from(rp.canvas.0),
                f64::from(rp.canvas.1),
                &rp,
            )
        }

        fn rgb(f: &Frame, x: f64, y: f64) -> [i32; 3] {
            let p = px(f, x as u32, y as u32);
            [i32::from(p[0]), i32::from(p[1]), i32::from(p[2])]
        }

        fn video_s(path: &Path) -> f64 {
            let info = mp4info::parse_file(path).unwrap_or_else(|e| fail(e.to_string()));
            info.tracks
                .iter()
                .find(|t| t.handler == "vide")
                .unwrap()
                .duration_s
        }

        /// Frame -> NV12 (BT.709 limited) from a per-pixel RGB function.
        fn nv12(w: u32, h: u32, f: &dyn Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
            let yuv = |c: [u8; 3]| {
                let (r, g, b) = (
                    f64::from(c[0]) / 255.0,
                    f64::from(c[1]) / 255.0,
                    f64::from(c[2]) / 255.0,
                );
                let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                (
                    (16.0 + 219.0 * y).round() as u8,
                    (128.0 + 224.0 * (b - y) / 1.8556).round() as u8,
                    (128.0 + 224.0 * (r - y) / 1.5748).round() as u8,
                )
            };
            let mut out = Vec::with_capacity((w * h * 3 / 2) as usize);
            for y in 0..h {
                for x in 0..w {
                    out.push(yuv(f(x, y)).0);
                }
            }
            for y in (0..h).step_by(2) {
                for x in (0..w).step_by(2) {
                    let (_, u, v) = yuv(f(x, y));
                    out.push(u);
                    out.push(v);
                }
            }
            out
        }

        fn tone_mag(v: &[i16], hz: f64) -> f64 {
            let w = 2.0 * std::f64::consts::PI * hz / 48_000.0;
            let (mut re, mut im) = (0.0, 0.0);
            for (i, f) in v.chunks(2).enumerate() {
                re += f64::from(f[0]) * (w * i as f64).cos();
                im += f64::from(f[0]) * (w * i as f64).sin();
            }
            (re * re + im * im).sqrt() / (v.len() / 2).max(1) as f64
        }

        pub fn run(ctx: &Ctx, base: &Project, a: &Path, fa: &Frame, t_in: f64) {
            let d = base.duration_ms as f64;
            let dir = &ctx.dir;
            speed(ctx, base, d);
            images(ctx, base, a, fa, t_in);
            devices(ctx, base, fa, t_in);
            webcam_basics(ctx, base, fa, t_in);
            annotations(ctx, base, a, fa, t_in, d);
            green_screen(ctx, base, fa, t_in);
            batch_and_gif(ctx, base, a, t_in);
            let _ = dir;
        }

        fn speed(ctx: &Ctx, base: &Project, d: f64) {
            println!("[speed] 2x over {:.0}..{:.0} ms", d * 0.25, d * 0.75);
            let (s0, s1) = ((d * 0.25) as u64, (d * 0.75) as u64);
            let seg = vec![SpeedSegment {
                start_ms: s0,
                end_ms: s1,
                speed: 2.0,
            }];
            let mut p = base.clone();
            p.edits.speed = seg.clone();
            let o = ctx.export("speed2x", &p);
            let want = (d - (s1 - s0) as f64 + (s1 - s0) as f64 / 2.0) / 1000.0;
            let got = video_s(&o);
            check(
                (got - want).abs() < 0.12,
                format!("2x segment shortens the output: {got:.3}s (want {want:.3}s)"),
            );
            let mut slow = base.clone();
            slow.edits.speed = vec![SpeedSegment {
                start_ms: s0,
                end_ms: s1,
                speed: 0.5,
            }];
            let so = ctx.export("speed0.5x", &slow);
            let want = (d + (s1 - s0) as f64) / 1000.0;
            let got = video_s(&so);
            check(
                (got - want).abs() < 0.12,
                format!("0.5x segment lengthens the output: {got:.3}s (want {want:.3}s)"),
            );
            // Frame mapping: the synthetic cursor encodes source time in its x position.
            let mut cur = base.clone();
            cur.cursor_baked = false;
            cur.edits.style.cursor_smoothing = false;
            cur.edits.style.cursor.motion_blur = 0.0;
            cur.edits.style.show_clicks = false;
            let cref = ctx.export("speed-cursor-ref", &cur);
            let mut cs = cur.clone();
            cs.edits.speed = seg.clone();
            let cso = ctx.export("speed-cursor", &cs);
            let mut ns = base.clone();
            ns.edits.speed = seg;
            let nso = ctx.export("speed-nocursor", &ns);
            let centre = |with: &Frame, without: &Frame| {
                let (n, bb) = diff(with, without);
                let (x0, _, x1, _) = bb.unwrap_or_else(|| fail("cursor not found".into()));
                assert!(n > 20);
                f64::from(x0 + x1) / 2.0
            };
            let t_out = s0 as f64 + d * 0.1; // inside the 2x part
            let t_src = s0 as f64 + d * 0.2; // what it must show
            let x_speed = centre(&frame(&cso, t_out), &frame(&nso, t_out));
            let x_ref = centre(&frame(&cref, t_src), &frame(ctx_a(ctx), t_src));
            let x_wrong = centre(&frame(&cref, t_out), &frame(ctx_a(ctx), t_out));
            check(
                (x_speed - x_ref).abs() < 14.0 && (x_speed - x_wrong).abs() > 30.0,
                format!(
                    "frame at output {t_out:.0} ms shows source {t_src:.0} ms: cursor x {x_speed:.0} (mapped ref {x_ref:.0}, unmapped would be {x_wrong:.0})"
                ),
            );
            // Audio follows: tone pitch doubles inside the 2x segment, length matches.
            let adir = ctx.out.join("audio-proj");
            if adir.join("audio-mic.wav").is_file() {
                let track = frostcap_project::AudioTrack {
                    file: "audio-mic.wav".into(),
                    offset_ms: 0,
                    sample_rate: 48_000,
                    channels: 2,
                    duration_ms: d as u64,
                };
                let mut ap = base.clone();
                ap.media.mic_audio = Some(track);
                ap.media.system_audio = None;
                ap.edits.style.audio.noise_gate = false;
                ap.edits.style.audio.high_pass = false;
                ap.edits.style.audio.normalize = false;
                ap.edits.speed = vec![SpeedSegment {
                    start_ms: s0,
                    end_ms: s1,
                    speed: 2.0,
                }];
                let actx = Ctx {
                    dir: adir,
                    out: ctx.out.clone(),
                };
                let pcm = audio::render(&actx.model(&ap))
                    .unwrap_or_else(|e| fail(e.to_string()))
                    .unwrap_or_else(|| fail("no audio".into()));
                let want_frames = (want_ms(d, s1 - s0) * 48.0).round() as usize;
                check(
                    (pcm.len() / 2).abs_diff(want_frames) < 100,
                    format!(
                        "audio length follows speed: {} frames (want ~{want_frames})",
                        pcm.len() / 2
                    ),
                );
                let at = |ms: f64| (ms * 48.0) as usize * 2;
                let seg = &pcm[at(s0 as f64 + 100.0)..at(s0 as f64 + 100.0 + 400.0)];
                let head = &pcm[at(20.0)..at(20.0 + 200.0).min(at(s0 as f64 - 20.0))];
                check(
                    tone_mag(seg, 880.0) > tone_mag(seg, 440.0) * 5.0
                        && tone_mag(head, 440.0) > tone_mag(head, 880.0) * 5.0,
                    "440 Hz tone plays at 880 Hz inside the 2x segment and unchanged before it"
                        .into(),
                );
            }
        }

        fn want_ms(d: f64, seg: u64) -> f64 {
            d - seg as f64 + seg as f64 / 2.0
        }

        /// The control export (frame style none, no cursor) used as reference.
        fn ctx_a(ctx: &Ctx) -> &Path {
            // Leaked once per process: lives for the run.
            static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
            PATH.get_or_init(|| ctx.out.join("control-a.mp4"))
        }

        fn images(ctx: &Ctx, base: &Project, a: &Path, fa: &Frame, t_in: f64) {
            println!("[background image] import, cover / contain / blur, png + jpg");
            let bdir = ctx.out.join("bg-proj");
            let _ = std::fs::remove_dir_all(&bdir);
            std::fs::create_dir_all(&bdir).unwrap();
            std::fs::copy(ctx.dir.join("screen.mp4"), bdir.join("screen.mp4")).unwrap();
            let bctx = Ctx {
                dir: bdir.clone(),
                out: ctx.out.clone(),
            };
            let logo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/brand/logo.png");
            let png = frostcap_project::import_background(&bdir, &logo)
                .unwrap_or_else(|e| fail(e.to_string()));
            check(
                png.starts_with("background-")
                    && png.ends_with(".png")
                    && bdir.join(&png).is_file(),
                format!("import_background copied the logo as {png}"),
            );
            let mut p = base.clone();
            p.edits.style.padding = 0.3;
            p.edits.style.shadow = 0.0;
            let plain = bctx.export("bg-none", &p);
            let f0 = frame(&plain, t_in);
            let with = |file: &str, fit, blur| {
                let mut q = p.clone();
                q.edits.style.background_image = Some(BackgroundImage {
                    file: file.into(),
                    fit,
                    blur,
                });
                q
            };
            let cover = bctx.export("bg-cover", &with(&png, BackgroundFit::Cover, 0.0));
            let (n, _) = diff(&frame(&cover, t_in), &f0);
            check(
                n > 3000,
                format!("PNG background changes the padding area: {n} px"),
            );
            let contain = bctx.export("bg-contain", &with(&png, BackgroundFit::Contain, 0.0));
            let (n2, _) = diff(&frame(&contain, t_in), &frame(&cover, t_in));
            check(n2 > 1000, format!("contain differs from cover: {n2} px"));
            let blur = bctx.export("bg-blur", &with(&png, BackgroundFit::Cover, 40.0));
            let (n3, _) = diff(&frame(&blur, t_in), &frame(&cover, t_in));
            check(n3 > 500, format!("blur 40 differs from sharp: {n3} px"));
            // JPEG: any system JPEG (read-only), decoded through WIC like the PNG.
            let jpg_src = [
                r"C:\Windows\Web\Wallpaper\Windows\img0.jpg",
                r"C:\Windows\Web\Wallpaper\Windows\img19.jpg",
            ]
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file());
            match jpg_src {
                Some(src) => {
                    let jpg = frostcap_project::import_background(&bdir, &src)
                        .unwrap_or_else(|e| fail(e.to_string()));
                    let o = bctx.export("bg-jpg", &with(&jpg, BackgroundFit::Cover, 0.0));
                    let (n, _) = diff(&frame(&o, t_in), &f0);
                    check(
                        jpg.ends_with(".jpg") && n > 100_000,
                        format!("JPEG background {jpg}: {n} px changed"),
                    );
                }
                None => println!("  skip JPEG: no system wallpaper found"),
            }
            let _ = (a, fa);
        }

        fn devices(ctx: &Ctx, base: &Project, fa: &Frame, t_in: f64) {
            println!("[device frames] laptop and phone");
            for (frame_kind, name) in [
                (WindowFrame::Laptop, "laptop"),
                (WindowFrame::Phone, "phone"),
            ] {
                let mut p = base.clone();
                p.edits.style.frame = frame_kind;
                p.edits.style.shadow = 0.0;
                let o = ctx.export(&format!("frame-{name}"), &p);
                let f = frame(&o, t_in);
                let (n, _) = diff(&f, fa);
                let l = lay(ctx, &p);
                let bez = rgb(
                    &f,
                    l.window.x + l.bezel / 2.0,
                    l.window.y + l.window.h / 2.0,
                );
                check(
                    n > 5000 && bez.iter().all(|c| *c < 70),
                    format!(
                        "{name}: {n} px differ from the plain frame, bezel pixel {bez:?} is dark"
                    ),
                );
                if frame_kind == WindowFrame::Laptop {
                    let base_px = rgb(
                        &f,
                        l.window.x + l.window.w / 2.0,
                        l.window.y1() + l.base_h / 2.0,
                    );
                    check(
                        base_px[0] > 110,
                        format!("laptop keyboard base is light metal: {base_px:?}"),
                    );
                } else {
                    let pill = rgb(
                        &f,
                        l.content.x + l.content.w / 2.0,
                        l.content.y + 12.0 * l.unit,
                    );
                    check(
                        pill.iter().all(|c| *c < 40),
                        format!("phone camera pill is near black: {pill:?}"),
                    );
                }
            }
        }

        fn has_webcam(ctx: &Ctx, base: &Project) -> bool {
            ctx.dir.join("webcam.mp4").is_file()
                && (base.media.webcam.is_some() || base.media.webcam_video.is_some())
        }

        fn webcam_basics(ctx: &Ctx, base: &Project, fa: &Frame, t_in: f64) {
            if !has_webcam(ctx, base) {
                println!("[webcam margin/border] skipped: project has no webcam");
                return;
            }
            println!("[webcam margin / border / shadow]");
            let mk = |margin: f64, border: f64| {
                let mut p = base.clone();
                p.edits.style.webcam.enabled = true;
                p.edits.style.webcam.position = Corner::TopLeft;
                p.edits.style.webcam.shape = WebcamShape::Square;
                p.edits.style.webcam.size = 0.3;
                p.edits.style.webcam.margin = margin;
                p.edits.style.webcam.border_width = border;
                p.edits.style.webcam.border_color = Rgba([255, 0, 0, 255]);
                p.edits.style.webcam.shadow = 0.0;
                p
            };
            let bbox = |name: &str, p: &Project| {
                let o = ctx.export(name, p);
                let f = frame(&o, t_in);
                let (_, bb) = diff(&f, fa);
                (
                    f,
                    bb.unwrap_or_else(|| fail(format!("{name}: bubble not drawn"))),
                )
            };
            let (_, b0) = bbox("wc-margin0", &mk(0.0, 0.0));
            let (_, b1) = bbox("wc-margin10", &mk(0.1, 0.0));
            check(
                b0.0 <= 2 && b0.1 <= 2,
                format!(
                    "margin 0 puts the bubble in the corner: bbox starts at ({}, {})",
                    b0.0, b0.1
                ),
            );
            check(
                (i64::from(b1.0) - 72).abs() < 6 && (i64::from(b1.1) - 72).abs() < 6,
                format!(
                    "margin 0.1 = 72 px of a 720 px high output: bbox starts at ({}, {})",
                    b1.0, b1.1
                ),
            );
            let (fb, bb) = bbox("wc-border", &mk(0.1, 12.0));
            let r = rgb(&fb, f64::from(bb.0) + 3.0, f64::from(bb.1 + bb.3) / 2.0);
            let w_in = rgb(
                &fb,
                f64::from(bb.0) + 12.0 * 720.0 / 1080.0 + 8.0,
                f64::from(bb.1 + bb.3) / 2.0,
            );
            check(
                r[0] > 180 && r[1] < 90 && r[2] < 90,
                format!("12 px (at 1080p) red border at the bubble edge: {r:?}"),
            );
            check(
                !(w_in[0] > 180 && w_in[1] < 90),
                format!("camera picture starts inside the border: {w_in:?}"),
            );
        }

        fn annotations(ctx: &Ctx, base: &Project, a: &Path, fa: &Frame, t_in: f64, d: f64) {
            println!("[annotations] every kind, pixel checked");
            let l = lay(ctx, base);
            let at = |nx: f64, ny: f64| {
                (
                    l.content.x + nx * l.content.w,
                    l.content.y + ny * l.content.h,
                )
            };
            let mk = |kind: AnnotationKind, f: &dyn Fn(&mut Annotation)| {
                let mut p = base.clone();
                let mut an = Annotation {
                    id: "a".into(),
                    kind,
                    start_ms: (d * 0.2) as u64,
                    end_ms: (d * 0.6) as u64,
                    x: 0.3,
                    y: 0.3,
                    w: 0.4,
                    h: 0.3,
                    x2: 0.8,
                    y2: 0.5,
                    color: Rgba([255, 0, 0, 255]),
                    stroke_width: 12.0,
                    fade_ms: 0,
                    ..Annotation::default()
                };
                f(&mut an);
                p.edits.annotations = vec![an];
                p
            };
            let red = |c: [i32; 3]| c[0] > 200 && c[1] < 70 && c[2] < 70;
            let (cx, cy) = at(0.5, 0.45);
            let same = |f: &Frame, x: f64, y: f64| {
                (0..3).all(|i| (rgb(f, x, y)[i] - rgb(fa, x, y)[i]).abs() <= 24)
            };

            let o = ctx.export("ann-rect", &mk(AnnotationKind::Rect, &|a| a.fill = true));
            let f = frame(&o, t_in);
            check(
                red(rgb(&f, cx, cy)) && same(&f, at(0.1, 0.1).0, at(0.1, 0.1).1),
                "rect (filled): red inside, untouched outside".into(),
            );
            let o = ctx.export("ann-rect-outline", &mk(AnnotationKind::Rect, &|_| {}));
            let f = frame(&o, t_in);
            let (ex, ey) = at(0.3, 0.45);
            check(
                red(rgb(&f, ex, ey)) && same(&f, cx, cy),
                "rect (outline): red edge, original picture inside".into(),
            );
            // Outside the time range nothing is drawn.
            let t_late = d * 0.9;
            check(
                diff(&frame(&o, t_late), &frame(a, t_late)).0 == 0,
                "annotation is gone after endMs".into(),
            );
            let o = ctx.export(
                "ann-ellipse",
                &mk(AnnotationKind::Ellipse, &|a| a.fill = true),
            );
            let f = frame(&o, t_in);
            let (kx, ky) = at(0.31, 0.31);
            check(
                red(rgb(&f, cx, cy)) && same(&f, kx, ky),
                "ellipse (filled): centre red, bbox corner untouched".into(),
            );
            let o = ctx.export(
                "ann-highlight",
                &mk(AnnotationKind::Highlight, &|a| {
                    a.color = Rgba([255, 255, 0, 255])
                }),
            );
            let f = frame(&o, t_in);
            let (n, _) = diff(&f, fa);
            check(
                n > 5000 && rgb(&f, cx, cy)[0] >= rgb(fa, cx, cy)[0] - 2,
                format!("highlight tints the region: {n} px changed"),
            );
            let o = ctx.export(
                "ann-arrow",
                &mk(AnnotationKind::Arrow, &|a| {
                    a.x = 0.2;
                    a.y = 0.5;
                    a.x2 = 0.8;
                    a.y2 = 0.5;
                    a.stroke_width = 14.0;
                }),
            );
            let f = frame(&o, t_in);
            let (sx, sy) = at(0.45, 0.5);
            let (hx, hy) = at(0.78, 0.5);
            check(
                red(rgb(&f, sx, sy)) && red(rgb(&f, hx, hy)),
                "arrow: red shaft and head".into(),
            );
            let o = ctx.export(
                "ann-spotlight",
                &mk(AnnotationKind::Spotlight, &|a| a.dim = 0.8),
            );
            let f = frame(&o, t_in);
            let (ox, oy) = at(0.1, 0.1);
            let lum = |c: [i32; 3]| c[0] + c[1] + c[2];
            check(
                same(&f, cx, cy) && lum(rgb(&f, ox, oy)) * 3 < lum(rgb(fa, ox, oy)) * 2 + 30,
                format!(
                    "spotlight: inside untouched, outside darkened ({} -> {})",
                    lum(rgb(fa, ox, oy)),
                    lum(rgb(&f, ox, oy))
                ),
            );
            // Spotlight never darkens the padding / background (screen content only).
            let (bx, by) = (4.0, 4.0);
            check(
                same(&f, bx, by),
                "spotlight leaves the background outside the screen content alone".into(),
            );
            // Blur / pixelate: horizontal detail drops inside the rect.
            let detail = |f: &Frame| {
                let (x0, y0) = at(0.32, 0.32);
                let (x1, y1) = at(0.68, 0.58);
                let (mut s, mut n) = (0i64, 0i64);
                for y in (y0 as u32..y1 as u32).step_by(3) {
                    for x in x0 as u32..x1 as u32 - 1 {
                        s += (i64::from(px(f, x, y)[0]) - i64::from(px(f, x + 1, y)[0])).abs();
                        n += 1;
                    }
                }
                s as f64 / n.max(1) as f64
            };
            let base_detail = detail(fa);
            for (kind, name) in [
                (AnnotationKind::Blur, "blur"),
                (AnnotationKind::Pixelate, "pixelate"),
            ] {
                let o = ctx.export(&format!("ann-{name}"), &mk(kind, &|a| a.strength = 1.0));
                let f = frame(&o, t_in);
                let (n, _) = diff(&f, fa);
                if base_detail > 1.5 {
                    check(
                        detail(&f) < base_detail * 0.7 && n > 1000,
                        format!(
                            "{name}: detail inside the rect {:.2} -> {:.2}, {n} px changed",
                            base_detail,
                            detail(&f)
                        ),
                    );
                } else {
                    println!(
                        "  note {name}: recording is flat here (detail {base_detail:.2}); changed px {n}"
                    );
                    check(true, format!("{name} exported"));
                }
            }
            let o = ctx.export(
                "ann-text",
                &mk(AnnotationKind::Text, &|a| {
                    a.text = "Frostcap annotation".into();
                    a.font_size = 60.0;
                    a.color = Rgba([255, 255, 255, 255]);
                }),
            );
            let f = frame(&o, t_in);
            let (n, bb) = diff(&f, fa);
            let (x0, y0) = at(0.3, 0.3);
            let (x1, y1) = at(0.7, 0.6);
            let (bx0, by0, bx1, by1) =
                bb.unwrap_or_else(|| fail("text annotation not drawn".into()));
            check(
                n > 300
                    && f64::from(bx0) > x0 - 30.0
                    && f64::from(bx1) < x1 + 30.0
                    && f64::from(by0) > y0 - 40.0
                    && f64::from(by1) < y1 + 40.0,
                format!(
                    "text: {n} px inside its box, bbox ({bx0},{by0})-({bx1},{by1}) vs box ({x0:.0},{y0:.0})-({x1:.0},{y1:.0})"
                ),
            );
            // Fade in: halfway through a 400 ms fade the fill is half way to red.
            let o = ctx.export(
                "ann-fade",
                &mk(AnnotationKind::Rect, &|a| {
                    a.fill = true;
                    a.fade_ms = 400;
                }),
            );
            let t_fade = d * 0.2 + 200.0;
            let c = rgb(&frame(&o, t_fade), cx, cy);
            let c0 = rgb(fa, cx, cy);
            check(
                c[0] > c0[0] + 30 && c[0] < 235,
                format!(
                    "fade: red channel {} -> {} half way (control {})",
                    c0[0], c[0], c0[0]
                ),
            );
            // Zoom: annotations ride the zoom (the rect grows with the picture).
            let mut z = mk(AnnotationKind::Rect, &|a| a.fill = true);
            z.edits.zoom_segments = vec![frostcap_project::ZoomSegment {
                start_ms: 0,
                end_ms: d as u64,
                center: frostcap_project::NormPoint { x: 0.5, y: 0.45 },
                scale: 2.0,
                easing: frostcap_project::Easing::Linear,
                transition_ms: 0,
                origin: frostcap_project::SegmentOrigin::Manual,
            }];
            let o = ctx.export("ann-zoom", &z);
            let f = frame(&o, t_in);
            let (n, bb) = diff(
                &f,
                &frame(
                    &ctx.export("ann-zoom-ref", &{
                        let mut q = z.clone();
                        q.edits.annotations.clear();
                        q
                    }),
                    t_in,
                ),
            );
            let (zx0, _, zx1, _) = bb.unwrap();
            let w_unzoomed = l.content.w * 0.4;
            check(
                f64::from(zx1 - zx0) > w_unzoomed * 1.6,
                format!(
                    "annotation scales with the zoom: width {} px vs {:.0} px unzoomed ({n} px)",
                    zx1 - zx0,
                    w_unzoomed
                ),
            );
        }

        fn green_screen(ctx: &Ctx, base: &Project, fa: &Frame, t_in: f64) {
            println!(
                "[webcam effects] synthetic green-screen camera (generated with the MF writer)"
            );
            let cdir = ctx.out.join("cam-proj");
            let _ = std::fs::remove_dir_all(&cdir);
            std::fs::create_dir_all(&cdir).unwrap();
            std::fs::copy(ctx.dir.join("screen.mp4"), cdir.join("screen.mp4")).unwrap();
            let (w, h) = (640u32, 360u32);
            {
                let mut wr = mf::Mp4Writer::create(
                    &cdir.join("webcam.mp4"),
                    w,
                    h,
                    30,
                    6_000_000,
                    false,
                    false,
                )
                .unwrap_or_else(|e| fail(format!("webcam writer: {e}")));
                for _ in 0..120 {
                    // Green screen with a red "subject" left of centre (in the square crop).
                    let f = nv12(w, h, &|x, y| {
                        if (200..320).contains(&x) && (100..260).contains(&y) {
                            [220, 40, 40]
                        } else {
                            [0, 255, 0]
                        }
                    });
                    wr.write_video(&f).unwrap_or_else(|e| fail(e.to_string()));
                }
                wr.finish().unwrap_or_else(|e| fail(e.to_string()));
            }
            let logo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/brand/logo.png");
            let png = frostcap_project::import_background(&cdir, &logo).unwrap();
            let cctx = Ctx {
                dir: cdir,
                out: ctx.out.clone(),
            };
            let mut p = base.clone();
            p.media.webcam = Some("webcam.mp4".into());
            p.media.webcam_video = Some(VideoTrack {
                file: "webcam.mp4".into(),
                name: "synthetic".into(),
                width: w,
                height: h,
                offset_ms: 0,
                duration_ms: 4000,
            });
            p.edits.style.webcam.enabled = true;
            p.edits.style.webcam.position = Corner::TopLeft;
            p.edits.style.webcam.shape = WebcamShape::Square;
            p.edits.style.webcam.size = 0.4;
            p.edits.style.webcam.margin = 0.0;
            p.edits.style.webcam.shadow = 0.0;
            // Bubble = 288 px square at (0,0); crop shows camera x 140..500, subject at crop 60..180.
            let (green_pt, subj_pt, mirror_pt) = ((10.0, 10.0), (96.0, 144.0), (192.0, 144.0));
            let o = cctx.export("gs-plain", &p);
            let f = frame(&o, t_in);
            let g = rgb(&f, green_pt.0, green_pt.1);
            let s = rgb(&f, subj_pt.0, subj_pt.1);
            check(
                g[1] > 200 && g[0] < 90 && s[0] > 170 && s[1] < 100,
                format!("no key: green background {g:?}, subject {s:?}"),
            );
            p.edits.style.webcam.chroma_key.enabled = true;
            p.edits.style.webcam.chroma_key.similarity = 0.4;
            p.edits.style.webcam.chroma_key.smoothness = 0.1;
            let o = cctx.export("gs-key", &p);
            let f = frame(&o, t_in);
            let g = rgb(&f, green_pt.0, green_pt.1);
            let want = rgb(fa, green_pt.0, green_pt.1);
            let s = rgb(&f, subj_pt.0, subj_pt.1);
            check(
                (0..3).all(|i| (g[i] - want[i]).abs() <= 40) && s[0] > 170,
                format!(
                    "key on, background none: screen shows through {g:?} (control {want:?}), subject kept {s:?}"
                ),
            );
            p.edits.style.webcam.background.kind = WebcamBackgroundKind::Color;
            p.edits.style.webcam.background.color = Rgba([0, 0, 255, 255]);
            let o = cctx.export("gs-blue", &p);
            let f = frame(&o, t_in);
            let b = rgb(&f, green_pt.0, green_pt.1);
            check(
                b[2] > 200 && b[0] < 60 && b[1] < 60 && rgb(&f, subj_pt.0, subj_pt.1)[0] > 170,
                format!("background colour blue replaces the green screen: {b:?}"),
            );
            p.edits.style.webcam.background.kind = WebcamBackgroundKind::Image;
            p.edits.style.webcam.background.image = Some(png);
            let o = cctx.export("gs-image", &p);
            let f = frame(&o, t_in);
            let (n, bb) = diff(&f, &frame(&cctx.out.join("gs-blue.mp4"), t_in));
            check(
                n > 1000 && bb.is_some() && rgb(&f, subj_pt.0, subj_pt.1)[0] > 170,
                format!("background image differs from the plain colour: {n} px, subject kept"),
            );
            p.edits.style.webcam.background.kind = WebcamBackgroundKind::Blur;
            p.edits.style.webcam.background.image = None;
            let o = cctx.export("gs-blur", &p);
            let f = frame(&o, t_in);
            let g = rgb(&f, green_pt.0, green_pt.1);
            check(
                (g[0] - g[1]).abs() < 35 && (g[1] - g[2]).abs() < 35 && g[1] > 80,
                format!("background blur: desaturated blurred camera {g:?}"),
            );
            p.edits.style.webcam.background.kind = WebcamBackgroundKind::None;
            p.edits.style.webcam.chroma_key.enabled = false;
            p.edits.style.webcam.mirror = true;
            let o = cctx.export("gs-mirror", &p);
            let f = frame(&o, t_in);
            let (l, r) = (
                rgb(&f, subj_pt.0, subj_pt.1),
                rgb(&f, mirror_pt.0, mirror_pt.1),
            );
            check(
                l[0] < 120 && r[0] > 170 && r[1] < 100,
                format!("mirror: subject moved from {subj_pt:?} {l:?} to {mirror_pt:?} {r:?}"),
            );
            p.edits.style.webcam.mirror = false;
            p.edits.style.webcam.brightness = 0.0;
            p.edits.style.webcam.saturation = 0.0;
            let o = cctx.export("gs-grey", &p);
            let f = frame(&o, t_in);
            let c = rgb(&f, subj_pt.0, subj_pt.1);
            check(
                (c[0] - c[1]).abs() < 18 && (c[1] - c[2]).abs() < 18,
                format!("saturation 0 -> grey subject {c:?}"),
            );
            p.edits.style.webcam.saturation = 1.0;
            p.edits.style.webcam.brightness = 0.3;
            let o = cctx.export("gs-bright", &p);
            let c2 = rgb(&frame(&o, t_in), subj_pt.0, subj_pt.1);
            let c1 = rgb(
                &frame(&cctx.out.join("gs-plain.mp4"), t_in),
                subj_pt.0,
                subj_pt.1,
            );
            check(
                c2[0] > c1[0] + 20 || c2[1] > c1[1] + 40,
                format!("brightness +0.3: {c1:?} -> {c2:?}"),
            );
        }

        fn batch_and_gif(ctx: &Ctx, base: &Project, a: &Path, t_in: f64) {
            println!("[batch export + GIF]");
            let rp = ctx.model(base);
            let bdir = ctx.out.join("batch");
            let _ = std::fs::remove_dir_all(&bdir);
            let mk = |w, h, fps, format| ExportPreset {
                aspect: Aspect::Landscape,
                width: w,
                height: h,
                fps,
                format,
            };
            let jobs = vec![
                BatchJob {
                    preset: mk(1280, 720, 30, OutputFormat::Mp4),
                    out_path: bdir.join("720.mp4"),
                },
                BatchJob {
                    preset: mk(854, 480, 30, OutputFormat::Mp4),
                    out_path: bdir.join("480.mp4"),
                },
                BatchJob {
                    preset: mk(640, 360, 15, OutputFormat::Gif),
                    out_path: bdir.join("small.gif"),
                },
            ];
            let mut last = vec![0.0f64; jobs.len()];
            let mut stages = std::collections::BTreeSet::new();
            let res = export_batch(
                &rp,
                &jobs,
                &ExportOptions::default(),
                &AtomicBool::new(false),
                &mut |i, f, s| {
                    last[i] = last[i].max(f);
                    stages.insert(s.as_str());
                },
            );
            for (j, r) in jobs.iter().zip(&res) {
                let s = r
                    .as_ref()
                    .unwrap_or_else(|e| fail(format!("batch {}: {e}", j.out_path.display())));
                println!(
                    "  batch {}: {}x{} {} KB, setup {:.2}s, total {:.2}s",
                    j.out_path.file_name().unwrap().to_string_lossy(),
                    s.width,
                    s.height,
                    s.bytes / 1024,
                    s.setup_s,
                    s.elapsed_s
                );
            }
            check(
                last.iter().all(|f| (*f - 1.0).abs() < 1e-9),
                format!("per-output progress reached 1.0 for every job: {last:?}"),
            );
            let leftovers: Vec<_> = std::fs::read_dir(&bdir)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains("partial"))
                .collect();
            check(
                leftovers.is_empty(),
                format!("atomic outputs: no .partial files left ({leftovers:?})"),
            );
            let single = ctx.export("batch-ref-720", base);
            let (n, _) = diff(&frame(&bdir.join("720.mp4"), t_in), &frame(&single, t_in));
            check(
                n == 0,
                format!("batch 720p output is pixel-identical to a single export: {n} px differ"),
            );
            let (v, v2) = (
                video_s(&bdir.join("720.mp4")),
                video_s(&bdir.join("480.mp4")),
            );
            check(
                (v - v2).abs() < 0.1,
                format!("outputs share the timeline: {v:.3}s / {v2:.3}s"),
            );
            // GIF: valid, right size, frame count, duration.
            let mut opts = gif::DecodeOptions::new();
            opts.set_color_output(gif::ColorOutput::Indexed);
            let mut dec = opts
                .read_info(std::fs::File::open(bdir.join("small.gif")).unwrap())
                .unwrap();
            let (gw, gh) = (dec.width(), dec.height());
            let (mut frames, mut cs) = (0, 0u32);
            while let Some(fr) = dec.read_next_frame().unwrap() {
                frames += 1;
                cs += u32::from(fr.delay);
            }
            let d = base.duration_ms as f64;
            check(
                (gw, gh) == (640, 360)
                    && frames >= 2
                    && (f64::from(cs) / 100.0 - d / 1000.0).abs() < 0.3,
                format!(
                    "GIF {gw}x{gh}, {frames} frames, {:.2}s (project {:.2}s)",
                    f64::from(cs) / 100.0,
                    d / 1000.0
                ),
            );
            let _ = a;
            // AV1 (MP4 container) only where Windows has an encoder.
            let caps = mf::encoder_caps();
            println!(
                "  encoders: h264={} vp9={} av1={}",
                caps.h264, caps.vp9, caps.av1
            );
            if caps.av1 {
                let o = bdir.join("av1.mp4");
                let r = export_project(
                    &rp,
                    &mk(1280, 720, 30, OutputFormat::Av1),
                    &o,
                    &ExportOptions::default(),
                    &AtomicBool::new(false),
                    &mut |_, _| {},
                );
                match r {
                    Ok(s) => {
                        let info = mp4info::parse_file(&o).unwrap_or_else(|e| fail(e.to_string()));
                        let dur = info
                            .tracks
                            .iter()
                            .find(|t| t.handler == "vide")
                            .unwrap()
                            .duration_s;
                        check(
                            (dur - d / 1000.0).abs() < 0.15,
                            format!(
                                "AV1/MP4 export: {} KB, {dur:.2}s, hardware encode={}",
                                s.bytes / 1024,
                                s.hardware_encode
                            ),
                        );
                    }
                    Err(e) => fail(format!("AV1 export failed although an encoder exists: {e}")),
                }
            }
        }
    }
}

#[cfg(windows)]
fn write_tone(path: &std::path::Path, hz: f64, amp: f64, ms: f64) {
    let n = (ms / 1000.0 * 48_000.0) as usize;
    let mut pcm = Vec::with_capacity(n * 4);
    for i in 0..n {
        let v =
            (amp * (2.0 * std::f64::consts::PI * hz * i as f64 / 48_000.0).sin() * 32767.0) as i16;
        pcm.extend_from_slice(&v.to_le_bytes());
        pcm.extend_from_slice(&v.to_le_bytes());
    }
    let mut w = b"RIFF".to_vec();
    w.extend(((36 + pcm.len()) as u32).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend(1u16.to_le_bytes());
    w.extend(2u16.to_le_bytes());
    w.extend(48_000u32.to_le_bytes());
    w.extend((48_000u32 * 4).to_le_bytes());
    w.extend(4u16.to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(b"data");
    w.extend((pcm.len() as u32).to_le_bytes());
    w.extend(pcm);
    std::fs::write(path, w).unwrap();
}
