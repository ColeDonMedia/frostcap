# Frostcap UI <-> backend contract

Rust is the source of truth. The project schema (`project.json`, validation,
limits) lives in `crates/frostcap-project` (re-exported by `frostcap-core::project`);
export orchestration (`export_with`, `ExportRequest`/`ExportResult`) in
`crates/frostcap-export`; capture, sources, recent, autozoom, region, webcam,
hotkeys and error in `crates/frostcap-core`; the wire types in
`src-tauri/src/dto.rs`. Golden JSON generated from those types is in
`fixtures/` (`project.sample.json`, `commands/<command>.json` with
`{ args, response, error? }`, `events/<event>.json`). They are checked by the
`dto::tests::golden_fixtures` test; regenerate after a deliberate change with:

```powershell
$env:FROSTCAP_UPDATE_FIXTURES='1'; cargo test -p frostcap --lib golden; Remove-Item Env:FROSTCAP_UPDATE_FIXTURES
```

## Conventions

* JSON keys are camelCase; enum values are camelCase strings (`"primaryMonitor"`,
  `"easeInOutCubic"`), except error codes, which are snake_case.
* Command arguments are named; Tauri maps `snake_case` Rust names to camelCase
  (`system_audio` -> `systemAudio`, `out_path` -> `outPath`, `job_id` -> `jobId`).
* Every command rejects with `{ code, message }`. Codes: `disk_full`,
  `source_gone`, `no_h264`, `invalid_path`, `invalid_project`, `permission`, `cancelled`,
  `io`, `internal`. Exception: when the *arguments* cannot be deserialized
  (wrong type, missing required field) Tauri itself rejects with a plain string
  before our code runs; treat a non-object rejection as `internal`.
