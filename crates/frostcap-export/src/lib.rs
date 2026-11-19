//! (crate `frostcap-export`: sits above `frostcap-core` and `frostcap-render`.)
//! Export pipeline.
//!
//! * **Baked export** (every preset except a raw export of an unedited project):
//!   delegated to `frostcap-render` (Media Foundation decode -> wgpu DX12
//!   compositor -> Media Foundation H.264 + AAC, or GIF). `baked: true`.
//! * **Raw copy**: `ExportPreset::Raw` on a project without timeline edits
//!   (no zooms, cuts, speed changes or captions) streams `screen.mp4` to the
//!   output. `baked: false`.
//!
//! Both paths write to a unique temp file next to the output, rename it over the
//! target on success, delete it on error/cancel, and refuse to overwrite project
//! inputs.
//!
//! The editor sends `{ aspect, width, height, fps, format }` ([`OutputPreset`]);
//! the legacy named presets still deserialize ([`ExportRequest`]).

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

pub use frostcap_render::{Aspect, ExportPreset as OutputPreset, ExportStats, OutputFormat, Stage};

use frostcap_core::{Error, Result};
use frostcap_project::{PROJECT_FILE, Project, SCREEN_FILE, project_dir_of};

/// Legacy named presets.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum ExportPreset {
    /// Source resolution; plain copy of `screen.mp4` when the project has no
    /// timeline edits, otherwise baked at the source size.
    #[default]
    Raw,
    /// Baked 1920x1080 60 fps H.264 MP4.
    Mp4_1080p,
    /// Baked 3840x2160 60 fps H.264 MP4.
    Mp4_4k,
    /// Baked 1280x720 30 fps H.264 MP4.
    Web720p,
}

impl ExportPreset {
    /// Whether this preset always bakes (Raw bakes only when there are edits).
    pub fn is_baked(self) -> bool {
        self != Self::Raw
    }

    /// Equivalent output preset for a source of `canvas` px captured at `fps`.
    pub fn to_output(self, canvas: (u32, u32), fps: u32) -> OutputPreset {
        let mk = |aspect, width, height, fps| OutputPreset {
            aspect,
            width,
            height,
            fps,
            format: OutputFormat::Mp4,
        };
        match self {
            Self::Raw => mk(Aspect::Source, canvas.0, canvas.1, fps.clamp(1, 120)),
            Self::Mp4_1080p => mk(Aspect::Landscape, 1920, 1080, 60),
            Self::Mp4_4k => mk(Aspect::Landscape, 3840, 2160, 60),
            Self::Web720p => mk(Aspect::Landscape, 1280, 720, 30),
        }
    }
}

/// What `export_project` was asked for: the editor's preset object or a legacy name.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(untagged)]
pub enum ExportRequest {
    /// `{ aspect, width, height, fps, format }`.
    Output(OutputPreset),
    /// `"raw" | "mp4_1080p" | "mp4_4k" | "web720p"`.
    Legacy(ExportPreset),
}

impl Default for ExportRequest {
    fn default() -> Self {
        Self::Legacy(ExportPreset::Raw)
    }
}

/// Result of an export.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    /// Written file.
    pub out_path: PathBuf,
    /// Bytes written.
    pub bytes: u64,
    /// Whether edits were baked in (false only for the raw copy).
    pub baked: bool,
    /// Legacy preset requested (`raw` when an output preset was used).
    pub preset: ExportPreset,
    /// Output preset actually rendered (absent for the raw copy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputPreset>,
}

fn render_err(e: frostcap_render::RenderError) -> Error {
    match e {
        // Keep I/O errors typed (disk full, access denied, ...).
        frostcap_render::RenderError::Io { path, source } => Error::Io { path, source },
        frostcap_render::RenderError::Cancelled => Error::Invalid("export cancelled".into()),
        frostcap_render::RenderError::Project(m) => Error::InvalidProject(m),
        frostcap_render::RenderError::Settings(m) => {
            Error::Invalid(format!("export settings: {m}"))
        }
        // GPU / Media Foundation / audio failures are internal errors, not bad input.
        e => Error::Coded(frostcap_core::ErrorPayload {
            code: frostcap_core::ErrorCode::Internal,
            message: format!("export failed: {e}"),
        }),
    }
}

/// Load and validate through `frostcap-project` once, then build the render model
/// from the same typed project.
fn load_render_project(dir: &Path) -> Result<frostcap_render::RenderProject> {
    let project = Project::load(dir)?;
    frostcap_render::RenderProject::from_project(dir, &project).map_err(render_err)
}

