# Changelog

All notable changes to Frostcap are listed here. Versions follow
[semantic versioning](https://semver.org/); while below 1.0, a minor version adds features and
a patch version fixes bugs.

## [0.1.0] - 2026-11-19

First public release. Windows 10/11 x64; needs the Microsoft Edge WebView2 Runtime.

### Record
- Record a whole monitor, all monitors at once, a single window or a region you draw, with a
  3-2-1 countdown.
- Webcam, microphone and system audio on separate tracks, kept on one clock with drift
  correction for long sessions.
- Pause and resume; global hotkeys (Ctrl+Alt+Shift+R start/stop, Ctrl+Alt+Shift+P pause).
- Frostcap's own windows are hidden from recordings. The yellow Windows capture border can be
  turned off (Windows 11, with a confirmation).
- HDR monitors are tone-mapped to SDR video.
- Cursor path, clicks and window focus are saved so the cursor and zooms can be re-rendered.
- Optional keystroke capture for a key overlay (off by default).
- Crash-safe recording: if Frostcap or Windows stops, the recording can be recovered with
  about a second lost.

### Edit
- Automatic zoom that follows clicks and typing, editable on the timeline.
- Timeline with trims, splits, undo/redo, waveform and keyboard shortcuts.
- Speed regions (0.25x to 4x) and "Suggest cuts" for silent, idle stretches.
- Annotations: arrows, text, rectangles, ellipses, highlights, spotlight, blur and pixelate.
- Smooth, resizable cursor with motion blur and click ripples, or hide it.
- Backgrounds (gradients or your own image, with optional blur), padding, rounded corners,
  shadow and frames (minimal, Windows, macOS, laptop, phone).
- Webcam bubble with shape, corner, size, margin, border, mirror and colour controls, plus
  green-screen removal with a colour, gradient, image or blurred background.
- Captions from SRT/VTT files.
- Audio: per-track gain and mute, noise gate, high-pass filter and loudness normalisation.

### Export
- MP4 (H.264 + AAC through Windows Media Foundation, hardware encoder when available), AV1 in
  MP4 when the PC has an AV1 encoder, and animated GIF.
- 16:9, 9:16 and 1:1 presets, resolution and frame rate options, progress and cancel.
- Batch export of several sizes and formats in one go.
- GPU rendering (DirectX 12) with a software fallback.

### App
- Glassmorphism interface with light and dark themes, a solid mode for reduced transparency
  and full keyboard support.
- English, Spanish, German, French, Portuguese (Brazil) and Japanese.
- No network requests, no telemetry, no account, no watermarks.

### Known issues
- The executables are not code-signed, so Windows SmartScreen may warn on first run. Verify
  the download against `SHA256SUMS.txt`.
- Tested mainly on Windows 11 with standard (SDR) monitors. HDR capture, "All displays" with
  three or more monitors, and Windows 10 have had little real-world testing.
- Batch export avoids overwriting files it wrote in the same session, but can overwrite files
  with the same name from an earlier session.

[0.1.0]: https://github.com/ColeDonMedia/frostcap/releases/tag/v0.1.0
