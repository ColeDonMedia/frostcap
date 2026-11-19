/**
 * Backend access + the complete wire <-> UI mapping.
 *
 * Rust (crates/frostcap-core, src-tauri/src/backend.rs) is the source of truth;
 * wire shapes are transcribed in contract.ts. Every conversion lives here:
 *   - pixels  <-> normalized 0..1 (using `canvas`)
 *   - `tMs`   <-> `t` / `startMs,endMs` <-> `start,end`
 *   - `trims` (cut ranges) <-> `clips` (kept ranges) + split points
 *   - style scales (padding fraction <-> %, shadow 0..1 <-> 0..100, ...)
 *   - CSS gradient string <-> {angle, stops}
 *   - UI-only editor state <-> `edits.ui`
 *   - string / {code,message} errors -> BackendError
 *
 * Without `window.__TAURI_INTERNALS__` every command goes to mockBackend.ts,
 * which speaks the same wire format, so the browser demo exercises this file.
 */
import type {
  WireCameraInfo,
  WireCaptureSource,
  WireClickEvent,
  WireEditList,
  WireExportProgress,
  WireExportResult,
  WireIssue,
  WireProject,
  WireRecentProject,
  WireHotkeyInfo,
  WireHotkeyPressed,
  WireRecordingState,
  WireRegionSelection,
  WireSourceList,
  WireStartResponse,
  WireStopResponse,
  WireStoppedEvent,
  WireStyle,
  WireTick,
  WireTrimRange,
  WireZoomPreset,
  WireZoomSegment,
  WireAnnotation,
  WireSpeedSegment,
  WireWebcam,
  WireExportPreset,
  WireOutputPreset,
} from "./contract";
import type {
  Annotation,
  SpeedSegment,
  WebcamStyle,
  CameraInfo,
  CaptureSource,
  Clip,
  ExportProgress,
  ExportRequest,
  ExportResult,
  GradientBackground,
  HotkeyChord,
  Issue,
  Project,
  ProjectStyle,
  RecentProject,
  RecordingResult,
  RecordingState,
  StartRecordingOptions,
  ZoomSegment,
} from "./types";
import { GRADIENTS, defaultStyle } from "./lib/presets";
import { t } from "./i18n";

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

export const isTauri = typeof window !== "undefined" && window.__TAURI_INTERNALS__ !== undefined;

export type Unlisten = () => void;

export const EVENTS = {
  recordingTick: "recording://tick",
  recordingError: "recording://error",
  recordingWarning: "recording://warning",
  recordingStopped: "recording://stopped",
  hotkeyPressed: "hotkey://pressed",
  exportProgress: "export://progress",
} as const;

// =================================================================== errors

export class BackendError extends Error {
  constructor(
    public code: string,
    message: string,
  ) {
    super(message);
    this.name = "BackendError";
  }
}

/** v1 commands return `Err(String)`; planned ones return `{code, message}`. Infer a code from v1 text. */
export function toBackendError(e: unknown): BackendError {
  if (e instanceof BackendError) return e;
  if (e && typeof e === "object" && "code" in e && "message" in e) {
    const w = e as WireIssue;
    return new BackendError(String(w.code), String(w.message));
  }
  const msg = e instanceof Error ? e.message : String(e);
  const rules: [RegExp, string][] = [
    [/command .* not found|unknown command|not allowed|not implemented/i, "notImplemented"],
    [/already in progress/i, "alreadyRecording"],
    [/^not recording/i, "notRecording"],
    [/capture source not found/i, "sourceNotFound"],
    [/unsupported project version/i, "unsupportedVersion"],
    [/project\.json error/i, "projectCorrupt"],
    [/audio error/i, "audio"],
    [/capture error/i, "capture"],
    [/I\/O error/i, "io"],
    [/missing .*screen\.mp4/i, "mediaMissing"],
  ];
  const hit = rules.find(([re]) => re.test(msg));
  return new BackendError(hit ? hit[1] : "unknown", msg);
}

/** Actionable, user-facing text per error code (Rust ErrorCode values first). */
export function describeIssue(i: Issue): string {
  switch (i.code) {
    case "disk_full":
      return t("issue.disk_full", { message: i.message });
    case "source_gone":
      return t("issue.source_gone", { message: i.message });
    case "no_h264":
      return t("issue.no_h264");
    case "invalid_path":
      return t("issue.invalid_path", { message: i.message });
    case "invalid_project":
      return t("issue.invalid_project", { message: i.message });
    case "permission":
      return t("issue.permission", { message: i.message });
    case "internal":
      return i.message;
    case "warning":
      return i.message;
    case "notImplemented":
      return t("issue.notImplemented");
    case "alreadyRecording":
      return t("issue.alreadyRecording");
    case "notRecording":
      return t("issue.notRecording");
    case "sourceNotFound":
      return t("issue.sourceNotFound");
    case "unsupportedVersion":
      return t("issue.unsupportedVersion");
    case "projectCorrupt":
      return t("issue.projectCorrupt");
    case "audio":
    case "micUnavailable":
      return t("issue.audio", { message: i.message });
    case "capture":
      return t("issue.capture", { message: i.message });
    case "io":
      return t("issue.io", { message: i.message });
    case "mediaMissing":
      return t("issue.mediaMissing");
    case "diskLow":
      return t("issue.diskLow", { message: i.message });
    case "systemAudioSilent":
      return t("issue.systemAudioSilent");
    default:
      return i.message;
  }
}