/// Whether `preset` on this project needs the compositor (false = raw copy).
pub fn needs_bake(project_path: &Path, preset: ExportPreset) -> Result<bool> {
    if preset.is_baked() {
        return Ok(true);
    }
    let dir = project_dir_of(project_path);
    if !dir.join(PROJECT_FILE).is_file() {
        return Ok(false); // bare recording folder: nothing to bake
    }
    Ok(load_render_project(&dir)?.has_timeline_edits())
}

/// Export with a legacy preset. `progress` gets `0.0..=1.0`.
pub fn export_project(
    project_path: &Path,
    preset: ExportPreset,
    out_path: &Path,
    mut progress: impl FnMut(f64),
) -> Result<ExportResult> {
    let never = AtomicBool::new(false);
    export_with(
        project_path,
        &ExportRequest::Legacy(preset),
        out_path,
        &never,
        |p, _| progress(p),
    )
}

/// Export with progress `(0.0..=1.0, stage)` and a cancel flag.
pub fn export_with(
    project_path: &Path,
    request: &ExportRequest,
    out_path: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f64, &str),
) -> Result<ExportResult> {
    let dir = project_dir_of(project_path);
    let src = dir.join(SCREEN_FILE);
    if !src.is_file() {
        return Err(Error::Invalid(format!("missing {}", src.display())));
    }
    if fs::canonicalize(&src).ok() == fs::canonicalize(out_path).ok() {
        return Err(Error::Invalid(
            "refusing to overwrite the source recording".into(),
        ));
    }
    let (legacy, output) = match *request {
        ExportRequest::Legacy(l) => (l, None),
        ExportRequest::Output(o) => (ExportPreset::Raw, Some(o)),
    };
    if output.is_none() && !needs_bake(&dir, legacy)? {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Invalid("export cancelled".into()));
        }
        if let Some(parent) = out_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let bytes = copy_with_progress(&src, out_path, cancel, &mut progress)?;
        return Ok(ExportResult {
            out_path: out_path.to_path_buf(),
            bytes,
            baked: false,
            preset: legacy,
            output: None,
        });
    }
    bake(&dir, legacy, output, out_path, cancel, &mut progress)
}

#[cfg(windows)]
fn bake(
    dir: &Path,
    legacy: ExportPreset,
    output: Option<OutputPreset>,
    out_path: &Path,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(f64, &str),
) -> Result<ExportResult> {
    let project = load_render_project(dir)?;
    let preset = output.unwrap_or_else(|| legacy.to_output(project.canvas, project.fps));
    let opts = frostcap_render::ExportOptions::default();
    let stats =
        frostcap_render::export_project(&project, &preset, out_path, &opts, cancel, &mut |p, s| {
            progress(p, s.as_str())
        })
        .map_err(|e| match e {
            frostcap_render::RenderError::Cancelled => Error::Invalid("export cancelled".into()),
            e => render_err(e),
        })?;
    Ok(ExportResult {
        out_path: stats.out_path,
        bytes: stats.bytes,
        baked: true,
        preset: legacy,
        output: Some(preset),
    })
}

#[cfg(not(windows))]
fn bake(
    _: &Path,
    _: ExportPreset,
    _: Option<OutputPreset>,
    _: &Path,
    _: &AtomicBool,
    _: &mut impl FnMut(f64, &str),
) -> Result<ExportResult> {
    Err(Error::Invalid("baked export requires Windows".into()))
}

fn copy_with_progress(
    src: &Path,
    dst: &Path,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(f64, &str),
) -> Result<u64> {
    let total = fs::metadata(src)
        .map_err(|e| Error::io(src, e))?
        .len()
        .max(1);
    let mut input = fs::File::open(src).map_err(|e| Error::io(src, e))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let name = dst
        .file_name()
        .map_or_else(|| "export".into(), |n| n.to_string_lossy().into_owned());
    let tmp = dst.with_file_name(format!(".{name}.{}-{nanos}.partial", std::process::id()));
    let res = (|| {
        let mut output = fs::File::create(&tmp).map_err(|e| Error::io(&tmp, e))?;
        let mut buf = vec![0u8; 1 << 20];
        let mut done = 0u64;
        progress(0.0, "copying");
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Invalid("export cancelled".into()));
            }
            let n = input.read(&mut buf).map_err(|e| Error::io(src, e))?;
            if n == 0 {
                break;
            }
            output
                .write_all(&buf[..n])
                .map_err(|e| Error::io(&tmp, e))?;
            done += n as u64;
            progress((done as f64 / total as f64).min(1.0), "copying");
        }
        output.sync_all().map_err(|e| Error::io(&tmp, e))?;
        drop(output);
        fs::rename(&tmp, dst).map_err(|e| Error::io(dst, e))?;
        Ok(done)
    })();
    match &res {
        Ok(_) => progress(1.0, "done"),
        Err(_) => {
            let _ = fs::remove_file(&tmp);
        }
    }
    res
}
#[cfg(test)]
mod tests {
    use super::*;