* All paths returned by the backend are absolute and expanded (no `%VAR%`, no
  `\\?\` prefix).
* Every command must be granted in `src-tauri/capabilities/default.json`
  (`allow-<command>`); the app manifest in `build.rs` denies everything else.

## Units

* Time: `tMs`, `startMs`, `durationMs`... are **milliseconds on the output
  timeline**: 0 = first video frame of `screen.mp4`, paused time removed.
  `pauses[]` says where pauses happened (`atMs` on the output timeline,
  `durationMs` of wall time removed).
* `focus[]` entries carry `windowId` (1, 2, ... per session; equal = same window)
  and `title` (empty unless `windowTitles` was enabled).
* Track coordinates (`cursor`, `clicks`, `focus.rect`) are **physical pixels of
  `canvas`** (the `screen.mp4` pixel grid; for window sources relative to the
  window, for regions relative to the region). Normalize with
  `x / canvas.width`, `y / canvas.height`; values may fall outside `0..1` when
  the cursor leaves the source. Zoom centers and region rects are already
  normalized `0..1`.
* `canvas` is locked at record start (window resizes are cropped/padded, never
  rescaled), so coordinates stay valid for the whole recording.
* Audio and webcam `offsetMs` place the first sample/frame on the output
  timeline (normally 0; may be negative in recovered files).

## Commands

| command | args | response |
|---|---|---|
| `list_sources` | `thumbnails?: bool` (default true) | `{ monitors: MonitorInfo[], windows: WindowInfo[], sources: SourceItem[] }`; `SourceItem = { id, kind: "display"\|"window"\|"region", name, width, height, thumbnail?: "data:image/png;base64,...", source: CaptureSource }` |
| `list_cameras` | – | `{ id, name }[]` (empty = no camera) |
| `pick_region` | – | `{ monitorId, rect: {x,y,width,height} (0..1), screenRect }` or `null` (Esc / right-click) |
| `start_recording` | `source: CaptureSource \| string id`, `fps?`, `cursor?` (bake cursor, default false), `mic?`, `systemAudio?`, `keys?: "off"\|"shortcuts"\|"all"` (default off; `shortcuts` = Win chords, chords with exactly one of Ctrl/Alt, F-keys; Ctrl+Alt alone is AltGr and ignored), `webcam?: "default" \| cameraId`, `windowTitles?` (default false: focus track stores opaque `windowId`s only) | `{ projectDir, canvas, startMs }`. Rejected while another start is in progress. |
| `pause_recording` / `resume_recording` | – | `bool` (false if already paused / not paused) |
| `stop_recording` | – | `{ projectDir, durationMs, warnings }`. Idempotent: returns the last result when idle |
| `recording_state` | – | `{ recording, paused, elapsedMs, projectDir }` |
| `load_project` | `path` (folder or project.json) | `Project` |
| `save_project` | `path`, `project` | `null` (validated, atomic) |
| `list_recent_projects` | – | `{ path, name, createdAtMs, durationMs, canvas, status, interrupted }[]` |
| `recover_project` | `path` | `Project` (`status: "recovered"`) |
| `scrub_project` | `path`, `keys: bool`, `titles: bool` | `Project` (saved) |
| `generate_auto_zoom` | `project`, `preset?: "gentle"\|"balanced"\|"snappy"` | `ZoomSegment[]` |
| `export_project` | `path`, `preset?: {aspect,width,height,fps,format} \| "raw"\|"mp4_1080p"\|"mp4_4k"\|"web720p"`, `outPath` (.mp4/.gif), `jobId?` | `{ jobId, outPath, bytes, baked, preset, output? }` |
| `cancel_export` | `jobId` | `bool` |
| `pick_project` | – | path or `null` |
| `pick_folder` | `title?` | path or `null` |
| `pick_save_path` | `defaultName`, `ext` (`mp4`\|`gif`) | path or `null` |
| `reveal_in_folder` | `path` (project or export) | `null` |
| `default_output_dir` | – | absolute path of `Videos\Frostcap` |
| `hotkeys` | – | `{ chords: { action, chord, registered }[] }` (defaults `Ctrl+Alt+Shift+R` start/stop, `Ctrl+Alt+Shift+P` pause/resume) |
| `set_hotkeys` | `bindings: { startStop, pauseResume }` (chord strings: >= 2 modifiers, Ctrl or Win required, never Ctrl+Alt alone; keys A-Z 0-9 F1-F24; must differ) | same as `hotkeys`; persisted |
| `capture_settings` | – | `{ captureBorder, excludeOwnWindows }` (both default `true`; persisted in the app data folder) |
| `set_capture_border` | `enabled: bool` | same object. Turning it OFF first shows a native Yes/No box (default No); declining rejects with code `cancelled` and changes nothing (revert the toggle). Turning it on never asks. Shows (`true`) or hides (`false`) the Windows capture border for the *next* recording; hiding needs Windows 11 and, if refused, a `recording://warning` says so |
| `set_exclude_own_windows` | `enabled: bool` | same object. `true` hides Frostcap's own windows (main window, region overlay) from recordings via `WDA_EXCLUDEFROMCAPTURE` while recording; restored on stop Note: `WDA_EXCLUDEFROMCAPTURE` hides these windows from every capture tool (Teams, OBS...) while recording, not only from Frostcap. |
| `launch_options` | – | `{ selftest: bool }`; `selftest` is true only when the exe was started with the exact argument `--selftest` |
| `write_selftest_report` | `report` (a JSON *string*, <= 1 MB; the length is checked before parsing) | `null`. Self-test only: writes `<Videos>/Frostcap/selftest-report.json` |
| `selftest_cleanup` | `paths: string[]` | `null`. Self-test only: deletes a project folder that *this run* recorded and `frostcap-selftest-*.mp4` exports that *this run* wrote (both directly inside `Videos/Frostcap`); anything else is refused (nothing is deleted if any path is refused). After a successful export to `frostcap-selftest-*.mp4` the self-test run also gets that exact file added to the asset scope |
| `pick_image` | – | path or `null`. Native picker for `.png`/`.jpg`/`.jpeg`; the path becomes importable this session |
| `import_background` | `projectPath`, `sourcePath` | `{ file }` (`background-<hash>.png\|jpg`, copied into the project). `sourcePath` must come from `pick_image` (or be inside `Videos\Frostcap`), `.png/.jpg/.jpeg`, <= 20 MiB; the project must be a non-recording project under the path policy. The imported file is added to the asset scope (that exact file) so the editor preview can load it |
| `suggest_cuts` | `projectPath`, `options?: { rmsThresholdDb -100..0 (-45), minIdleMs 200..600000 (1500), marginMs 0..5000 (200), cursorMinPx 0.5..200 (3) }` | `{ ranges: { startMs, endMs }[] }` (cuts to remove; not applied or saved). Out-of-range options are rejected, not clamped |
| `encoder_capabilities` | – | `{ h264, vp9, av1, formats: ("mp4"\|"gif"\|"av1")[] }`. `export_project` / `export_batch` reject a `format` that is not listed (e.g. `"av1"` without an AV1 encoder: "this PC has no AV1 encoder ...") |
| `export_batch` | `projectPath`, `jobs: { preset, outPath }[]` (1..8; `preset` as in `export_project`), `jobId?` | `{ jobId, results: { index, outPath, ok, bytes?, output?, error?: {code,message} }[] }`, one per job in order; a failed job does not fail the others. Every `outPath` goes through the export target policy and no two may be the same file; nothing starts if any is refused. Progress uses `export://progress` with an extra `jobIndex`; `cancel_export(jobId)` cancels the whole batch. Successful outputs are remembered like `export_project` outputs |

`CaptureSource` (tagged by `kind`): `{kind:"primaryMonitor"}`,
`{kind:"monitor", id}`, `{kind:"window", hwnd, pid?}`,
`{kind:"region", monitorId, rect}`, `{kind:"allDisplays"}`. All displays records every monitor as one canvas (the union of their virtual-desktop bounds, each monitor copied 1:1, gaps black, `source.kind` stays `monitor`, `source.bounds` = the union, cursor coordinates relative to the union origin which may be negative). The flat list has an `{ id: "all-displays", kind: "allDisplays", ... }` item only when 2+ monitors are present. A union too big for the encoder is scaled down uniformly (at most 4096 px per side and 36 864 macroblocks; two 1440p monitors become 4096x1152): `canvas` is the scaled size and cursor/click/focus coordinates are already multiplied by the same factor, while `source.bounds` stays in desktop pixels. HDR monitors are captured as scRGB and tone-mapped to SDR BT.709 sRGB. String ids: `display:<id>`, `window:<hwnd>`,
`primary`.

### Path policy

`load_project`, `save_project`, `recover_project`, `scrub_project` and
`export_project` canonicalize paths and accept only `*.frostcap` folders that
contain `project.json` and are inside `Videos\Frostcap`, in the recent list, or
picked with `pick_project` / `pick_folder` this session. Export targets must be
`.mp4`/`.gif`, not inside a project folder, and either inside `Videos\Frostcap`
or exactly the file returned by `pick_save_path`. `..` is always rejected, and so is any file name with `:` (NTFS streams), wildcard or control characters, a trailing dot or space, or a reserved device name (`CON`, `NUL`, `COM1`...). Imported background images are re-granted to the asset scope on every project load.

### Project validation

`project.json` <= 64 MB; version 1; canvas 1..16384; fps 1..240; media file
names must be plain basenames; arrays capped (cursor 2M, clicks/keys 500k,
focus 100k, edits 10k); strings <= 4096 chars; `edits.ui` (opaque editor state,
round-tripped verbatim) <= 4 MB; style numbers finite and in range (ranges in the `edits` table below).

Limits (changed in the security pass; the UI must not exceed them):
`durationMs` <= 86 400 000 (24 h); `edits.style.padding` 0..0.45;
`edits.style.cursorScale` 0.1..8; `zoomSegments[].scale` 1..8 (center 0..1);
caption text <= 4096 bytes, <= 10 000 cues, <= 10 000 splits. Validation ranges
equal what the renderer clamps to, so a valid project renders exactly as stored.
Legacy `edits.ui` blobs are clamped into these ranges while migrating (inverted
cues are swapped, malformed cues dropped, oversized text cut), so old projects
still load. Media (WAV) accepted by export: 8/16/24/32-bit PCM or 32/64-bit
float, 1..32 channels, 8 000..768 000 Hz. Hotkeys (`set_hotkeys`): at least two
modifiers, Ctrl or Win required, never Ctrl+Alt alone (AltGr). A window source's
`source.name` is the generic `"Window"` unless window titles were opted in.

## Events

| event | payload |
|---|---|
| `recording://tick` | `{ elapsedMs, paused }` every 100 ms |
| `recording://warning` | `{ code: "warning", message }` |
| `recording://error` | `{ code, message }`; the session is then stopped automatically |
| `recording://stopped` | `{ result?: StopResponse, error?: {code,message} }` after an automatic or hotkey stop |
| `export://progress` | `{ jobId, path, outPath, progress (0..1), stage, jobIndex? }` (`jobIndex` only for `export_batch`) |
| `hotkey://pressed` | `{ action: "startStop"\|"pauseResume", handled }`. `handled: false` = start-stop while idle; it is only emitted when the main window is visible, and the UI must show its visible countdown before calling `start_recording` (never start silently) |

Closing the main window while recording stops and finalizes the recording.
The Windows capture border is on by default (`set_capture_border`).

## Media files

`screen.mp4` and `webcam.mp4` are **fragmented MP4** (H.264, `moov` + `moof`/`mdat`
fragments): a recording that crashes is still playable up to the last fragment
(at most ~1 s lost; a static screen is refreshed every 0.5 s). Samples carry their
real (variable) durations. Only media files (`*.mp4`, `*.wav`) of a project are
readable through the asset protocol; `project.json` and the sidecars are not.

## Project (`project.json`)

See `fixtures/project.sample.json` and `crates/frostcap-project/src/lib.rs`.
Top level: `version, name, createdAtMs, source {kind: monitor|window|region,
name, bounds, crop?}, canvas, fps, durationMs, media {screen, webcam?,
webcamVideo?, micAudio?, systemAudio?}, tracks {cursor, clicks, keys, focus},
edits {zoomSegments, trims, style, captions, splits, ui?}, pauses?, status
(recording|complete|recovered), warnings?, cursorBaked`.

### `edits` (what the exporter renders)

Everything the editor previews is typed, so the exported file matches it.
`edits = { zoomSegments, trims, style, captions, splits, speed, annotations, ui? }`. All new fields
are optional on read (missing -> the editor default below) and always written.

`edits.style` (the flat fields are unchanged):

| field | type / range | default | notes |
|---|---|---|---|
| `background, padding (0..0.45), cornerRadius (0..4096), shadow (0..1)` | as before | | |
| `cursorScale (0.1..8), cursorSmoothing (bool), showClicks, showKeys` | as before | | `cursorSmoothing` is the on/off switch |
| `frame` | `"none" / "minimal" / "windows" / "macos" / "laptop" / "phone"` | `"minimal"` | laptop / phone: see Editor v2 fields |
| `cursor` | `{ visible, smoothing 0..1, motionBlur 0..1 }` | `true, 0.6, 0.3` | smoothing amount applies while `cursorSmoothing` is true; a baked cursor (`cursorBaked`) is never drawn twice |
| `webcam` | `{ enabled, shape "circle" / "rounded" / "square", size 0.05..0.6, position "top-left" / "top-right" / "bottom-left" / "bottom-right" }` | `false, circle, 0.22, bottom-right` | `size` = bubble diameter / output height; needs `media.webcam` or `media.webcamVideo` |
| `audio` | `{ noiseGate, noiseGateThreshold -100..0 dB, highPass, highPassHz 10..1000, normalize, targetLufs -40..-5, micGain 0..4, systemGain 0..4 }` | `true, -50, true, 80, true, -16, 1, 1` | gain 0 = muted; gate / high-pass apply to the mic only |
| `captions` | `{ enabled, fontSize 8..200, position "bottom" / "top", background }` | `true, 40, bottom, true` | style only |

`edits.captions`: `[{ startMs, endMs, text }]` (video clock, `endMs >= startMs`,
text <= 4096 bytes, <= 10 000 cues). Cues with no visible text or zero length
are not rendered. `edits.splits`: `[ms]` (<= 10 000): clip boundaries inside kept
ranges, no visual effect (kept so the editor timeline round-trips).

`edits.ui` stays an opaque, size-capped blob for purely cosmetic editor state
(zoom defaults...). **Migration:** projects saved before these fields existed
carry them in a `{"v":1, frame, cursor, webcam, audio, captions, captionCues:
[{start,end,text}], splits}` blob; on load every group that is *absent* from
`edits.style` / `edits` is filled from that blob (groups that are present
win), so old projects keep their look. A project written by the new UI needs
no migration. See `fixtures/legacy/project.ui-v1.json`.

### Editor v2 fields (speed, annotations, webcam effects, images, devices)

All new fields are optional on read (missing -> the default below) and always
written, except the two optional image references (`style.backgroundImage`,
`webcam.background.image`), which are absent when unset. Ranges are enforced by
`Project::validate` (helper `check_range`, same rules as `check_f64`) and
clamped by the renderer; limits that the UI must mirror are in the tables.

`edits.speed`: `[{ startMs, endMs, speed }]` on the **source clock**, `speed`
0.25..4 (1 = normal), `endMs >= startMs`, at most 10 000, segments may touch but
not overlap. Parts outside every segment play at 1x; cuts (`trims`) are removed
first, so a segment spanning a cut only affects what remains. A ramp is several
adjacent segments. Video time and audio are both remapped (audio resamples:
pitch follows speed). Output length = the sum of `segment length / speed`.

`edits.annotations`: at most 500, drawn over the screen content after zoom/pan
(coordinates are `0..1` of the screen content, so they move with the zoom),
below the cursor, webcam and captions. At most 8 are drawn at once (the first 8
by start time).

| field | type / range | default | notes |
|---|---|---|---|
| `id` | string, <= 64 bytes | `""` | editor id |
| `kind` | `"arrow"` `"text"` `"rect"` `"ellipse"` `"highlight"` `"spotlight"` `"blur"` `"pixelate"` | `"arrow"` | |
| `startMs`, `endMs` | ms, `endMs >= startMs`, `endMs` <= 86 400 000 | `0, 0` | video clock; a zero-length annotation is not drawn |
| `x`, `y`, `w`, `h` | 0..1 | `0` | rect / ellipse / highlight / spotlight / blur / pixelate / text box; arrows use `x, y` as the start |
| `x2`, `y2` | 0..1 | `0` | arrow end |
| `color` | `"#RRGGBB"` | `"#4ed2fb"` | |
| `strokeWidth` | 1..40 | `4` | px at 1080p, scales with the output height (not the zoom) |
| `fill` | bool | `false` | rect / ellipse |
| `text` | string, <= 500 chars | `""` | `kind: "text"`, wrapped to the box width, centred, caption font |
| `fontSize` | 8..200 | `40` | px at 1080p |
| `opacity` | 0..1 | `1` | |
| `strength` | 0..1 | `0.5` | blur radius / pixel block size |
| `dim` | 0..1 | `0.6` | spotlight darkening outside the rect (screen content only) |
| `cornerRadius` | 0..200 | `0` | px at 1080p |
| `fadeMs` | 0..2000 | `150` | fade in and out |

`edits.style.frame` also accepts `"laptop"` (screen bezel + keyboard base) and
`"phone"` (thick rounded bezel + camera pill), drawn procedurally in the shader.
Device frames have no title bar and bring their own corner radius.

`edits.style.backgroundImage` (optional): `{ file, fit, blur }`; `file` is a
plain `.png`/`.jpg`/`.jpeg` basename inside the project folder (<= 20 MiB,
<= 16 384 px per side, <= 32 MP; produced by `import_background`), `fit`
`"cover"` (default) / `"contain"` (the gradient shows in the letterbox), `blur`
0..100 (Gaussian sigma, px at 1080p).

`edits.style.webcam` (new fields; existing ones unchanged):

| field | type / range | default | notes |
|---|---|---|---|
| `margin` | 0..0.2 | 0.0259259 (28 px at 1080p) | distance from the output edge, fraction of the output height |
| `borderWidth` | 0..20 | `0` | px at 1080p, drawn inside the bubble; 0 = none |
| `borderColor` | `"#RRGGBB"` | `"#ffffff"` | |
| `mirror` | bool | `false` | |
| `brightness` | -1..1 | `0` | |
| `contrast` | 0..2 | `1` | |
| `saturation` | 0..2 | `1` | |
| `shadow` | 0..1 | `0.4` | bubble drop shadow strength |
| `chromaKey` | `{ enabled, color, similarity, smoothness, spill }` | `false, "#00ff00", 0.4, 0.1, 0.5` | `similarity`, `smoothness`, `spill` each 0..1; a colour-distance key, no segmentation |
| `background` | `{ kind, color, gradient, image?, blur }` | `none, "#11141c", style.background default gradient, -, 0.5` | what shows where the camera was keyed out: `kind` `"none"` (transparent) / `"color"` / `"gradient"` (CSS `linear-gradient`, first and last stop) / `"image"` (project-folder basename, same rules as `backgroundImage`) / `"blur"` (blurred, desaturated copy of the camera frame); `blur` 0..1. Only applies while `chromaKey.enabled` |

`Rgba` colours are written `#rrggbb` (or `#rrggbbaa` when not opaque) and read as
`#rgb`, `#rrggbb` or `#rrggbbaa`.

#### Backend APIs for the command wave (Rust, no Tauri command yet)

* `frostcap_project::import_background(project_dir, source) -> Result<String>`:
  validates (extension, <= 20 MiB, PNG/JPEG magic, dimensions) and copies into the
  project as `background-<hash>.png|jpg`; returns the basename for
  `style.backgroundImage.file` / `webcam.background.image`.
* `frostcap_render::suggest::suggest_cuts(dir, &Project, &SuggestOptions) -> Result<Vec<TrimRange>>`
  (`rms_threshold_db` -100..0 default -45, `min_idle_ms` 200..600 000 default
  1500, `margin_ms` 0..5000 default 200, `cursor_min_px` 0.5..200 default 3).
* `frostcap_render::export_batch(&RenderProject, &[BatchJob], &ExportOptions,
  &cancel, &mut |job, fraction, stage| ..) -> Vec<Result<ExportStats>>`: one
  project, several presets, shared device/decoder, atomic outputs.
* `OutputFormat::Av1` (`"av1"`): AV1 + AAC in MP4, only when
  `frostcap_render::mf::encoder_caps().av1`; Media Foundation has no WebM muxer
  and no standalone VP9 path, so there is no `webm` format.
* `frostcap_render::gpu::Gpu::release_shared()` frees the cached GPU device.