// ================================================================ transport

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

let invokeImpl: Invoke | null = null;
/** Tests: route every command to `fn` (null restores the real/mock transport). */
export function __setInvoke(fn: Invoke | null) {
  invokeImpl = fn;
}
async function getInvoke(): Promise<Invoke> {
  if (invokeImpl) return invokeImpl;
  if (isTauri) {
    const core = await import("@tauri-apps/api/core");
    invokeImpl = core.invoke as Invoke;
  } else {
    const m = await import("./mockBackend");
    invokeImpl = m.mockInvoke as Invoke;
  }
  return invokeImpl;
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const inv = await getInvoke();
  try {
    return await inv<T>(cmd, args);
  } catch (e) {
    throw toBackendError(e);
  }
}

async function listen<T>(event: string, cb: (payload: T) => void): Promise<Unlisten> {
  if (isTauri) {
    const ev = await import("@tauri-apps/api/event");
    return ev.listen<T>(event, (e) => cb(e.payload));
  }
  const m = await import("./mockBackend");
  return m.mockListen<T>(event, cb);
}

/** Webview URL for a local file (asset protocol). Null in the browser demo. */
let toAssetUrl: (absPath: string) => string | null = () => null;
async function ensureAssetUrl() {
  if (!isTauri) return;
  const core = await import("@tauri-apps/api/core");
  toAssetUrl = (p) => core.convertFileSrc(p);
}

// ============================================================ path helpers

export function projectDirOf(path: string): string {
  return path.replace(/[\\/]+project\.json$/i, "");
}

/**
 * Project folder + media file name. Defense in depth: media names come from project.json,
 * so only a bare file name is accepted (no drive, root, scheme, separators or "..").
 */
export function joinPath(dir: string, file: string): string {
  if (!file || file === "." || file === ".." || /[\\/:]/.test(file) || file.startsWith("..")) throw new Error(`unsafe media file name: ${file}`);
  const sep = dir.includes("\\") ? "\\" : "/";
  return dir.replace(/[\\/]+$/, "") + sep + file;
}

// ======================================================== sources / recent

export function fromWireSources(list: WireSourceList): CaptureSource[] {
  if (list.sources?.length) {
    return list.sources.map((s) => {
      const src = s.source;
      const mon = src.kind === "monitor" ? list.monitors.find((m) => m.id === src.id) : undefined;
      const win = src.kind === "window" ? list.windows.find((w) => w.hwnd === src.hwnd) : undefined;
      return {
        id: s.id,
        kind: s.kind,
        name: s.name,
        width: s.width,
        height: s.height,
        thumbnail: s.thumbnail ?? null,
        primary: mon?.primary,
        refreshRate: mon?.refreshRate,
        app: win?.processName || null,
        wire: src,
      } satisfies CaptureSource;
    });
  }
  const monitors: CaptureSource[] = list.monitors.map((m, i) => ({
    id: `monitor:${m.id}`,
    kind: "display",
    name: m.name || `Display ${i + 1}`,
    width: m.bounds.width,
    height: m.bounds.height,
    primary: m.primary,
    refreshRate: m.refreshRate,
    wire: { kind: "monitor", id: m.id },
  }));
  const windows: CaptureSource[] = list.windows.map((w) => ({
    id: `window:${w.hwnd}`,
    kind: "window",
    name: w.title || w.processName || "Window",
    app: w.processName || null,
    width: w.bounds.width,
    height: w.bounds.height,
    wire: { kind: "window", hwnd: w.hwnd },
  }));
  return [...monitors, ...windows];
}

export function fromWireRecent(r: WireRecentProject): RecentProject {
  return {
    path: r.path,
    name: r.name,
    durationMs: r.durationMs,
    createdAt: new Date(r.createdAtMs).toISOString(),
    width: r.canvas.width,
    height: r.canvas.height,
    interrupted: r.interrupted ?? false,
  };
}

// ================================================================ project

const r9 = (v: number) => Math.round(v * 1e9) / 1e9;
let idSeq = 0;
const nid = (p: string) => `${p}${(idSeq++).toString(36)}`;

/** Parse `#rrggbb` or `linear-gradient(<angle>deg, c1, c2, ...)`. */
export function parseBackground(css: string): GradientBackground {
  const s = css.trim();
  const m = s.match(/^linear-gradient\(\s*(-?[\d.]+)deg\s*,(.*)\)$/i);
  let g: GradientBackground;
  if (m) {
    const stops = splitTopLevel(m[2]).map((x) => x.trim()).filter(Boolean);
    g = { id: "custom", angle: Number(m[1]), stops };
  } else {
    g = { id: "custom", angle: 180, stops: [s] };
  }
  const preset = GRADIENTS.find((p) => p.angle === g.angle && p.stops.length === g.stops.length && p.stops.every((c, i) => c.toLowerCase() === g.stops[i].toLowerCase()));
  return preset ? { ...preset } : g;
}

