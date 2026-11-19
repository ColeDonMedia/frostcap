//! Lists the video encoder MFTs and tries to write 30 synthetic frames as
//! AV1 / VP9 into an MP4 container through the Media Foundation sink writer.
//!
//! `cargo run --release -p frostcap-render --example codec_probe -- <out-dir>`

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    use frostcap_render::mf::{self, MfGuard, Mp4Writer, VideoCodec};
    for (name, names) in mf::encoder_report() {
        println!("{name}: {names:?}");
    }
    let dir = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "target/smoke/codec".into()),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let _g = MfGuard::new().unwrap();
    let (w, h) = (640u32, 360u32);
    for (codec, name) in [
        (VideoCodec::Av1, "av1"),
        (VideoCodec::Vp9, "vp9"),
        (VideoCodec::H264, "h264"),
    ] {
        let path = dir.join(format!("probe-{name}.mp4"));
        let _ = std::fs::remove_file(&path);
        match Mp4Writer::create_codec(&path, w, h, 30, 2_000_000, false, true, codec) {
            Ok(mut wr) => {
                let mut ok = true;
                for i in 0..30u32 {
                    let mut frame = vec![(40 + i * 4) as u8; (w * h) as usize];
                    frame.extend(vec![128u8; (w * h / 2) as usize]);
                    if let Err(e) = wr.write_video(&frame) {
                        println!("{name}: write_video failed at frame {i}: {e}");
                        ok = false;
                        break;
                    }
                }
                if ok {
                    match wr.finish() {
                        Ok(()) => println!(
                            "{name}: OK, {} bytes",
                            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                        ),
                        Err(e) => println!("{name}: finalize failed: {e}"),
                    }
                }
            }
            Err(e) => println!("{name}: sink writer refused: {e}"),
        }
    }
}
