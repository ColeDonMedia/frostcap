//! Measures export setup time and GIF speed, and runs a multi-preset batch.
//!
//! `cargo run --release -p frostcap-render --example batch -- <project-dir> [out-dir]`
//!
//! Set `FROSTCAP_NO_GPU_CACHE=1` for the "before" numbers (a new wgpu device and
//! fresh pipelines for every export, like the old code).

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    use std::sync::atomic::AtomicBool;

    use frostcap_render::{
        Aspect, BatchJob, ExportOptions, ExportPreset, OutputFormat, RenderProject, export_batch,
        export_project,
    };

    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(dir) = args.first() else {
        eprintln!("usage: batch <project-dir> [out-dir]");
        std::process::exit(2);
    };
    let out = std::path::PathBuf::from(args.get(1).map_or("target/smoke/batch", String::as_str));
    std::fs::create_dir_all(&out).unwrap();
    let p = RenderProject::load(std::path::Path::new(dir)).expect("load project");
    let cancel = AtomicBool::new(false);
    let opts = ExportOptions::default();
    let preset = |w, h, fps, format| ExportPreset {
        aspect: Aspect::Landscape,
        width: w,
        height: h,
        fps,
        format,
    };
    println!(
        "gpu cache: {}",
        if std::env::var_os("FROSTCAP_NO_GPU_CACHE").is_some() {
            "OFF (before)"
        } else {
            "ON"
        }
    );
    // Sequential single exports in one process: setup time of #1 (cold) vs #2/#3.
    for n in 1..=3 {
        let f = out.join(format!("single-{n}.mp4"));
        let s = export_project(
            &p,
            &preset(1280, 720, 30, OutputFormat::Mp4),
            &f,
            &opts,
            &cancel,
            &mut |_, _| {},
        )
        .expect("export");
        println!(
            "mp4 720p30 export #{n}: setup {:.2}s, total {:.2}s, render {:.2}x realtime",
            s.setup_s, s.elapsed_s, s.realtime_factor
        );
    }
    // GIF speed.
    for n in 1..=2 {
        let f = out.join(format!("clip-{n}.gif"));
        let s = export_project(
            &p,
            &preset(960, 540, 15, OutputFormat::Gif),
            &f,
            &opts,
            &cancel,
            &mut |_, _| {},
        )
        .expect("gif");
        println!(
            "gif 960x540@15 #{n}: setup {:.2}s, total {:.2}s = {:.2}x realtime, {} KB",
            s.setup_s,
            s.elapsed_s,
            s.realtime_factor,
            s.bytes / 1024
        );
    }
    // Batch: three outputs at 30 fps share one decoder pass; the GIF (15 fps) has its own.
    let jobs = [
        BatchJob {
            preset: preset(1280, 720, 30, OutputFormat::Mp4),
            out_path: out.join("b-720.mp4"),
        },
        BatchJob {
            preset: preset(854, 480, 30, OutputFormat::Mp4),
            out_path: out.join("b-480.mp4"),
        },
        BatchJob {
            preset: preset(1080, 1080, 30, OutputFormat::Mp4),
            out_path: out.join("b-square.mp4"),
        },
        BatchJob {
            preset: preset(640, 360, 15, OutputFormat::Gif),
            out_path: out.join("b-small.gif"),
        },
    ];
    let started = std::time::Instant::now();
    let mut last = vec![-1.0f64; jobs.len()];
    let results = export_batch(&p, &jobs, &opts, &cancel, &mut |i, f, stage| {
        if f - last[i] >= 0.25 || f >= 1.0 {
            last[i] = f;
            println!("  progress job {i}: {:>3.0}% {}", f * 100.0, stage.as_str());
        }
    });
    let wall = started.elapsed().as_secs_f64();
    for (j, r) in jobs.iter().zip(&results) {
        match r {
            Ok(s) => println!(
                "batch {}: {}x{} {}fps {} KB, setup {:.2}s",
                j.out_path.file_name().unwrap().to_string_lossy(),
                s.width,
                s.height,
                s.fps,
                s.bytes / 1024,
                s.setup_s
            ),
            Err(e) => println!("batch {}: FAILED {e}", j.out_path.display()),
        }
    }
    println!("batch wall time for {} outputs: {wall:.2}s", jobs.len());
    // Sequential baseline for the same four outputs.
    let started = std::time::Instant::now();
    for (i, j) in jobs.iter().enumerate() {
        let f = out.join(format!(
            "seq-{i}.{}",
            if j.preset.format == OutputFormat::Gif {
                "gif"
            } else {
                "mp4"
            }
        ));
        export_project(&p, &j.preset, &f, &opts, &cancel, &mut |_, _| {}).expect("seq");
    }
    println!(
        "same outputs one by one: {:.2}s",
        started.elapsed().as_secs_f64()
    );
    if results.iter().any(Result::is_err) {
        std::process::exit(1);
    }
}
