//! # frostcap-render
//!
//! Offline export renderer for Frostcap projects ("baked" export):
//!
//! 1. **Decode** `screen.mp4` (and `webcam.mp4`) with the Media Foundation Source
//!    Reader: DXVA hardware decode through a D3D11 device manager when available,
//!    NV12 output ([`mf`]).
//! 2. **Composite** every output frame with `wgpu` on the DX12 backend only, WGSL
//!    shaders ([`gpu`], `shaders.wgsl`): background gradient, padding, rounded
//!    corners, analytic Gaussian shadow, window frame, eased zoom/pan, re-rendered
//!    cursor with smoothing + ghost motion blur, click ripples, webcam bubble,
//!    captions and key overlay ([`text`], system font via `ab_glyph`).
//!    The layout/easing math ([`math`], [`scene`]) mirrors the editor preview
//!    (`ui/src/lib/render.ts`) and is pure + unit tested.
//! 3. **Encode** H.264 with the Media Foundation Sink Writer (hardware MFTs
//!    enabled) plus AAC audio mixed from the mic/system WAVs ([`audio`]: offsets,
//!    high-pass, noise gate, BS.1770 loudness normalization, limiter), or a GIF
//!    ([`gifenc`]).
//!
//! Cuts and speed changes are applied by remapping output time to source time
//! (`RenderProject::source_time`). Output is deterministic for a given project
//! (all inputs to the GPU are computed in f64 on the CPU; no randomness; the GIF
//! quantizer is deterministic). The H.264 bitstream itself depends on the OS /
//! GPU encoder.
//!
//! **No GPU adapter:** [`gpu::Gpu::new`] falls back to the DX12 WARP software
//! rasterizer (works, much slower; the result reports `software_gpu: true`).
//! `FROSTCAP_FORCE_WARP=1` forces that path for testing.

#![warn(missing_docs)]

pub mod audio;
pub mod background;
pub mod error;
pub mod font;
pub mod gifenc;
pub mod math;
pub mod mixer;
pub mod model;
pub mod mp4info;
pub mod scene;
pub mod suggest;
pub mod text;
pub mod wavstream;

#[cfg(windows)]
mod export;
#[cfg(windows)]
pub mod gpu;
#[cfg(windows)]
pub mod image;
#[cfg(windows)]
pub mod mf;

use serde::{Deserialize, Serialize};

pub use error::{RenderError, Result};
#[cfg(windows)]
pub use export::{BatchJob, BatchProgress, export, export_batch, export_project};
pub use model::RenderProject;

/// Output aspect ratio.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Aspect {
    /// 16:9 landscape.
    #[default]
    #[serde(rename = "16:9")]
    Landscape,
    /// 9:16 portrait.
    #[serde(rename = "9:16")]
    Portrait,
    /// 1:1 square.
    #[serde(rename = "1:1")]
    Square,
    /// Same aspect as the recording.
    #[serde(rename = "source")]
    Source,
}

/// Output container.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum OutputFormat {
    /// H.264 + AAC in MP4.
    #[default]
    Mp4,
    /// Animated GIF (fps and size capped).
    Gif,
    /// AV1 + AAC in MP4. Needs an AV1 encoder MFT (`mf::encoder_caps().av1`);
    /// Media Foundation has no WebM muxer, so this is the container Windows offers.
    Av1,
}

impl OutputFormat {
    /// Written through the Media Foundation MP4 sink (with AAC audio).
    pub fn is_mp4_container(self) -> bool {
        matches!(self, Self::Mp4 | Self::Av1)
    }
}

/// Export request preset, the editor's `{ aspect, width, height, fps, format }`.
/// `width`/`height` 0 means "derive from `aspect` with a 1080 px short edge".
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct ExportPreset {
    /// Aspect ratio.
    pub aspect: Aspect,
    /// Output width (even).
    pub width: u32,
    /// Output height (even).
    pub height: u32,
    /// Output frame rate.
    pub fps: u32,
    /// Container.
    pub format: OutputFormat,
}

impl Default for ExportPreset {
    fn default() -> Self {
        Self {
            aspect: Aspect::Landscape,
            width: 1920,
            height: 1080,
            fps: 60,
            format: OutputFormat::Mp4,
        }
    }
}

/// GIF limits (the editor offers <= 24 fps / <= 1080 short edge).
pub const GIF_MAX_FPS: u32 = 30;
/// Longest GIF edge.
pub const GIF_MAX_EDGE: u32 = 1280;

/// Concrete output parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// Width (even).
    pub width: u32,
    /// Height (even).
    pub height: u32,
    /// Frame rate.
    pub fps: u32,
    /// Container.
    pub format: OutputFormat,
    /// H.264 bitrate (bit/s).
    pub bitrate: u32,
}

fn even(x: f64) -> u32 {
    (((x / 2.0).round() as u32) * 2).clamp(16, 7680)
}

