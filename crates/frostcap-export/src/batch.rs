//! Batch export, cut suggestions and encoder capabilities (orchestration above
//! `frostcap-render`).

#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(windows)]
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

use frostcap_core::{Error, Result};

#[cfg(windows)]
use crate::{ExportPreset, ExportResult};
use crate::{OutputFormat, OutputPreset};

/// One requested output of a batch.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BatchRequest {
    /// Output preset (`{ aspect, width, height, fps, format }`).
    pub preset: OutputPreset,
    /// Destination (`.mp4` / `.gif`); validated by the caller's path policy.
    pub out_path: PathBuf,
}

/// Which encoders this PC offers.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// H.264 encoder MFT present.
    pub h264: bool,
    /// VP9 encoder MFT present.
    pub vp9: bool,
    /// AV1 encoder MFT present.
    pub av1: bool,
    /// Export `format` values that will work: `mp4`, `gif`, and `av1` when available.
    pub formats: Vec<OutputFormat>,
}

impl Capabilities {
    /// Build from raw flags (GIF is always possible: it has its own encoder).
    pub fn new(h264: bool, vp9: bool, av1: bool) -> Self {
        let mut formats = Vec::new();
        if h264 {
            formats.push(OutputFormat::Mp4);
        }
        formats.push(OutputFormat::Gif);
        if av1 {
            formats.push(OutputFormat::Av1);
        }
        Self {
            h264,
            vp9,
            av1,
            formats,
        }
    }
}

/// Reject presets whose encoder is missing, with a message the UI can show.
pub fn check_encoders(jobs: &[BatchRequest], caps: &Capabilities) -> Result<()> {
    for (i, j) in jobs.iter().enumerate() {
        let missing = |what: &str| {
            Error::Invalid(format!(
                "job {}: this PC has no {what} encoder (Media Foundation); pick another format",
                i + 1
            ))
        };
        match j.preset.format {
            OutputFormat::Av1 if !caps.av1 => return Err(missing("AV1")),
            OutputFormat::Mp4 if !caps.h264 => return Err(missing("H.264")),
            _ => {}
        }
    }
    Ok(())
}

/// Silence/idle suggestion options as sent by the UI (all optional).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SuggestRequest {
    /// dBFS, -100..=0 (default -45).
    pub rms_threshold_db: Option<f64>,
    /// ms, 200..=600000 (default 1500).
    pub min_idle_ms: Option<u64>,
    /// ms, 0..=5000 (default 200).
    pub margin_ms: Option<u64>,
    /// px, 0.5..=200 (default 3).
    pub cursor_min_px: Option<f64>,
}

impl SuggestRequest {
    /// Validate (out-of-range or non-finite values are rejected, not clamped).
    pub fn validate(&self) -> Result<frostcap_render::suggest::SuggestOptions> {
        let mut o = frostcap_render::suggest::SuggestOptions::default();
        let bad = |name: &str, range: &str| Error::Invalid(format!("{name} must be {range}"));
        if let Some(v) = self.rms_threshold_db {
            if !(v.is_finite() && (-100.0..=0.0).contains(&v)) {
                return Err(bad("rmsThresholdDb", "-100..0"));
            }
            o.rms_threshold_db = v;
        }
        if let Some(v) = self.min_idle_ms {
            if !(200..=600_000).contains(&v) {
                return Err(bad("minIdleMs", "200..600000"));
            }
            o.min_idle_ms = v;
        }
        if let Some(v) = self.margin_ms {
            if v > 5000 {
                return Err(bad("marginMs", "0..5000"));
            }
            o.margin_ms = v;
        }
        if let Some(v) = self.cursor_min_px {
            if !(v.is_finite() && (0.5..=200.0).contains(&v)) {
                return Err(bad("cursorMinPx", "0.5..200"));
            }
            o.cursor_min_px = v;
        }
        Ok(o)
    }
}

/// Encoders available on this machine.
#[cfg(windows)]
pub fn capabilities() -> Capabilities {
    let c = frostcap_render::mf::encoder_caps();
    Capabilities::new(c.h264, c.vp9, c.av1)
}

/// Suggest cuts for the project in `dir`.
#[cfg(windows)]
pub fn suggest_cuts(dir: &Path, req: &SuggestRequest) -> Result<Vec<frostcap_project::TrimRange>> {
    let opts = req.validate()?;
    let project = frostcap_project::Project::load(dir)?;
    frostcap_render::suggest::suggest_cuts(dir, &project, &opts).map_err(crate::render_err)
}

