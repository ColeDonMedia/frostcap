//! Real export of a synthetic 2-hour project to prove the streaming audio path:
//! a 2 h `screen.mp4` (1 fps, tiny frames), a 2 h sparse microphone WAV (silence
//! with a few tone bursts) and a 2 h system WAV, exported with a cut and a speed
//! segment. Reports peak memory and checks the audio/video durations match.
//!
//! `cargo run --release -p frostcap-render --example long_audio -- [out-dir] [hours]`
//!
//! Everything is written under the given folder (default `target/smoke/long-audio`)
//! and deleted afterwards unless `--keep` is passed.

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    use std::io::{Seek, SeekFrom, Write};
    use std::sync::atomic::AtomicBool;

    use frostcap_project::{AudioTrack, Project, SpeedSegment, TrimRange};
    use frostcap_render::mf::{MfGuard, Mp4Writer};
    use frostcap_render::{
        Aspect, ExportOptions, ExportPreset, OutputFormat, RenderProject, export_project, mp4info,
    };

    let args: Vec<String> = std::env::args().skip(1).collect();
    let keep = args.iter().any(|a| a == "--keep");
    let pos: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let root = std::path::PathBuf::from(
        pos.first()
            .map_or("target/smoke/long-audio", |s| s.as_str()),
    );
    let hours: f64 = pos.get(1).and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let dur_s = (hours * 3600.0) as u64;
    let dir = root.join("long.frostcap");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (320u32, 180u32);

    println!("writing a {hours} h screen.mp4 ({dur_s} frames at 1 fps)...");
    {
        let _g = MfGuard::new().unwrap();
        let mut wr =
            Mp4Writer::create(&dir.join("screen.mp4"), w, h, 1, 200_000, false, false).unwrap();
        for i in 0..dur_s {
            let mut f = vec![(60 + (i % 100)) as u8; (w * h) as usize];
            f.extend(vec![128u8; (w * h / 2) as usize]);
            wr.write_video(&f).unwrap();
        }
        wr.finish().unwrap();
    }
    // Sparse WAVs: header + set_len; tone bursts written at a few places.
    let rate = 48_000u32;
    let data_bytes = dur_s * u64::from(rate) * 4;
    let write_wav = |name: &str, hz: f64| {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        let mut hdr = b"RIFF".to_vec();
        hdr.extend((36 + data_bytes as u32).to_le_bytes());
        hdr.extend(b"WAVEfmt ");
        hdr.extend(16u32.to_le_bytes());
        hdr.extend([1, 0, 2, 0]);
        hdr.extend(rate.to_le_bytes());
        hdr.extend((rate * 4).to_le_bytes());
        hdr.extend([4, 0, 16, 0]);
        hdr.extend(b"data");
        hdr.extend((data_bytes as u32).to_le_bytes());
        f.write_all(&hdr).unwrap();
        f.set_len(hdr.len() as u64 + data_bytes).unwrap();
        for burst in 0..8u64 {
            let at_s = dur_s * (burst + 1) / 9;
            let pcm: Vec<u8> = (0..rate as usize)
                .flat_map(|i| {
                    let v = (0.4
                        * (2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(rate)).sin()
                        * 32767.0) as i16;
                    let b = v.to_le_bytes();
                    [b[0], b[1], b[0], b[1]]
                })
                .collect();
            f.seek(SeekFrom::Start(
                hdr.len() as u64 + at_s * u64::from(rate) * 4,
            ))
            .unwrap();
            f.write_all(&pcm).unwrap();
        }
    };
    write_wav("audio-mic.wav", 440.0);
    write_wav("audio-system.wav", 1000.0);

    let mut p = frostcap_project::samples::sample();
    p.canvas = frostcap_project::Size {
        width: w,
        height: h,
    };
    p.fps = 1;
    p.duration_ms = dur_s * 1000;
    p.tracks = Default::default();
    let track = |f: &str| AudioTrack {
        file: f.into(),
        offset_ms: 0,
        sample_rate: rate,
        channels: 2,
        duration_ms: dur_s * 1000,
    };
    p.media.mic_audio = Some(track("audio-mic.wav"));
    p.media.system_audio = Some(track("audio-system.wav"));
    // A cut and a 2x segment (the output is shorter than the source).
    p.edits.trims = vec![TrimRange {
        start_ms: 600_000,
        end_ms: 900_000,
    }];
    p.edits.speed = vec![SpeedSegment {
        start_ms: 1_800_000,
        end_ms: 2_400_000,
        speed: 2.0,
    }];
    Project::save(&p, &dir).unwrap();
    let rp = RenderProject::load(&dir).unwrap();
    let want_s = rp.output_ms() / 1000.0;
    println!(
        "project {:.0} s, expected output {want_s:.1} s; exporting...",
        dur_s as f64
    );
    let out = root.join("out.mp4");
    let t = std::time::Instant::now();
    let preset = ExportPreset {
        aspect: Aspect::Landscape,
        width: w,
        height: h,
        fps: 1,
        format: OutputFormat::Mp4,
    };
    let stats = export_project(
        &rp,
        &preset,
        &out,
        &ExportOptions::default(),
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap_or_else(|e| {
        eprintln!("FAIL: {e}");
        std::process::exit(1)
    });
    println!(
        "export took {:.1} s, audio={}, warnings={:?}",
        t.elapsed().as_secs_f64(),
        stats.audio,
        stats.warnings
    );
    let info = mp4info::parse_file(&out).unwrap();
    let dur = |h: &str| {
        info.tracks
            .iter()
            .find(|t| t.handler == h)
            .map(|t| t.duration_s)
    };
    let (v, a) = (dur("vide").unwrap_or(0.0), dur("soun").unwrap_or(0.0));
    println!("video {v:.2} s, audio {a:.2} s (expected {want_s:.2} s)");
    let pid = std::process::id();
    let peak = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!("(Get-Process -Id {pid}).PeakWorkingSet64 / 1MB"),
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    println!("peak working set: {peak} MB");
    let ok =
        stats.audio && (v - want_s).abs() < 1.5 && (a - want_s).abs() < 1.5 && (a - v).abs() < 0.5;
    if !keep {
        let _ = std::fs::remove_dir_all(&root);
        println!("deleted {}", root.display());
    }
    if !ok {
        eprintln!("FAIL: durations do not match");
        std::process::exit(1);
    }
    println!("long_audio: OK");
}