impl ExportPreset {
    /// Resolve sizes for a source of `src` pixels.
    pub fn resolve(&self, src: (u32, u32)) -> Resolved {
        let (sw, sh) = (src.0.max(2) as f64, src.1.max(2) as f64);
        let (mut w, mut h) = if self.width >= 16 && self.height >= 16 {
            (self.width as f64, self.height as f64)
        } else {
            let short = 1080.0;
            match self.aspect {
                Aspect::Landscape => (short * 16.0 / 9.0, short),
                Aspect::Portrait => (short, short * 16.0 / 9.0),
                Aspect::Square => (short, short),
                Aspect::Source => (sw, sh),
            }
        };
        let mut fps = self.fps.clamp(1, 120);
        if self.format == OutputFormat::Gif {
            fps = fps.min(GIF_MAX_FPS);
            let k = (GIF_MAX_EDGE as f64 / w.max(h)).min(1.0);
            w *= k;
            h *= k;
        }
        let (width, height) = (even(w), even(h));
        let px = (width as f64 * height as f64) / (1920.0 * 1080.0);
        let mut bitrate = (16_000_000.0 * px * (fps as f64 / 60.0).max(0.5))
            .clamp(2_000_000.0, 80_000_000.0) as u32;
        if self.format == OutputFormat::Av1 {
            bitrate = (bitrate as f64 * 0.65) as u32; // AV1 needs fewer bits for the same quality
        }
        Resolved {
            width,
            height,
            fps,
            format: self.format,
            bitrate,
        }
    }
}

/// Export progress stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Opening inputs / GPU.
    Preparing,
    /// Mixing audio.
    Audio,
    /// Rendering + encoding frames.
    Rendering,
    /// Finalizing the container.
    Finalizing,
    /// Finished.
    Done,
}

impl Stage {
    /// Stable label for UI events.
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Preparing => "preparing",
            Stage::Audio => "audio",
            Stage::Rendering => "rendering",
            Stage::Finalizing => "finalizing",
            Stage::Done => "done",
        }
    }
}

/// Knobs that are not part of the user-facing preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportOptions {
    /// Allow the WARP software adapter when no GPU exists.
    pub allow_software_gpu: bool,
    /// Use hardware decoder/encoder MFTs when available.
    pub hardware_codecs: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            allow_software_gpu: true,
            hardware_codecs: true,
        }
    }
}

/// What an export did.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportStats {
    /// Final file.
    pub out_path: std::path::PathBuf,
    /// File size.
    pub bytes: u64,
    /// Frames rendered.
    pub frames: u64,
    /// Output duration (ms).
    pub duration_ms: f64,
    /// Wall time (s).
    pub elapsed_s: f64,
    /// Time from the start of the export to the first rendered frame (s):
    /// decoder/encoder/GPU setup.
    pub setup_s: f64,
    /// Non-fatal problems (for example audio skipped on a very long recording).
    pub warnings: Vec<String>,
    /// Output seconds per wall second.
    pub realtime_factor: f64,
    /// GPU adapter used.
    pub adapter: String,
    /// WARP software rasterizer was used.
    pub software_gpu: bool,
    /// DXVA decode was used.
    pub hardware_decode: bool,
    /// A hardware H.264 encoder MFT was used.
    pub hardware_encode: bool,
    /// An AAC track was written.
    pub audio: bool,
    /// Output width.
    pub width: u32,
    /// Output height.
    pub height: u32,
    /// Output fps.
    pub fps: u32,
    /// Container.
    pub format: OutputFormat,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_json_matches_editor_shape() {
        let p: ExportPreset = serde_json::from_str(
            r#"{"aspect":"9:16","width":1080,"height":1920,"fps":30,"format":"mp4"}"#,
        )
        .unwrap();
        assert_eq!(p.aspect, Aspect::Portrait);
        assert_eq!((p.width, p.height, p.fps), (1080, 1920, 30));
        let s = serde_json::to_string(&ExportPreset::default()).unwrap();
        assert!(s.contains(r#""aspect":"16:9""#) && s.contains(r#""format":"mp4""#));
    }

    #[test]
    fn av1_format_is_an_mp4_container_with_a_lower_bitrate() {
        let p: ExportPreset = serde_json::from_str(r#"{"format":"av1"}"#).unwrap();
        assert_eq!(p.format, OutputFormat::Av1);
        assert_eq!(
            serde_json::to_string(&OutputFormat::Av1).unwrap(),
            r#""av1""#
        );
        assert!(OutputFormat::Av1.is_mp4_container() && OutputFormat::Mp4.is_mp4_container());
        assert!(!OutputFormat::Gif.is_mp4_container());
        let h264 = ExportPreset::default().resolve((1920, 1080));
        let av1 = ExportPreset {
            format: OutputFormat::Av1,
            ..ExportPreset::default()
        }
        .resolve((1920, 1080));
        assert_eq!(
            (av1.width, av1.height, av1.fps),
            (h264.width, h264.height, h264.fps)
        );
        assert!(av1.bitrate < h264.bitrate && av1.bitrate >= 1_000_000);
    }

    #[test]
    fn resolve_caps_gif_and_derives_sizes() {
        let g = ExportPreset {
            width: 1920,
            height: 1080,
            fps: 60,
            format: OutputFormat::Gif,
            ..Default::default()
        }
        .resolve((2560, 1440));
        assert_eq!((g.width, g.height, g.fps), (1280, 720, 30));
        let sq = ExportPreset {
            aspect: Aspect::Square,
            width: 0,
            height: 0,
            ..Default::default()
        }
        .resolve((2560, 1440));
        assert_eq!((sq.width, sq.height), (1080, 1080));
        let odd = ExportPreset {
            width: 1279,
            height: 721,
            ..Default::default()
        }
        .resolve((10, 10));
        assert_eq!((odd.width % 2, odd.height % 2), (0, 0));
    }
}
