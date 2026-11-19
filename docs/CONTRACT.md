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
  `source_gone`, `no_h264`, `invalid_path`, `invalid_project`, `permission`,
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

`CaptureSource` (tagged by `kind`): `{kind:"primaryMonitor"}`,
`{kind:"monitor", id}`, `{kind:"window", hwnd, pid?}`,
`{kind:"region", monitorId, rect}`. String ids: `display:<id>`, `window:<hwnd>`,
`primary`.

### Path policy

`load_project`, `save_project`, `recover_project`, `scrub_project` and
`export_project` canonicalize paths and accept only `*.frostcap` folders that
contain `project.json` and are inside `Videos\Frostcap`, in the recent list, or
picked with `pick_project` / `pick_folder` this session. Export targets must be
`.mp4`/`.gif`, not inside a project folder, and either inside `Videos\Frostcap`
or exactly the file returned by `pick_save_path`. `..` is always rejected.

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
| `export://progress` | `{ jobId, path, outPath, progress (0..1), stage }` |
| `hotkey://pressed` | `{ action: "startStop"\|"pauseResume", handled }`. `handled: false` = start-stop while idle; it is only emitted when the main window is visible, and the UI must show its visible countdown before calling `start_recording` (never start silently) |

Closing the main window while recording stops and finalizes the recording.
The Windows capture border stays on while recording.

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
`edits = { zoomSegments, trims, style, captions, splits, ui? }`. All new fields
are optional on read (missing -> the editor default below) and always written.

`edits.style` (the flat fields are unchanged):

| field | type / range | default | notes |
|---|---|---|---|
| `background, padding (0..0.45), cornerRadius (0..4096), shadow (0..1)` | as before | | |
| `cursorScale (0.1..8), cursorSmoothing (bool), showClicks, showKeys` | as before | | `cursorSmoothing` is the on/off switch |
| `frame` | `"none" / "minimal" / "windows" / "macos"` | `"minimal"` | |
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