function splitTopLevel(s: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let cur = "";
  for (const ch of s) {
    if (ch === "(") depth++;
    if (ch === ")") depth--;
    if (ch === "," && depth === 0) {
      out.push(cur);
      cur = "";
    } else cur += ch;
  }
  out.push(cur);
  return out;
}

/** Inverse of parseBackground. Single stop -> plain colour. No spaces (matches Rust default style). */
export function formatBackground(g: GradientBackground): string {
  return g.stops.length === 1 ? g.stops[0] : `linear-gradient(${g.angle}deg,${g.stops.join(",")})`;
}

/**
 * `edits.ui` now only holds cosmetic editor state (zoom defaults). Projects saved
 * by earlier builds carried frame/cursor/webcam/audio/captions/cues/splits there
 * (legacy v1 blob); those are read only when the typed groups are absent.
 */
interface UiBlob {
  v: 1;
  zoom?: ProjectStyle["zoom"];
  // legacy v1 (read only)
  frame?: ProjectStyle["frame"];
  cursor?: { visible: boolean; smoothing: number; motionBlur: number };
  webcam?: ProjectStyle["webcam"];
  audio?: ProjectStyle["audio"];
  captions?: ProjectStyle["captions"];
  captionCues?: { start: number; end: number; text: string }[];
  splits?: number[];
}

function readBlob(edits: WireEditList | undefined): Partial<UiBlob> {
  const u = edits?.ui;
  return u && typeof u === "object" && (u as UiBlob).v === 1 ? (u as UiBlob) : {};
}

/**
 * Backend validation ranges (docs/CONTRACT.md "edits", Project::validate).
 * limits.drift.test.ts parses both sources, so these must match exactly.
 */
export const LIMITS = {
  padding: [0, 0.45],
  cornerRadius: [0, 4096],
  zoomScale: [1, 8],
  shadow: [0, 1],
  cursorScale: [0.1, 8],
  unit: [0, 1],
  webcamSize: [0.05, 0.6],
  noiseGateThreshold: [-100, 0],
  highPassHz: [10, 1000],
  targetLufs: [-40, -5],
  gain: [0, 4],
  fontSize: [8, 200],
  webcamMargin: [0, 0.2],
  webcamBorderWidth: [0, 20],
  brightness: [-1, 1],
  /** webcam contrast and saturation */
  gain2: [0, 2],
  backgroundBlur: [0, 100],
  speed: [0.25, 4],
  annStroke: [1, 40],
  annCorner: [0, 200],
  annFadeMs: [0, 2000],
  maxAnnotations: 500,
  /** at most this many annotations are drawn at once (the first by start time) */
  maxAnnotationsDrawn: 8,
  maxAnnotationText: 500,
  maxIdBytes: 64,
  maxSpeed: 10_000,
  maxDurationMs: 86_400_000,
  maxCues: 10_000,
  maxSplits: 10_000,
  maxCueBytes: 4096,
  maxStringBytes: 4096,
} as const;

/**
 * Clamp into [lo, hi]. A non-finite value becomes `def` (clamped too): NaN
 * would serialize as `null` and make the backend reject the whole save.
 */
const clampTo = (v: number, [lo, hi]: readonly [number, number], def: number = lo) => {
  const x = Number.isFinite(v) ? v : def;
  return Math.min(hi, Math.max(lo, Number.isFinite(x) ? x : lo));
};
/** Whole, non-negative milliseconds (serde u64 rejects fractions and negatives). */
const ms = (v: number, def = 0) => Math.round(clampTo(v, [0, Number.MAX_SAFE_INTEGER], def));

/** Truncate to at most `max` UTF-8 bytes without splitting a code point. */
function utf8Truncate(s: string, max: number): string {
  const enc = new TextEncoder();
  if (enc.encode(s).length <= max) return s;
  let out = "";
  let n = 0;
  for (const ch of s) {
    const b = enc.encode(ch).length;
    if (n + b > max) break;
    out += ch;
    n += b;
  }
  return out;
}

/** Kept ranges = [0,duration] minus trims, then cut at split points. */
export function trimsToClips(trims: WireTrimRange[], durationMs: number, splits: number[] = []): Clip[] {
  const cuts = trims
    .map((t) => ({ s: Math.max(0, Math.min(t.startMs, durationMs)), e: Math.max(0, Math.min(t.endMs, durationMs)) }))
    .filter((t) => t.e > t.s)
    .sort((a, b) => a.s - b.s);
  const kept: [number, number][] = [];
  let cur = 0;
  for (const c of cuts) {
    if (c.s > cur) kept.push([cur, c.s]);
    cur = Math.max(cur, c.e);
  }
  if (cur < durationMs) kept.push([cur, durationMs]);
  const out: Clip[] = [];
  for (const [s, e] of kept) {
    const inner = splits.filter((p) => p > s && p < e).sort((a, b) => a - b);
    let a = s;
    for (const p of inner) {
      out.push({ id: nid("clip"), start: a, end: p });
      a = p;
    }
    out.push({ id: nid("clip"), start: a, end: e });
  }
  return out;
}