/// Render several outputs of one project in a single pass. Returns one result per
/// job; `Err` when the whole batch is rejected up front. `progress` gets
/// `(job index, 0..=1, stage)`.
#[cfg(windows)]
pub fn export_batch(
    dir: &Path,
    jobs: &[BatchRequest],
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize, f64, &str),
) -> Result<Vec<Result<ExportResult>>> {
    check_encoders(jobs, &capabilities())?;
    let project = crate::load_render_project(dir)?;
    let render_jobs: Vec<_> = jobs
        .iter()
        .map(|j| frostcap_render::BatchJob {
            preset: j.preset,
            out_path: j.out_path.clone(),
        })
        .collect();
    let opts = frostcap_render::ExportOptions::default();
    let results =
        frostcap_render::export_batch(&project, &render_jobs, &opts, cancel, &mut |i, p, s| {
            progress(i, p, s.as_str())
        });
    Ok(results
        .into_iter()
        .zip(jobs)
        .map(|(r, j)| {
            r.map_err(crate::render_err).map(|s| ExportResult {
                out_path: s.out_path,
                bytes: s.bytes,
                baked: true,
                preset: ExportPreset::Raw,
                output: Some(j.preset),
            })
        })
        .collect())
}

/// Release the shared GPU device (call when the app exits).
#[cfg(windows)]
pub fn release_gpu() {
    frostcap_render::gpu::Gpu::release_shared();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(format: OutputFormat) -> BatchRequest {
        BatchRequest {
            preset: OutputPreset {
                format,
                ..OutputPreset::default()
            },
            out_path: PathBuf::from("out.mp4"),
        }
    }

    #[test]
    fn av1_is_rejected_without_an_encoder_with_a_clear_message() {
        let none = Capabilities::new(true, false, false);
        let e = check_encoders(&[job(OutputFormat::Mp4), job(OutputFormat::Av1)], &none)
            .unwrap_err()
            .to_string();
        assert!(e.contains("job 2") && e.contains("AV1"), "{e}");
        let some = Capabilities::new(true, false, true);
        assert!(check_encoders(&[job(OutputFormat::Av1)], &some).is_ok());
        assert!(check_encoders(&[job(OutputFormat::Gif)], &Capabilities::default()).is_ok());
        assert!(check_encoders(&[job(OutputFormat::Mp4)], &Capabilities::default()).is_err());
    }

    #[test]
    fn capabilities_list_the_usable_formats() {
        assert_eq!(
            Capabilities::new(true, false, false).formats,
            [OutputFormat::Mp4, OutputFormat::Gif]
        );
        assert_eq!(
            Capabilities::new(true, true, true).formats,
            [OutputFormat::Mp4, OutputFormat::Gif, OutputFormat::Av1]
        );
        let v = serde_json::to_value(Capabilities::new(true, false, true)).unwrap();
        assert_eq!(v["formats"], serde_json::json!(["mp4", "gif", "av1"]));
        assert_eq!(v["av1"], true);
    }

    #[test]
    fn suggest_options_are_validated_not_silently_clamped() {
        let d = SuggestRequest::default().validate().unwrap();
        assert_eq!((d.min_idle_ms, d.margin_ms), (1500, 200));
        let ok = SuggestRequest {
            rms_threshold_db: Some(-60.0),
            min_idle_ms: Some(200),
            margin_ms: Some(5000),
            cursor_min_px: Some(0.5),
        };
        assert_eq!(ok.validate().unwrap().rms_threshold_db, -60.0);
        let base = SuggestRequest::default();
        for bad in [
            SuggestRequest {
                rms_threshold_db: Some(1.0),
                ..base
            },
            SuggestRequest {
                rms_threshold_db: Some(f64::NAN),
                ..base
            },
            SuggestRequest {
                min_idle_ms: Some(199),
                ..base
            },
            SuggestRequest {
                min_idle_ms: Some(600_001),
                ..base
            },
            SuggestRequest {
                margin_ms: Some(5001),
                ..base
            },
            SuggestRequest {
                cursor_min_px: Some(0.1),
                ..base
            },
            SuggestRequest {
                cursor_min_px: Some(201.0),
                ..base
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        let parsed: SuggestRequest = serde_json::from_str(r#"{"minIdleMs":3000}"#).unwrap();
        assert_eq!(parsed.min_idle_ms, Some(3000));
    }
}
