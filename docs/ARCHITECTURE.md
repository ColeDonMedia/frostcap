# Frostcap architecture (backend)

Frostcap is a free, MIT-licensed Windows 11 screen recorder/editor. This document
covers the Rust side. The UI <-> backend contract is in `docs/CONTRACT.md`
(golden JSON in `fixtures/`); the React UI is in `ui/` (`docs/UI.md`).

```
Crate dependency graph (arrows point at dependencies):

  src-tauri (frostcap) ---> frostcap-export ---> frostcap-core ---> frostcap-project
        |                          |                                       ^
        |                          +-----------> frostcap-render ----------+
        +---------------------------------------> frostcap-core
```

`frostcap-render` does **not** depend on `frostcap-core`, and `frostcap-core` does
not depend on `frostcap-render`: both build on the one typed, validated schema in
`frostcap-project`, so the renderer no longer re-parses `project.json` leniently.

```
crates/frostcap-project/     project.json schema (serde + serde_json only, no windows)
  lib.rs       Project + sub-types, limits, Project::load/validate/save, file-name
               constants, write_atomic, meta.json; re-exported by core::project
crates/frostcap-export/      export orchestration above core + render
(frostcap-render/examples/verify_edits.rs checks, on real exports, that every typed editor field reaches the file)
  lib.rs       export_with / export_project / needs_bake, ExportRequest/Result,
               raw copy (progress, cancel, atomic temp+rename) or baked render
crates/frostcap-core/        capture, input, recovery, zoom (no UI, unit-tested)
  sources.rs   monitors/windows/regions, flat UI list, validated resolution
  thumb.rs     one-frame WGC thumbnails -> WIC PNG data URIs (time-bounded)
  screen.rs    WGC free-threaded frame pool -> mf.rs on the GPU (crop, hold frame)
  mf.rs        IMFSinkWriter H.264/MP4 writer (D3D11 BGRA or NV12 input)
  webcam.rs    MF device enumeration + source reader (NV12) -> mf.rs
  audio.rs     WASAPI mic / loopback -> WAV, aligned to the timeline
  input.rs     120 Hz input sampler + crash-recovery sidecar (input.jsonl)
  timeline.rs  output timeline: t0 = first video frame, pauses removed, end
  recorder.rs  RecordingSession: start/pause/resume/health/stop, Drop cleanup
  recovery.rs  rebuild interrupted recordings from sidecars
  project.rs   re-export of frostcap-project (+ test samples, error conversion)
  autozoom.rs  pure auto-zoom generator (presets)
  dialogs.rs   IFileOpenDialog / IFileSaveDialog / reveal in Explorer
  region.rs    region picker overlay (layered topmost Win32 window)
  hotkeys.rs   RegisterHotKey on a message thread (configurable; default Ctrl+Alt+Shift+R / P)
  mp4.rs       fragmented/classic MP4 inspection (duration, playable)
  disk.rs      free-space checks;  com.rs  pinned MTA;  error.rs  codes
crates/frostcap-render/      export compositor; model built from frostcap-project::Project
crates/option-ext-shim/      MIT replacement for MPL option-ext
src-tauri/                   Tauri v2 shell: lib.rs (app), backend.rs (commands),
                             dto.rs (wire types + fixture test), paths.rs (path policy)
```

## Capture pipeline

`windows-capture` is used only for monitor/window enumeration. Capture and
encoding are our own code on the `windows` crate (same 0.62 major as Tauri):

* **Screen**: a D3D11 device (BGRA + video support, multithread protected), a
  WGC item, a *free-threaded* frame pool (no dispatcher queue / message loop).
  Each frame is copied (and cropped for regions) into a fresh texture and
  handed to an `IMFSinkWriter` with a DXGI device manager, so colour
  conversion and H.264 encoding stay on the GPU (OS encoder MFT, hardware when
  available). The encoder size is locked at start; window resizes are cropped
  or padded, never rescaled. `MinUpdateInterval` caps the fps (with a software
  guard). The sink writer throttles when the encoder falls behind, bounding
  memory.
* **Hold frame**: WGC only delivers frames on change. On stop the last texture
  is written again at the stop position, so the MP4 duration equals the
  recording duration (`durationMs`), verified by the smoke runs below.
* **Webcam**: MF source reader with video processing to NV12, best mode
  <= 1080p, written by the same `H264Writer`. Camera activation can take seconds,
  so it runs in the background; a missing camera is a warning, not an error.
* **Audio**: WASAPI shared mode, mix format written verbatim as WAV
  (`WAVE_FORMAT_EXTENSIBLE`). Every packet's QPC position is mapped through the
  timeline; pre-t0 and paused packets are dropped and silence is inserted when
  the file falls behind (loopback is silent while nothing plays), so each WAV
  starts at video t=0 and ends with the video. Headers are re-patched every 3 s.
* **COM**: windows-rs caches WinRT factories; tearing down the MTA left them
  dangling (an access violation found by the regression test
  `capture_after_thumbnails_and_hold_frame`). `com::ensure_mta` pins the MTA.

### Start latency

Measured on the dev machine (2560x1440, release build, `examples/record.rs`):

| | wave 1 | now (release) |
|---|---|---|
| first frame after `start` returns | 0 ms | 0 ms |
| `start` (cold process) | 2.9-3.4 s (2854 ms measured) | 1835 ms |
| `start` after `recorder::prewarm` | ~1.6 s (1389 ms measured) | 1327-1458 ms |