/** Inverse of trimsToClips. */
export function clipsToTrims(clips: Clip[], durationMs: number): { trims: WireTrimRange[]; splits: number[] } {
  const sorted = [...clips].sort((a, b) => a.start - b.start);
  const trims: WireTrimRange[] = [];
  const splits: number[] = [];
  let cur = 0;
  // Whole ms on the wire; a gap that rounds to nothing is dropped.
  const trim = (s: number, e: number) => {
    const [a, b] = [ms(s), ms(e)];
    if (b > a) trims.push({ startMs: a, endMs: b });
  };
  sorted.forEach((c, i) => {
    if (c.start > cur) trim(cur, c.start);
    else if (i > 0 && c.start === cur) splits.push(ms(cur));
    cur = Math.max(cur, c.end);
  });
  if (cur < durationMs) trim(cur, durationMs);
  return { trims, splits };
}

export function fromWireZoom(z: WireZoomSegment): ZoomSegment {
  return {
    id: nid("z"),
    start: z.startMs,
    end: z.endMs,
    cx: z.center.x,
    cy: z.center.y,
    scale: z.scale,
    easing: z.easing,
    transitionMs: z.transitionMs,
    auto: (z.origin ?? "auto") === "auto",
  };
}

/** UI -> wire, clamped to Project::validate (center 0..1, scale range, endMs >= startMs, whole ms). */
export function toWireZoom(z: ZoomSegment): WireZoomSegment {
  const d = defaultStyle().zoom;
  const startMs = ms(z.start);
  return {
    startMs,
    endMs: Math.max(startMs, ms(z.end, startMs)),
    center: { x: clampTo(z.cx, LIMITS.unit, 0.5), y: clampTo(z.cy, LIMITS.unit, 0.5) },
    scale: clampTo(z.scale, LIMITS.zoomScale, d.scale),
    easing: z.easing,
    transitionMs: ms(z.transitionMs, d.transitionMs),
    origin: z.auto ? "auto" : "manual",
  };
}

const WIRE_STYLE_DEFAULT: WireStyle = {
  background: "linear-gradient(135deg,#a1c4fd,#c2e9fb)",
  padding: 0.08,
  cornerRadius: 12,
  shadow: 0.5,
  cursorScale: 1,
  cursorSmoothing: true,
  showClicks: true,
  showKeys: false,
};

const HEX = /^#[0-9a-fA-F]{6}$/;
const hex = (v: unknown, def: string) => (typeof v === "string" && HEX.test(v) ? v : def);

/** Webcam style: every new field is optional on read (Rust defaults), deep-merged. */
function mergeWebcam(d: WebcamStyle, w: WireWebcam | undefined): WebcamStyle {
  if (!w) return structuredClone(d);
  const { chromaKey, background, ...rest } = w;
  const out: WebcamStyle = { ...d, ...rest, chromaKey: { ...d.chromaKey, ...(chromaKey ?? {}) }, background: { ...d.background, ...(background ?? {}) } };
  if (!background?.image) delete out.background.image;
  return out;
}

function toWireWebcam(w: WebcamStyle, d: WebcamStyle): NonNullable<WireStyle["webcam"]> {
  const L = LIMITS;
  const k = w.chromaKey;
  const b = w.background;
  return {
    enabled: w.enabled,
    shape: w.shape,
    size: clampTo(w.size, L.webcamSize, d.size),
    position: w.position,
    margin: clampTo(w.margin, L.webcamMargin, d.margin),
    borderWidth: clampTo(w.borderWidth, L.webcamBorderWidth, d.borderWidth),
    borderColor: hex(w.borderColor, d.borderColor),
    mirror: w.mirror,
    brightness: clampTo(w.brightness, L.brightness, 0),
    contrast: clampTo(w.contrast, L.gain2, 1),
    saturation: clampTo(w.saturation, L.gain2, 1),
    shadow: clampTo(w.shadow, L.unit, d.shadow),
    chromaKey: { enabled: k.enabled, color: hex(k.color, d.chromaKey.color), similarity: clampTo(k.similarity, L.unit, d.chromaKey.similarity), smoothness: clampTo(k.smoothness, L.unit, d.chromaKey.smoothness), spill: clampTo(k.spill, L.unit, d.chromaKey.spill) },
    background: {
      kind: b.kind,
      color: hex(b.color, d.background.color),
      gradient: new TextEncoder().encode(b.gradient).length > L.maxStringBytes ? d.background.gradient : b.gradient,
      ...(b.image ? { image: b.image } : {}),
      blur: clampTo(b.blur, L.unit, d.background.blur),
    },
  };
}

export function fromWireAnnotation(a: WireAnnotation): Annotation {
  return { id: a.id || nid("ann"), kind: a.kind, start: a.startMs, end: a.endMs, x: a.x, y: a.y, w: a.w, h: a.h, x2: a.x2, y2: a.y2, color: a.color, strokeWidth: a.strokeWidth, fill: a.fill, text: a.text, fontSize: a.fontSize, opacity: a.opacity, strength: a.strength, dim: a.dim, cornerRadius: a.cornerRadius, fadeMs: a.fadeMs };
}

