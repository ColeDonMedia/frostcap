/**
 * Wire types: the exact JSON shapes the Rust backend (de)serializes.
 * Source of truth: crates/frostcap-core, crates/frostcap-render and
 * src-tauri/src/{backend,dto}.rs; golden JSON in fixtures/ (generated from
 * Rust). Keep this file a literal transcription; all conversion to the UI
 * model lives in backend.ts.
 */

export interface WireSize {
  width: number;
  height: number;
}

export interface WireRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface WireNormRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

// ------------------------------------------------------------- sources.rs

export interface WireMonitorInfo {
  id: string;
  name: string;
  bounds: WireRect;
  refreshRate: number;
  scaleFactor: number;
  primary: boolean;
}

export interface WireWindowInfo {
  hwnd: number;
  pid: number;
  title: string;
  processName: string;
  bounds: WireRect;
}

/** `#[serde(tag = "kind", rename_all = "camelCase")]` */
export type WireCaptureSource =
  | { kind: "primaryMonitor" }
  | { kind: "monitor"; id: string }
  | { kind: "window"; hwnd: number; pid?: number }
  | { kind: "region"; monitorId: string; rect: WireNormRect }
  | { kind: "allDisplays" };

export interface WireSourceItem {
  /** Flat id: `display:\\.\DISPLAY1`, `window:65812`. */
  id: string;
  kind: "display" | "window" | "region" | "allDisplays";
  name: string;
  width: number;
  height: number;
  thumbnail?: string;
  source: WireCaptureSource;
}

export interface WireSourceList {
  monitors: WireMonitorInfo[];
  windows: WireWindowInfo[];
  /** UI-shaped list (with thumbnails unless `thumbnails: false`). */
  sources: WireSourceItem[];
}

/** `pick_region` response. */
export interface WireRegionSelection {
  monitorId: string;
  rect: WireNormRect;
  screenRect: WireRect;
}

/** `start_recording` `keys` argument (input.rs KeyCapture). */
export type WireKeyCapture = "off" | "shortcuts" | "all";

// ------------------------------------------------------------- backend.rs / dto.rs

export interface WireStartResponse {
  projectDir: string;
  canvas: WireSize;
  startMs: number;
}

export interface WireStopResponse {
  projectDir: string;
  durationMs: number;
  warnings: string[];
}

export interface WireRecordingState {
  recording: boolean;
  paused: boolean;
  elapsedMs: number;
  projectDir: string | null;
}

/** `recording://tick` */
export interface WireTick {
  elapsedMs: number;
  paused: boolean;
}

/**
 * Every command error, `recording://error`, and `recording://warning` (code
 * "warning"). Codes: disk_full, source_gone, no_h264, invalid_path,
 * invalid_project, permission, io, internal.
 */
export interface WireIssue {
  code: string;
  message: string;
}

/** `recording://stopped`: auto-stop (fatal error, window close...) or user stop. */
export interface WireStoppedEvent {
  result: WireStopResponse | null;
  error: WireIssue | null;
}

/** `export://progress` */
export interface WireExportProgress {
  jobId: string;
  path: string;
  outPath: string;
  progress: number;
  stage: string;
  jobIndex?: number;
}

export type WireHotkeyAction = "startStop" | "pauseResume";

/** `hotkeys` response. */
export interface WireHotkeyInfo {
  chords: { action: WireHotkeyAction; chord: string; registered: boolean }[];
}

/** `hotkey://pressed` */
export interface WireHotkeyPressed {
  action: WireHotkeyAction;
  handled: boolean;
}

// -------------------------------------------------------------- project.rs

export type WireSourceKind = "monitor" | "window" | "region";
export type WireMouseButton = "left" | "right" | "middle";
export type WireButtonState = "down" | "up";
export type WireEasing = "linear" | "easeInOutCubic" | "easeOutQuint" | "spring";
export type WireSegmentOrigin = "auto" | "manual";
export type WireProjectStatus = string;

export interface WireAudioTrack {
  file: string;
  offsetMs: number;
  sampleRate: number;
  channels: number;
  durationMs: number;
}

export interface WireWebcamVideo {
  file: string;
  offsetMs: number;
  width: number;
  height: number;
  durationMs: number;
  name?: string;
}

export interface WireMediaFiles {
  screen: string;
  webcam?: string;
  webcamVideo?: WireWebcamVideo;
  micAudio?: WireAudioTrack;
  systemAudio?: WireAudioTrack;
}

export interface WireCursorSample {
  tMs: number;
  x: number;
  y: number;
}

export interface WireClickEvent {
  tMs: number;
  x: number;
  y: number;
  button: WireMouseButton;
  state: WireButtonState;
}

export interface WireModifiers {
  ctrl: boolean;
  shift: boolean;
  alt: boolean;
  win: boolean;
}

export interface WireKeyEvent {
  tMs: number;
  vk: number;
  label: string;
  modifiers: WireModifiers;
}

export interface WireFocusChange {
  tMs: number;
  /** Opaque per-session window id (equal ids = same window). */
  windowId: number;
  /** Empty unless title capture was opted in. */
  title: string;
  rect: WireRect;
}

export interface WireTracks {
  cursor: WireCursorSample[];
  clicks: WireClickEvent[];
  keys: WireKeyEvent[];
  focus: WireFocusChange[];
}