    fn test_tmp_dir() -> PathBuf {
        let base = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
        let dir = base.join("test-tmp");
        fs::create_dir_all(&dir).expect("create test tmp dir");
        dir
    }

    #[test]
    fn raw_export_copies_with_progress() {
        let dir = test_tmp_dir().join(format!("export-{}.frostcap", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let data = vec![7u8; 3 * (1 << 20) + 5];
        fs::write(dir.join(SCREEN_FILE), &data).unwrap();
        let out = dir.join("out").join("export.mp4");
        let mut seen = Vec::new();
        let r = export_project(&dir, ExportPreset::Raw, &out, |p| seen.push(p)).unwrap();
        assert_eq!(r.bytes, data.len() as u64);
        assert_eq!(fs::read(&out).unwrap(), data);
        assert_eq!(seen.first().copied(), Some(0.0));
        assert_eq!(seen.last().copied(), Some(1.0));
        assert!(seen.windows(2).all(|w| w[0] <= w[1]));
        let _ = fs::remove_dir_all(&dir);
    }

    fn project_dir(name: &str, edits: serde_json::Value) -> PathBuf {
        let dir = test_tmp_dir().join(format!("{name}-{}.frostcap", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(SCREEN_FILE), b"not really an mp4").unwrap();
        let json = serde_json::json!({
            "version": 1, "name": name, "createdAtMs": 1,
            "source": {"kind": "monitor", "name": "D", "bounds": {"x": 0, "y": 0, "width": 2560, "height": 1440}},
            "canvas": {"width": 2560, "height": 1440}, "fps": 60, "durationMs": 3000,
            "media": {"screen": "screen.mp4"},
            "tracks": {"cursor": [], "clicks": [], "keys": [], "focus": []},
            "edits": edits
        });
        fs::write(dir.join("project.json"), serde_json::to_vec(&json).unwrap()).unwrap();
        dir
    }

    #[test]
    fn export_validates_project_with_core_loader() {
        // Only core validation knows this schema version is unsupported.
        let dir = project_dir("future", serde_json::json!({}));
        let mut v: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
        v["version"] = serde_json::json!(999);
        fs::write(dir.join("project.json"), serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(needs_bake(&dir, ExportPreset::Raw).is_err());
        assert!(export_project(&dir, ExportPreset::Mp4_1080p, &dir.join("o.mp4"), |_| {}).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hostile_media_paths_are_rejected() {
        for bad in [
            r"\\attacker\share\x.mp4",
            r"C:\Windows\win.ini",
            r"..\x.mp4",
        ] {
            let dir = project_dir("hostile", serde_json::json!({}));
            let mut v: serde_json::Value =
                serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
            v["media"]["screen"] = serde_json::json!(bad);
            fs::write(dir.join("project.json"), serde_json::to_vec(&v).unwrap()).unwrap();
            let out = dir.join("o.mp4");
            assert!(
                export_project(&dir, ExportPreset::Mp4_1080p, &out, |_| {}).is_err(),
                "{bad}"
            );
            assert!(!out.exists());
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn render_failures_get_honest_error_codes() {
        use frostcap_core::ErrorCode;
        use frostcap_render::RenderError as R;
        for e in [
            R::Gpu("device lost".into()),
            R::Media("0x8000 decode".into()),
            R::Audio("x".into()),
        ] {
            let m = e.to_string();
            let got = render_err(e);
            assert!(matches!(got, Error::Coded(_)), "{got:?}");
            assert_eq!(got.code(), ErrorCode::Internal);
            assert!(got.to_string().contains(&m), "{got}");
        }
        let p = render_err(R::Project("bad canvas".into()));
        assert_eq!(p.code(), ErrorCode::InvalidProject);
        assert!(render_err(R::Cancelled).to_string().contains("cancelled"));
    }

    #[test]
    fn render_io_errors_stay_io_errors() {
        let e = render_err(frostcap_render::RenderError::io(
            Path::new("D:/full.mp4"),
            std::io::Error::from(std::io::ErrorKind::StorageFull),
        ));
        match e {
            Error::Io { path, source } => {
                assert_eq!(path, PathBuf::from("D:/full.mp4"));
                assert_eq!(source.kind(), std::io::ErrorKind::StorageFull);
            }
            other => panic!("expected Error::Io, got {other:?}"),
        }
    }

    #[test]
    fn legacy_presets_map_to_output_presets() {
        let canvas = (2560, 1440);
        let p = ExportPreset::Mp4_1080p.to_output(canvas, 60);
        assert_eq!(
            (p.width, p.height, p.fps, p.format),
            (1920, 1080, 60, OutputFormat::Mp4)
        );
        assert_eq!(ExportPreset::Mp4_4k.to_output(canvas, 60).width, 3840);
        let web = ExportPreset::Web720p.to_output(canvas, 60);
        assert_eq!((web.width, web.height, web.fps), (1280, 720, 30));
        let raw = ExportPreset::Raw.to_output(canvas, 30);
        assert_eq!(
            (raw.width, raw.height, raw.fps, raw.aspect),
            (2560, 1440, 30, Aspect::Source)
        );
        assert!(ExportPreset::Mp4_1080p.is_baked() && !ExportPreset::Raw.is_baked());
    }

    #[test]
    fn raw_copy_only_without_timeline_edits() {
        let plain = project_dir(
            "plain",
            serde_json::json!({"zoomSegments": [], "trims": []}),
        );
        assert!(!needs_bake(&plain, ExportPreset::Raw).unwrap());
        assert!(needs_bake(&plain, ExportPreset::Mp4_1080p).unwrap());
        let zoomed = project_dir(
            "zoomed",
            serde_json::json!({"zoomSegments": [{"startMs": 0, "endMs": 1000, "center": {"x": 0.5, "y": 0.5},
                "scale": 2.0, "easing": "linear", "transitionMs": 300}], "trims": []}),
        );
        assert!(needs_bake(&zoomed, ExportPreset::Raw).unwrap());
        // A raw export of an edited project goes through the compositor, never the copy.
        let out = zoomed.join("out.mp4");
        assert!(export_project(&zoomed, ExportPreset::Raw, &out, |_| {}).is_err());
        assert!(!out.exists());
        let _ = fs::remove_dir_all(&plain);
        let _ = fs::remove_dir_all(&zoomed);
    }

    #[test]
    fn never_overwrites_the_source_recording() {
        let dir = project_dir("selfcopy", serde_json::json!({}));
        let src = dir.join(SCREEN_FILE);
        assert!(export_project(&dir, ExportPreset::Raw, &src, |_| {}).is_err());
        assert_eq!(fs::read(&src).unwrap(), b"not really an mp4");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_leaves_no_output() {
        let dir = project_dir("cancel", serde_json::json!({}));
        let out = dir.join("o").join("x.mp4");
        let cancel = AtomicBool::new(true);
        let r = export_with(
            &dir,
            &ExportRequest::Legacy(ExportPreset::Raw),
            &out,
            &cancel,
            |_, _| {},
        );
        assert!(r.unwrap_err().to_string().contains("cancelled"));
        assert!(!out.exists());
        let leftovers = fs::read_dir(out.parent().unwrap())
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(leftovers, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn request_accepts_editor_preset_json() {
        let r: ExportRequest = serde_json::from_str(
            r#"{"aspect":"1:1","width":1080,"height":1080,"fps":30,"format":"gif"}"#,
        )
        .unwrap();
        assert!(
            matches!(r, ExportRequest::Output(p) if p.format == OutputFormat::Gif && p.width == 1080)
        );
        let l: ExportRequest = serde_json::from_str(r#""mp4_1080p""#).unwrap();
        assert_eq!(l, ExportRequest::Legacy(ExportPreset::Mp4_1080p));
    }

    #[test]
    fn missing_screen_is_an_error() {
        let dir = test_tmp_dir().join("does-not-exist.frostcap");
        assert!(export_project(&dir, ExportPreset::Raw, &dir.join("x.mp4"), |_| {}).is_err());
    }
}
