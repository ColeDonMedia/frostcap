# Frostcap privacy

**Short version:** Frostcap runs only on your computer. It has no servers, makes no network
requests and collects no telemetry. Everything it records is written to files on your disk
that you control, and nothing is shared unless you share it.

This document describes the app as built from this repository. It is written for users and
for contributors who change what is recorded (please keep it accurate).

## Network

- Frostcap makes **no network connections**: no update checks, analytics, crash reporting,
  licence checks, accounts, remote fonts or CDNs.
- The Tauri shell is built without the updater, HTTP or any other plugin, and the UI's
  Content Security Policy (`src-tauri/tauri.conf.json`) only allows the app's own files,
  the local asset protocol and IPC. Even a bug in the UI cannot load remote content.
- The system components Frostcap uses are maintained by Microsoft and have their own
  behaviour: the **WebView2 Runtime** (it updates itself through Windows/Edge update) and,
  in the installer only, the WebView2 bootstrapper, which downloads the runtime from
  Microsoft if it is missing. Frostcap does not send them any of your data.

## What is recorded, and only when you record

Nothing is captured until you press Record, and capture stops when you press Stop.

| Data | When | Where it goes |
|---|---|---|
| Screen video of the chosen monitor, window or region | Every recording | `screen.mp4` |
| Microphone audio | Only if the mic is enabled | `audio-mic.wav` |
| System audio (what your PC plays) | Only if system audio is enabled | `audio-system.wav` |
| Webcam video | Only if you choose a camera | `webcam.mp4` |
| Cursor position and mouse clicks (button, position, time) | Every recording | `project.json` (tracks) |
| Focused-window changes: **window title** and its position/size | Every recording | `project.json` (tracks) |
| Keystrokes | **Off by default**; only if you enable it | `project.json` (tracks) |
| Source name (monitor name or window title at record time) | Every recording | `project.json` |
| Your edits (trims, zooms, style) | When you edit | `project.json` |

### Keystroke capture

Keystroke capture is **off by default**. It has three modes:

- **Off** (default): no keys are recorded.
- **Shortcuts only**: keys pressed while Ctrl, Alt or Win is held, and F1-F24.
- **All keys**: every key-down, needed for the on-screen key overlay and typing-based
  auto-zoom.

When it is on, Frostcap stores only **key-down events**: the virtual-key code, a display label
(such as `A`, `Enter`, `F5`), the modifier keys held and the time. It does not store key
releases and never reconstructs text, but with "All keys" the sequence of letters you typed
can still be read from the list, including anything typed into other apps while recording
(for example a password typed during a recording). Use "Off" or "Shortcuts only" when that
matters, and do not share projects recorded with "All keys" without checking them.

Keys are read by polling the keyboard state about 120 times a second while recording. No
global keyboard hook is installed, and nothing is captured when not recording.

> Maintainer note: at the time of writing the key-capture mode exists in `frostcap-core`
> (`KeyCapture`, default `Off`) and the UI setting is being added. Re-check this section
> against the shipped UI before each release.

### Window titles

To make auto-zoom follow the window you work in, Frostcap records the title and bounds of the
foreground window each time it changes, even when you record a whole monitor. Titles can
contain document names, e-mail subjects, chat names or web page titles. They are stored in
`project.json`; they are not burned into the exported video.

## Where data is stored

| Item | Location (default) |
|---|---|
| Recordings (one folder each) | `%USERPROFILE%\Videos\Frostcap\Recording YYYY-MM-DD HH.MM.SS.frostcap\` |
| Exports | Wherever you choose to save them |
| Recent-projects list | `%APPDATA%\app.frostcap.desktop\recent.json` |
| UI settings (theme, preferences) | WebView2 local storage under `%LOCALAPPDATA%\app.frostcap.desktop\` |

A recording folder contains `screen.mp4`, optional `webcam.mp4`, `audio-mic.wav`,
`audio-system.wav`, `project.json` (tracks and edits) and `meta.json` (a small summary used by
the recent list: name, duration, size, status). While a recording is running, two journal
files, `input.jsonl` and `session.json`, are written into the folder so an interrupted
recording can be recovered. They are deleted when the recording finishes normally or is
recovered; they contain the same kind of data as the tracks above.

Source-picker thumbnails are generated in memory and are not saved.

## Sharing safely

Exported videos contain only the pixels and audio you exported. A **project folder** or
`project.json` contains much more (window titles, cursor path, and keys if enabled). Share the
exported video, not the project folder, unless you have checked it.

## How to remove data

- **One recording:** delete its `.frostcap` folder (in Explorer, or from the recent list if
  the app offers it). This removes video, audio and all tracks.
- **Scrub a project before sharing it:** remove keystrokes and window titles from
  `project.json` by editing the file. With Frostcap closed, in PowerShell:

  ```powershell
  $p = 'C:\path\to\Recording ....frostcap\project.json'
  $j = Get-Content $p -Raw | ConvertFrom-Json
  $j.tracks.keys = @()
  foreach ($f in $j.tracks.focus) { $f.title = '' }
  $j.source.name = ''
  $j | ConvertTo-Json -Depth 100 | Set-Content $p -Encoding utf8
  ```

  Field names follow the project format in `crates/frostcap-project/src/lib.rs`; check them
  against your file first and keep a backup. Typing-based auto-zoom segments that were
  already generated stay in `edits`, but no keys remain.
- **Recent list:** delete `%APPDATA%\app.frostcap.desktop\recent.json`.
- **Everything:** uninstall Frostcap, then delete `%APPDATA%\app.frostcap.desktop\`,
  `%LOCALAPPDATA%\app.frostcap.desktop\` and, if you want your recordings gone too,
  `%USERPROFILE%\Videos\Frostcap\`.

## Questions

Open an issue, or for anything sensitive use private reporting as described in
[SECURITY.md](../SECURITY.md).
