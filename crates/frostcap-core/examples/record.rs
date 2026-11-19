//! Smoke test for the recording pipeline. Records through the core API, then
//! validates screen.mp4 by walking its boxes and checks durations line up.
//!
//! ```text
//! cargo run -p frostcap-core --example record -- [seconds] [flags]
//!   --notepad    launch Notepad and record its window (never injects input;
//!                type into it yourself to check key capture)
//!   --region     record the centre quarter of the primary monitor
//!   --pause      pause for 1 s in the middle (gap must be removed)
//!   --webcam     also record the default camera (warns if none)
//!   --mic --system   record audio
//!   --crash-after-ms N   abort the process mid-recording (crash simulation)
//!   --recover-latest     recover the newest smoke recording and inspect its media
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use frostcap_core::autozoom::{self, ZoomPreset};
use frostcap_core::input::KeyCapture;
use frostcap_core::recorder::{self, RecordOptions, RecordingSession};
use frostcap_core::sources::{self, CaptureSource, NormRect};
use frostcap_core::webcam;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let secs: f64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(3.0);
    let flag = |f: &str| args.iter().any(|a| a == f);
    let root = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| PathBuf::from("target"), PathBuf::from)
        .join("smoke");

    if flag("--recover-latest") {
        // Recover the newest smoke folder left behind by a `--crash-after-ms` run.
        let dir = fs::read_dir(&root)
            .expect("smoke dir")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "frostcap"))
            .max_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok())
            .expect("no smoke recordings");
        println!("recovering {}", dir.display());
        let p = frostcap_core::recovery::recover_project(&dir).expect("recover");
        println!(
            "recovered: status={:?} duration_ms={} cursor={} warnings={:?}",
            p.status,
            p.duration_ms,
            p.tracks.cursor.len(),
            p.warnings
        );
        for f in ["screen.mp4", "webcam.mp4"] {
            if dir.join(f).exists() {
                match inspect_mp4(&dir.join(f)) {
                    Ok(i) => println!("{f} OK after crash: {i}"),
                    Err(e) => println!("{f} INVALID after crash: {e}"),
                }
            }
        }
        if let Some(w) = &p.media.webcam_video {
            println!("webcam attached: {} ms", w.duration_ms);
        }
        return;
    }
    let crash_after: Option<u64> = args
        .iter()
        .position(|a| a == "--crash-after-ms")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok());

    recorder::ensure_dpi_awareness();
    let t = Instant::now();
    let list = sources::list_sources(true).expect("list sources");
    let with_thumbs = list
        .sources
        .iter()
        .filter(|s| s.thumbnail.is_some())
        .count();
    println!(
        "sources: {} monitors, {} windows, {} thumbnails in {} ms",
        list.monitors.len(),
        list.windows.len(),
        with_thumbs,
        t.elapsed().as_millis()
    );
    println!(
        "cameras: {:?}",
        webcam::list_cameras().map(|c| c.into_iter().map(|c| c.name).collect::<Vec<_>>())
    );

    let mut notepad = None;
    let source = if flag("--notepad") {
        let child = std::process::Command::new("notepad.exe")
            .spawn()
            .expect("launch notepad");
        let pid = child.id();
        notepad = Some(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        let w = loop {
            // Win11 Notepad may be hosted by a different process than the one spawned.
            if let Some(w) = sources::list_windows()
                .unwrap()
                .into_iter()
                .find(|w| w.pid == pid || w.process_name.eq_ignore_ascii_case("notepad.exe"))
            {
                break w;
            }
            assert!(Instant::now() < deadline, "notepad window did not appear");
            std::thread::sleep(Duration::from_millis(200));
        };
        println!(
            "window: {:?} hwnd={} bounds={:?}",
            w.title, w.hwnd, w.bounds
        );
        CaptureSource::Window {
            hwnd: w.hwnd,
            pid: Some(w.pid),
        }
    } else if flag("--region") {
        let m = &list.monitors[0];
        CaptureSource::Region {
            monitor_id: m.id.clone(),
            rect: NormRect {
                x: 0.25,
                y: 0.25,
                width: 0.5,
                height: 0.5,
            },
        }
    } else {
        CaptureSource::PrimaryMonitor
    };

    if flag("--prewarm") {
        println!("prewarm: {} ms", recorder::prewarm(&root).expect("prewarm"));
    }
    let dir = recorder::new_project_dir(&root).expect("project dir");
    let mut opts = RecordOptions::new(source);
    opts.mic = flag("--mic");
    opts.system_audio = flag("--system");
    opts.keys = KeyCapture::All;
    opts.webcam = flag("--webcam").then(|| "default".to_owned());
    let t = Instant::now();
    let mut session = RecordingSession::start(opts, &dir).expect("start recording");
    let timings = session.timings();
    println!(
        "start: {} ms total (capture setup {} ms, encoder setup {} ms), canvas {}x{}",
        t.elapsed().as_millis(),
        timings.capture_setup_ms,
        timings.encoder_setup_ms,
        timings.width,
        timings.height
    );
    let wait = Instant::now();
    while session.frames() == 0 && wait.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
    }
    println!(
        "first frame after start returned: {} ms",
        wait.elapsed().as_millis()
    );

    if let Some(ms) = crash_after {
        std::thread::sleep(Duration::from_millis(ms));
        println!(
            "simulating a crash (process abort) after {ms} ms, frames so far {}",
            session.frames()
        );
        std::process::abort();
    }
    let half = Duration::from_secs_f64(secs / 2.0);
    std::thread::sleep(half);
    if flag("--pause") {
        assert!(session.pause());
        std::thread::sleep(Duration::from_secs(1));
        assert!(session.resume());
    }
    std::thread::sleep(half);
    let h = session.health();
    println!("health: {h:?}");
    let res = session.stop().expect("stop recording");
    assert!(session.stop().is_err(), "second stop must be refused");
    let p = &res.project;
    println!(
        "done: duration_ms={} frames={} cursor={} clicks={} keys={} focus={} pauses={:?} source={:?}",
        res.duration_ms,
        res.frames,
        p.tracks.cursor.len(),
        p.tracks.clicks.len(),
        p.tracks.keys.len(),
        p.tracks.focus.len(),
        p.pauses,
        p.source.kind
    );
    for w in &res.warnings {
        println!("warning: {w}");
    }
    for (name, a) in [
        ("mic", &p.media.mic_audio),
        ("system", &p.media.system_audio),
    ] {
        if let Some(a) = a {
            println!(
                "{name} audio: {} Hz x{} {} ms offset {} ms",
                a.sample_rate, a.channels, a.duration_ms, a.offset_ms
            );
        }
    }
    if let Some(w) = &p.media.webcam_video {
        println!(
            "webcam: {} {}x{} {} ms offset {} ms",
            w.name, w.width, w.height, w.duration_ms, w.offset_ms
        );
        match inspect_mp4(&res.project_dir.join(&w.file)) {
            Ok(info) => println!("WEBCAM MP4 OK: {info}"),
            Err(e) => println!("WEBCAM MP4 INVALID: {e}"),
        }
    }
    if let Some(k) = p.tracks.keys.first() {
        println!("first key: {} at {} ms", k.label, k.t_ms);
    }
    println!(
        "auto-zoom segments (balanced): {}",
        autozoom::generate_for_project(p, ZoomPreset::Balanced).len()
    );

    let mp4 = res.project_dir.join("screen.mp4");
    let code = match inspect_mp4(&mp4) {
        Ok(info) => {
            println!("MP4 OK: {info}");
            0
        }
        Err(e) => {
            eprintln!("MP4 INVALID: {e}");
            2
        }
    };
    if let Some(mut c) = notepad {
        let _ = c.kill();
    }
    std::process::exit(code);
}
/// Duration/fragment info from the core inspector plus the codec fourcc.
fn inspect_mp4(path: &Path) -> Result<String, String> {
    let data = fs::read(path).map_err(|e| e.to_string())?;
    let info = frostcap_core::mp4::inspect_bytes(&data).ok_or("not an MP4")?;
    if !info.playable() {
        return Err(format!("not playable: {info:?}"));
    }
    let codec = ["avc1", "hvc1", "hev1"]
        .into_iter()
        .find(|c| data.windows(4).any(|w| w == c.as_bytes()))
        .unwrap_or("?");
    Ok(format!(
        "size={} bytes duration={:.3}s codec={codec} {}x{} fragmented={} fragments={}",
        data.len(),
        info.duration_ms as f64 / 1000.0,
        info.width,
        info.height,
        info.fragmented,
        info.fragments
    ))
}
