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
        println!("verify_edits: ALL OK");
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