const ANN_KINDS: readonly string[] = ["arrow", "text", "rect", "ellipse", "highlight", "spotlight", "blur", "pixelate"];

/** UI -> wire with every field clamped to the schema (a NaN or oversized value never reaches the backend). */
export function toWireAnnotation(a: Annotation): WireAnnotation {
  const L = LIMITS;
  const startMs = Math.min(L.maxDurationMs, ms(a.start));
  return {
    id: utf8Truncate(a.id, L.maxIdBytes),
    kind: (ANN_KINDS.includes(a.kind) ? a.kind : "arrow") as WireAnnotation["kind"],
    startMs,
    endMs: Math.min(L.maxDurationMs, Math.max(startMs, ms(a.end))),
    x: clampTo(a.x, L.unit, 0),
    y: clampTo(a.y, L.unit, 0),
    w: clampTo(a.w, L.unit, 0),
    h: clampTo(a.h, L.unit, 0),
    x2: clampTo(a.x2, L.unit, 0),
    y2: clampTo(a.y2, L.unit, 0),
    color: hex(a.color, "#4ed2fb"),
    strokeWidth: clampTo(a.strokeWidth, L.annStroke, 4),
    fill: !!a.fill,
    text: [...a.text].slice(0, L.maxAnnotationText).join(""),
    fontSize: clampTo(a.fontSize, L.fontSize, 40),
    opacity: clampTo(a.opacity, L.unit, 1),
    strength: clampTo(a.strength, L.unit, 0.5),
    dim: clampTo(a.dim, L.unit, 0.6),
    cornerRadius: clampTo(a.cornerRadius, L.annCorner, 0),
    fadeMs: Math.round(clampTo(a.fadeMs, L.annFadeMs, 150)),
  };
}

export function toWireSpeed(list: SpeedSegment[]): WireSpeedSegment[] {
  const sorted = list.map((s) => ({ startMs: ms(s.start), endMs: ms(s.end), speed: clampTo(s.speed, LIMITS.speed, 1) })).sort((a, b) => a.startMs - b.startMs);
  const out: WireSpeedSegment[] = [];
  for (const s of sorted) {
    const startMs = Math.max(s.startMs, out.at(-1)?.endMs ?? 0); // segments may touch but never overlap
    const endMs = Math.max(startMs, s.endMs);
    if (endMs > startMs) out.push({ startMs, endMs, speed: s.speed });
  }
  return out.slice(0, LIMITS.maxSpeed);
}

/** Typed groups win; a legacy v1 blob fills only the groups that are absent. */
export function fromWireStyle(ws: Partial<WireStyle> | undefined, x: Partial<UiBlob>): ProjectStyle {
  const w = { ...WIRE_STYLE_DEFAULT, ...(ws ?? {}) };
  const d = defaultStyle();
  const cur = w.cursor ?? x.cursor;
  return {
    background: parseBackground(w.background),
    padding: r9(w.padding * 100),
    radius: w.cornerRadius,
    shadow: r9(w.shadow * 100),
    frame: w.frame ?? x.frame ?? d.frame,
    zoom: { ...d.zoom, ...(x.zoom ?? {}) },
    cursor: {
      visible: cur?.visible ?? d.cursor.visible,
      smooth: w.cursorSmoothing,
      smoothing: cur?.smoothing ?? d.cursor.smoothing,
      size: w.cursorScale,
      clickRipple: w.showClicks,
      motionBlur: cur?.motionBlur ?? d.cursor.motionBlur,
    },
    keys: { show: w.showKeys },
    webcam: mergeWebcam(d.webcam, w.webcam ?? x.webcam),
    ...(w.backgroundImage ? { backgroundImage: { file: w.backgroundImage.file, fit: w.backgroundImage.fit === "contain" ? "contain" : "cover", blur: w.backgroundImage.blur } } : {}),
    audio: { ...d.audio, ...(w.audio ?? x.audio ?? {}) },
    captions: { ...d.captions, ...(w.captions ?? x.captions ?? {}) },
  };
}