Where the time went (instrumented): the provisional `project.json` write (two
fsyncs, ~0.75 s) sat before the screen thread even started; it now runs on a
background thread (`ProvisionalSave`). The D3D device is shared across sessions
(`screen::shared_device`), and the encoder is created on its own thread while the
WGC pools and sessions are set up. What remains is the hardware H.264 encoder MFT
(`SetInputMediaType`, ~1.0-1.3 s even after prewarm): Media Foundation cannot hand
an encoder instance from one sink writer to the next, so it is the floor without
a custom byte-stream proxy. The countdown in the UI can hide the rest. Audio/video offsets
do not depend on start order.

An all-displays canvas wider than 4096 px (two 1440p monitors = 5120 px) made
the encoder initialize for ~13 s and fall behind (5 frames in 4 s). The canvas is
now scaled down to at most 4096 px per side and 36 864 macroblocks
(`canvas::fit_scale`; 5120x1440 becomes 4096x1152), which starts in ~2 s (debug build)
and keeps up (231 frames in 4.2 s on a scrolling console).

## Timeline, pause and sync

`timeline::Timeline` maps QPC timestamps (100 ns, same clock as WGC
`SystemRelativeTime` and WASAPI positions) to the output timeline: t0 = first
video frame, pauses removed, and a common end instant set at stop. Every track
(video, webcam, audio, input) goes through it, so all files line up and paused
time is cut everywhere. `project.json.pauses[]` records where pauses happened.

## Crash safety

* A provisional `project.json` (`status: "recording"`) is written at start.
* `input.jsonl` (raw input, appended every ~2 s) and `session.json` (t0, pauses,
  heartbeat every ~2 s) are kept while recording; WAV headers are re-patched.
* `stop` always writes the final `project.json` (with warnings if the encoder
  failed) and removes the sidecars. `RecordingSession` stops and finalizes on
  `Drop`. Closing the window / exiting the app stops the recording first.
* `screen.mp4` / `webcam.mp4` are fragmented MP4: verified with a real
  `process::abort()` mid-recording (`examples/record.rs --crash-after-ms`) that both
  files still parse and play with the expected duration. A static screen repeats
  its last frame every 0.5 s so fragments keep reaching disk. Samples are written
  one step late with their real duration (variable frame rate).
* `list_recent_projects` flags `interrupted` folders; `recover_project` rebuilds
  tracks, WAV headers, and attaches `webcam.mp4`; the screen duration comes from
  the fragmented file. Sidecars are size/line capped when read.
* Release builds use `panic = "unwind"` so `catch_unwind` around hotkey handlers
  and export jobs works and Drop-based cleanup finalizes recordings.

## Live errors

`RecordingSession::health()` reports encoder faults, source-window closure, low
disk (< 300 MB; start needs 1 GB), and audio/webcam thread failures. The Tauri
monitor thread emits `recording://warning` / `recording://error`, auto-stops on
errors and emits `recording://stopped`. Errors carry stable codes
(`disk_full`, `source_gone`, `no_h264`, `invalid_path`, `invalid_project`,
`permission`, `io`, `internal`).

## Privacy and security

* Keys: off by default; `shortcuts` records Win chords, chords with exactly one
  of Ctrl/Alt (Ctrl+Alt alone is AltGr) and F-keys; `all` is opt-in. Only
  key-downs, never text. Nothing is captured while paused. Keys and window titles
  are never written to the crash sidecar. Window titles are opt-in
  (`windowTitles`); by default the focus track stores opaque per-session ids.
  `scrub_project` removes keys/titles and leftover sidecars.
* Hotkeys default to uncommon chords, are configurable, never start a recording
  silently (only a visible window is told; the UI runs its countdown), and the
  WGC capture border stays on.
* The system cursor is not baked by default (`cursorBaked` in project.json).
* Path policy (`src-tauri/src/paths.rs`), project validation (size and range
  caps), compact JSON, `meta.json` for cheap listings.
* Commands are denied unless granted: `build.rs` registers an app manifest and
  `capabilities/default.json` lists `allow-<command>` plus the minimal core
  permissions (event listen, window controls). CSP: `default-src 'self'`, no
  remote origins, `object-src`/`base-uri`/`form-action` `'none'`,
  `connect-src` includes the asset protocol for waveform/Web Audio fetches.
* Asset protocol: static scope `$VIDEO/Frostcap/*.frostcap/*`; other project
  folders are allowed non-recursively when opened via the path policy. Tauri
  2's asset handler already sends `Access-Control-Allow-Origin: <window origin>`
  and exposes `content-range`, so `fetch` / Web Audio work without extra config.
* No network, no telemetry, no plugins, no updater.

## Export

`crates/frostcap-export` copies `screen.mp4` for unedited raw
exports or delegates to `frostcap-render` (MF decode -> wgpu DX12 -> MF
encode / GIF). The Tauri command adds the path policy, job ids,
`export://progress` with stages, and `cancel_export`.

## Licences

`cargo deny check` with a permissive allow-list (MIT, Apache-2.0, BSD, ISC,
Zlib, Unicode-3.0, ...). `option-ext` (MPL-2.0) is replaced by
`crates/option-ext-shim`. Tauri's codegen pulls MPL-2.0 `cssparser`,
`selectors`, `dtoa-short` into the *build-time* proc-macro only (not linked
into the exe); see `deny.toml` for how this is handled.