export interface WireZoomSegment {
  startMs: number;
  endMs: number;
  center: { x: number; y: number };
  scale: number;
  easing: WireEasing;
  transitionMs: number;
  origin?: WireSegmentOrigin;
}

export interface WireTrimRange {
  startMs: number;
  endMs: number;
}

export interface WireWebcam {
  enabled: boolean;
  shape: "circle" | "rounded" | "square";
  size: number;
  position: "top-left" | "top-right" | "bottom-left" | "bottom-right";
  margin?: number;
  borderWidth?: number;
  borderColor?: string;
  mirror?: boolean;
  brightness?: number;
  contrast?: number;
  saturation?: number;
  shadow?: number;
  chromaKey?: { enabled: boolean; color: string; similarity: number; smoothness: number; spill: number };
  background?: { kind: "none" | "color" | "gradient" | "image" | "blur"; color: string; gradient: string; image?: string; blur: number };
}

export interface WireSpeedSegment {
  startMs: number;
  endMs: number;
  speed: number;
}

export interface WireAnnotation {
  id: string;
  kind: "arrow" | "text" | "rect" | "ellipse" | "highlight" | "spotlight" | "blur" | "pixelate";
  startMs: number;
  endMs: number;
  x: number;
  y: number;
  w: number;
  h: number;
  x2: number;
  y2: number;
  color: string;
  strokeWidth: number;
  fill: boolean;
  text: string;
  fontSize: number;
  opacity: number;
  strength: number;
  dim: number;
  cornerRadius: number;
  fadeMs: number;
}

export interface WireStyle {
  background: string;
  padding: number;
  cornerRadius: number;
  shadow: number;
  cursorScale: number;
  cursorSmoothing: boolean;
  showClicks: boolean;
  showKeys: boolean;
  /** Typed editor settings (optional on read, always written). */
  frame?: "none" | "minimal" | "windows" | "macos" | "laptop" | "phone";
  backgroundImage?: { file: string; fit: "cover" | "contain"; blur: number };
  cursor?: { visible: boolean; smoothing: number; motionBlur: number };
  webcam?: WireWebcam;
  audio?: {
    noiseGate: boolean;
    noiseGateThreshold: number;
    highPass: boolean;
    highPassHz: number;
    normalize: boolean;
    targetLufs: number;
    micGain: number;
    systemGain: number;
  };
  captions?: { enabled: boolean; fontSize: number; position: "bottom" | "top"; background: boolean };
}

export interface WireCaptionCue {
  startMs: number;
  endMs: number;
  text: string;
}

export interface WireEditList {
  zoomSegments: WireZoomSegment[];
  trims: WireTrimRange[];
  style: WireStyle;
  captions?: WireCaptionCue[];
  /** Optional on read (missing = none), always written. */
  speed?: WireSpeedSegment[];
  annotations?: WireAnnotation[];
  /** Clip boundaries inside kept ranges (timeline round-trip only). */
  splits?: number[];
  /** Opaque cosmetic editor state (zoom defaults), round-tripped verbatim by Rust (size-capped). */
  ui?: unknown;
}

export interface WireProject {
  version: number;
  status?: WireProjectStatus;
  name: string;
  createdAtMs: number;
  source: { kind: WireSourceKind; name: string; bounds: WireRect; /** Region sources: the captured rectangle within the monitor. */ crop?: WireRect };
  /** Non-fatal problems recorded with the project. */
  warnings?: string[];
  canvas: WireSize;
  fps: number;
  durationMs: number;
  /** True when the system cursor is baked into screen.mp4 (the editor must not draw a second one). */
  cursorBaked?: boolean;
  /** Pauses on the output timeline. */
  pauses?: { atMs: number; durationMs: number }[];
  media: WireMediaFiles;
  tracks: WireTracks;
  edits?: WireEditList;
}

// --------------------------------------------------------------- recent.rs

export interface WireRecentProject {
  path: string;
  name: string;
  createdAtMs: number;
  durationMs: number;
  canvas: WireSize;
  status?: WireProjectStatus;
  /** Interrupted recording that recover_project can rebuild. */
  interrupted?: boolean;
}

// --------------------------------------------------------------- export

/** Legacy names (serde camelCase of `Raw | Mp4_1080p | Mp4_4k | Web720p`). */
export type WireLegacyPreset = "raw" | "mp4_1080p" | "mp4_4k" | "web720p";

/** The editor's output preset (frostcap-render ExportPreset). width/height 0 = derive from aspect. */
export interface WireOutputPreset {
  aspect: "16:9" | "9:16" | "1:1" | "source";
  width: number;
  height: number;
  fps: number;
  format: "mp4" | "gif" | "av1";
}

/** `export_project` `preset` argument (untagged). */
export type WireExportPreset = WireOutputPreset | WireLegacyPreset;

export interface WireExportResult {
  jobId: string;
  outPath: string;
  bytes: number;
  baked: boolean;
  preset: WireLegacyPreset;
  output?: WireOutputPreset;
}

// -------------------------------------------------------------- autozoom.rs

export type WireZoomPreset = "gentle" | "balanced" | "snappy";

// --------------------------------------------------------------- webcam.rs

export interface WireCameraInfo {
  id: string;
  name: string;
}