/** UI style -> wire, clamped to the backend's ranges. */
/** UI style -> wire, clamped to the backend's ranges; non-finite numbers become the editor default. */
export function toWireStyle(s: ProjectStyle): WireStyle {
  const L = LIMITS;
  const d = defaultStyle();
  const a = s.audio;
  const da = d.audio;
  let background = formatBackground(s.background);
  if (new TextEncoder().encode(background).length > L.maxStringBytes) background = formatBackground(d.background);
  return {
    background,
    padding: r9(clampTo(s.padding / 100, L.padding, d.padding / 100)),
    cornerRadius: clampTo(s.radius, L.cornerRadius, d.radius),
    shadow: r9(clampTo(s.shadow / 100, L.shadow, d.shadow / 100)),
    cursorScale: clampTo(s.cursor.size, L.cursorScale, d.cursor.size),
    cursorSmoothing: s.cursor.smooth,
    showClicks: s.cursor.clickRipple,
    showKeys: s.keys.show,
    frame: s.frame,
    cursor: {
      visible: s.cursor.visible,
      smoothing: clampTo(s.cursor.smoothing, L.unit, d.cursor.smoothing),
      motionBlur: clampTo(s.cursor.motionBlur, L.unit, d.cursor.motionBlur),
    },
    webcam: toWireWebcam(s.webcam, d.webcam),
    ...(s.backgroundImage ? { backgroundImage: { file: s.backgroundImage.file, fit: s.backgroundImage.fit, blur: clampTo(s.backgroundImage.blur, L.backgroundBlur, 0) } } : {}),
    audio: {
      noiseGate: a.noiseGate,
      noiseGateThreshold: clampTo(a.noiseGateThreshold, L.noiseGateThreshold, da.noiseGateThreshold),
      highPass: a.highPass,
      highPassHz: clampTo(a.highPassHz, L.highPassHz, da.highPassHz),
      normalize: a.normalize,
      targetLufs: clampTo(a.targetLufs, L.targetLufs, da.targetLufs),
      micGain: clampTo(a.micGain, L.gain, da.micGain),
      systemGain: clampTo(a.systemGain, L.gain, da.systemGain),
    },
    captions: { enabled: s.captions.enabled, fontSize: clampTo(s.captions.fontSize, L.fontSize, d.captions.fontSize), position: s.captions.position, background: s.captions.background },
  };
}

/**
 * Wire -> UI. `dir` is the project folder (for media paths). Tracks are
 * normalized once here; `wire` is kept so saving never re-serializes them.
 */
export function fromWireProject(w: WireProject, dir: string, assetUrl: (abs: string) => string | null = () => null): Project {
  const W = w.canvas.width || 1;
  const H = w.canvas.height || 1;
  const x = readBlob(w.edits);
  const media = (file: string) => {
    let url: string | null = null;
    try {
      url = assetUrl(joinPath(dir, file));
    } catch {
      url = null; // hostile/odd name: never load it
    }
    return { file, url };
  };
  const audio = (a: NonNullable<WireProject["media"]["micAudio"]>) => ({ ...media(a.file), offsetMs: a.offsetMs, sampleRate: a.sampleRate, channels: a.channels, durationMs: a.durationMs });
  const t = w.tracks;
  const webcamFile = w.media.webcam ?? w.media.webcamVideo?.file;
  const cues = w.edits?.captions ? w.edits.captions.map((c) => ({ start: c.startMs, end: c.endMs, text: c.text })) : (x.captionCues ?? []);
  return {
    name: w.name,
    createdAt: new Date(w.createdAtMs).toISOString(),
    media: {
      dir,
      screen: media(w.media.screen),
      webcam: webcamFile ? media(webcamFile) : null,
      mic: w.media.micAudio ? audio(w.media.micAudio) : null,
      systemAudio: w.media.systemAudio ? audio(w.media.systemAudio) : null,
      width: W,
      height: H,
      fps: w.fps,
      durationMs: w.durationMs,
    },
    cursor: t.cursor.map((c) => ({ t: c.tMs, x: c.x / W, y: c.y / H })),
    clicks: t.clicks.map((c: WireClickEvent) => ({ t: c.tMs, x: c.x / W, y: c.y / H, button: c.button, state: c.state })),
    keys: t.keys.map((k) => ({ t: k.tMs, vk: k.vk, label: k.label, modifiers: { ...k.modifiers } })),
    focus: t.focus.map((f) => ({ t: f.tMs, windowId: f.windowId, title: f.title ?? "", rect: { x: f.rect.x / W, y: f.rect.y / H, width: f.rect.width / W, height: f.rect.height / H } })),
    zoomSegments: (w.edits?.zoomSegments ?? []).map(fromWireZoom).sort((a, b) => a.start - b.start),
    annotations: (w.edits?.annotations ?? []).map(fromWireAnnotation),
    speed: (w.edits?.speed ?? []).map((s) => ({ id: nid("spd"), start: s.startMs, end: s.endMs, speed: s.speed })),
    clips: trimsToClips(w.edits?.trims ?? [], w.durationMs, w.edits?.splits ?? x.splits ?? []),
    captions: cues.map((c) => ({ ...c, id: nid("c") })),
    style: fromWireStyle(w.edits?.style, x),
    cursorBaked: !!w.cursorBaked,
    wire: w,
  };
}

/** UI -> wire. Tracks, media, canvas etc. come from `p.wire` untouched; typed edits are always written. */
export function toWireProject(p: Project): WireProject {
  const { trims, splits } = clipsToTrims(p.clips, p.media.durationMs);
  const captions = p.captions.slice(0, LIMITS.maxCues).map((c) => {
    const startMs = Math.max(0, Math.round(c.start));
    return { startMs, endMs: Math.max(startMs, Math.round(c.end)), text: utf8Truncate(c.text, LIMITS.maxCueBytes) };
  });
  return {
    ...p.wire,
    name: p.name,
    edits: {
      zoomSegments: [...p.zoomSegments].sort((a, b) => a.start - b.start).map(toWireZoom),
      trims,
      style: toWireStyle(p.style),
      annotations: p.annotations.slice(0, LIMITS.maxAnnotations).map(toWireAnnotation),
      speed: toWireSpeed(p.speed),
      captions,
      splits: splits.slice(0, LIMITS.maxSplits),
      ui: { v: 1, zoom: p.style.zoom },
    },
  };
}

// ================================================================= the API

/** Backend-persisted capture options (both default to true). */
export interface CaptureSettings {
  captureBorder: boolean;
  excludeOwnWindows: boolean;
}

/** Wire shapes of the command-wave fixtures (fixtures/commands/{encoder_capabilities,export_batch,suggest_cuts}.json). */
export interface EncoderCaps {
  h264: boolean;
  av1: boolean;
  vp9: boolean;
  formats: string[];
}
export interface BatchJob {
  outPath: string;
  preset: WireExportPreset;
}
export type BatchResult = { index: number; outPath: string } & (
  | { ok: true; bytes: number; output?: WireOutputPreset }
  | { ok: false; error: { code: string; message: string } }
);
export interface SuggestOptions {
  rmsThresholdDb: number;
  minIdleMs: number;
  marginMs: number;
  cursorMinPx: number;
}

export const backend = {
  async listSources(): Promise<CaptureSource[]> {
    return fromWireSources(await call<WireSourceList>("list_sources", { thumbnails: true }));
  },

  /** Returns the new project folder. */
  async startRecording(o: StartRecordingOptions): Promise<string> {
    const r = await call<WireStartResponse>("start_recording", {
      source: o.source.wire satisfies WireCaptureSource,
      fps: o.fps,
      cursor: o.cursor,
      mic: o.mic,
      systemAudio: o.systemAudio,
      keys: o.keys,
      webcam: o.camera,
      // Sent only when opted in; the backend defaults to off.
      ...(o.windowTitles ? { windowTitles: true } : {}),
    });
    return r.projectDir;
  },

  pauseRecording: () => call<boolean>("pause_recording"),
  resumeRecording: () => call<boolean>("resume_recording"),

  async stopRecording(): Promise<RecordingResult> {
    return fromWireStop(await call<WireStopResponse>("stop_recording"));
  },

  async recordingState(): Promise<RecordingState> {
    const r = await call<WireRecordingState>("recording_state");
    return { status: !r.recording ? "idle" : r.paused ? "paused" : "recording", elapsedMs: r.elapsedMs, projectDir: r.projectDir };
  },

  async loadProject(path: string): Promise<Project> {
    await ensureAssetUrl();
    const w = await call<WireProject>("load_project", { path });
    return fromWireProject(w, projectDirOf(path), toAssetUrl);
  },

  async saveProject(path: string, project: Project): Promise<void> {
    await call<void>("save_project", { path, project: toWireProject(project) });
  },

  async listRecentProjects(): Promise<RecentProject[]> {
    return (await call<WireRecentProject[]>("list_recent_projects")).map(fromWireRecent);
  },

  /** Rebuilds an interrupted recording and returns the repaired project. */
  async recoverProject(path: string): Promise<Project> {
    await ensureAssetUrl();
    const w = await call<WireProject>("recover_project", { path });
    return fromWireProject(w, projectDirOf(path), toAssetUrl);
  },

  /** Removes key presses and/or window titles from a project (privacy). */
  async scrubProject(path: string, what: { keys: boolean; titles: boolean }): Promise<Project> {
    await ensureAssetUrl();
    const w = await call<WireProject>("scrub_project", { path, keys: what.keys, titles: what.titles });
    return fromWireProject(w, projectDirOf(path), toAssetUrl);
  },

  async generateAutoZoom(project: Project, preset: WireZoomPreset): Promise<ZoomSegment[]> {
    const segs = await call<WireZoomSegment[]>("generate_auto_zoom", { project: toWireProject(project), preset });
    return segs.map(fromWireZoom);
  },

  /** Resolves when the export finishes; progress arrives via onExportProgress for `jobId`. */
  async exportProject(req: ExportRequest): Promise<ExportResult> {
    const r = await call<WireExportResult>("export_project", { path: req.path, preset: req.preset, outPath: req.outPath, jobId: req.jobId });
    return { jobId: r.jobId, outPath: r.outPath, bytes: r.bytes, baked: r.baked };
  },

  cancelExport: (jobId: string) => call<boolean>("cancel_export", { jobId }),

  pickProject: () => call<string | null>("pick_project"),
  pickFolder: (title: string) => call<string | null>("pick_folder", { title }),
  /** The only way to get an export target the backend accepts. */
  pickSavePath: (defaultName: string, ext: string) => call<string | null>("pick_save_path", { defaultName, ext }),
  revealInFolder: (path: string) => call<void>("reveal_in_folder", { path }),
  pickRegion: () => call<WireRegionSelection | null>("pick_region"),
  defaultOutputDir: () => call<string>("default_output_dir"),
  async hotkeys(): Promise<HotkeyChord[]> {
    return (await call<WireHotkeyInfo>("hotkeys")).chords.map((c) => ({ ...c }));
  },
  /** Change the global hotkeys (validated and persisted by the backend). */
  async setHotkeys(bindings: { startStop: string; pauseResume: string }): Promise<HotkeyChord[]> {
    return (await call<WireHotkeyInfo>("set_hotkeys", { bindings: { startStop: bindings.startStop, pauseResume: bindings.pauseResume } })).chords.map((c) => ({ ...c }));
  },
  // ---- self-test (dev harness; see src/selftest). The backend adds these commands.
  /** `{selftest:false}` when the backend does not know the command. */
  async launchOptions(): Promise<{ selftest: boolean }> {
    try {
      const r = await call<{ selftest?: boolean } | null>("launch_options");
      return { selftest: r?.selftest === true };
    } catch {
      return { selftest: false };
    }
  },
  encoderCapabilities: () => call<EncoderCaps>("encoder_capabilities"),
  /** Native image dialog (the backend only imports what this returned). Null when cancelled. */
  pickImage: () => call<string | null>("pick_image"),
  /** Copies the picked image into the project; returns the basename for style.backgroundImage.file / webcam.background.image. */
  async importBackground(projectPath: string, sourcePath: string): Promise<string> {
    return (await call<{ file: string }>("import_background", { projectPath, sourcePath })).file;
  },
  async suggestCuts(projectPath: string, options: SuggestOptions): Promise<{ startMs: number; endMs: number }[]> {
    return (await call<{ ranges: { startMs: number; endMs: number }[] }>("suggest_cuts", { projectPath, options })).ranges;
  },
  exportBatch: (req: { projectPath: string; jobs: BatchJob[]; jobId: string }) => call<{ jobId: string; results: BatchResult[] }>("export_batch", req),
  async captureSettings(): Promise<CaptureSettings> {
    return { ...(await call<CaptureSettings>("capture_settings")) };
  },
  async setCaptureBorder(enabled: boolean): Promise<CaptureSettings> {
    return { ...(await call<CaptureSettings>("set_capture_border", { enabled })) };
  },
  async setExcludeOwnWindows(enabled: boolean): Promise<CaptureSettings> {
    return { ...(await call<CaptureSettings>("set_exclude_own_windows", { enabled })) };
  },
  /** The backend takes the report as a JSON string. */
  writeSelftestReport: (report: unknown) => call<void>("write_selftest_report", { report: JSON.stringify(report) }),
  /** Deletes only what the self-test created: its project folder and exported file. */
  selftestCleanup: (paths: string[]) => call<void>("selftest_cleanup", { paths }),
  /** Webview URL for an absolute media path (null in the browser). */
  async assetUrl(path: string): Promise<string | null> {
    await ensureAssetUrl();
    return toAssetUrl(path);
  },
  async listCameras(): Promise<CameraInfo[]> {
    return (await call<WireCameraInfo[]>("list_cameras")).map((c) => ({ id: c.id, name: c.name }));
  },

  // ---- events
  onRecordingTick: (cb: (elapsedMs: number, paused: boolean) => void) => listen<WireTick>(EVENTS.recordingTick, (p) => cb(p.elapsedMs, !!p.paused)),
  onRecordingError: (cb: (i: Issue) => void) => listen<WireIssue>(EVENTS.recordingError, (p) => cb(issue(p))),
  onRecordingWarning: (cb: (i: Issue) => void) => listen<WireIssue>(EVENTS.recordingWarning, (p) => cb(issue(p))),
  onRecordingStopped: (cb: (e: { result: RecordingResult | null; error: Issue | null }) => void) =>
    listen<WireStoppedEvent>(EVENTS.recordingStopped, (p) => cb({ result: p.result ? fromWireStop(p.result) : null, error: p.error ? issue(p.error) : null })),
  onHotkey: (cb: (action: WireHotkeyPressed["action"], handled: boolean) => void) => listen<WireHotkeyPressed>(EVENTS.hotkeyPressed, (p) => cb(p.action, p.handled)),
  onExportProgress: (cb: (p: ExportProgress) => void) =>
    listen<WireExportProgress>(EVENTS.exportProgress, (p) => cb({ jobId: p.jobId, progress: p.progress, stage: p.stage ?? "", ...(p.jobIndex !== undefined ? { jobIndex: p.jobIndex } : {}) })),
};

function fromWireStop(r: WireStopResponse): RecordingResult {
  return { projectPath: r.projectDir, durationMs: r.durationMs, warnings: r.warnings ?? [] };
}

function issue(p: WireIssue | string): Issue {
  return typeof p === "string" ? { code: "unknown", message: p } : { code: p.code, message: p.message };
}

// ========================================================== window controls

async function win() {
  if (!isTauri) return null;
  const w = await import("@tauri-apps/api/window");
  return w.getCurrentWindow();
}

export const windowControls = {
  async minimize() {
    await (await win())?.minimize();
  },
  async toggleMaximize() {
    await (await win())?.toggleMaximize();
  },
  async close() {
    await (await win())?.close();
  },
  async isMaximized(): Promise<boolean> {
    return (await (await win())?.isMaximized()) ?? false;
  },
};
