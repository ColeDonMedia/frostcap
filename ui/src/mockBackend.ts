/**
 * In-browser stand-in for the Rust backend. It speaks the exact wire format
 * of src-tauri/src/backend.rs (see contract.ts) so the browser demo runs the
 * same adapter code as the desktop app. All data here is fake.
 */
import type {
  WireCaptureSource,
  WireClickEvent,
  WireCursorSample,
  WireExportPreset,
  WireExportProgress,
  WireExportResult,
  WireFocusChange,
  WireIssue,
  WireKeyEvent,
  WireProject,
  WireRecentProject,
  WireRecordingState,
  WireSourceList,
  WireStartResponse,
  WireStopResponse,
  WireTick,
  WireZoomPreset,
  WireZoomSegment,
  WireCameraInfo,
  WireHotkeyInfo,
  WireRegionSelection,
} from "./contract";

type Handler<T> = (payload: T) => void;
const listeners = new Map<string, Set<Handler<unknown>>>();

export function mockListen<T>(event: string, cb: Handler<T>): () => void {
  let set = listeners.get(event);
  if (!set) listeners.set(event, (set = new Set()));
  set.add(cb as Handler<unknown>);
  return () => set!.delete(cb as Handler<unknown>);
}

/** Tests: deliver an event as if the backend emitted it. */
export function __mockEmit<T>(event: string, payload: T) {
  emit(event, payload);
}

function emit<T>(event: string, payload: T) {
  listeners.get(event)?.forEach((h) => h(payload));
}

const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));
const ROOT = "C:\\Users\\demo\\Videos\\Frostcap";

// ---------------------------------------------------------------- sources

const SOURCES: WireSourceList = {
  monitors: [
    { id: "\\\\.\\DISPLAY1", name: "Dell U2720Q", bounds: { x: 0, y: 0, width: 2560, height: 1440 }, refreshRate: 60, scaleFactor: 1.5, primary: true },
    { id: "\\\\.\\DISPLAY2", name: "LG 24MP88", bounds: { x: 2560, y: 0, width: 1920, height: 1080 }, refreshRate: 75, scaleFactor: 1, primary: false },
  ],
  windows: [
    { hwnd: 197332, pid: 8120, title: "frostcap — Visual Studio Code", processName: "Code.exe", bounds: { x: 120, y: 80, width: 1800, height: 1100 } },
    { hwnd: 263010, pid: 9004, title: "Docs — Mozilla Firefox", processName: "firefox.exe", bounds: { x: 300, y: 60, width: 1600, height: 1000 } },
    { hwnd: 328554, pid: 11232, title: "Windows PowerShell", processName: "WindowsTerminal.exe", bounds: { x: 400, y: 300, width: 1280, height: 760 } },
  ],
  sources: [],
};
SOURCES.sources = [
  ...SOURCES.monitors.map((m) => ({ id: `display:${m.id}`, kind: "display" as const, name: m.name, width: m.bounds.width, height: m.bounds.height, source: { kind: "monitor" as const, id: m.id } })),
  // Like the backend: only offered when 2+ monitors fit one H.264 canvas (here 2560+1920 x 1440).
  { id: "all-displays", kind: "allDisplays" as const, name: "All displays", width: 4480, height: 1440, source: { kind: "allDisplays" as const } },
  ...SOURCES.windows.map((w) => ({ id: `window:${w.hwnd}`, kind: "window" as const, name: w.title, width: w.bounds.width, height: w.bounds.height, source: { kind: "window" as const, hwnd: w.hwnd, pid: w.pid } })),
];

const capture = { captureBorder: true, excludeOwnWindows: true };

const CAMERAS: WireCameraInfo[] = [
  { id: "cam:integrated", name: "Integrated Camera" },
  { id: "cam:usb", name: "Logitech C920" },
];

// -------------------------------------------------------------- recording

interface Active {
  dir: string;
  name: string;
  elapsedMs: number;
  paused: boolean;
  captureKeys: boolean;
  windowTitles: boolean;
  mic: boolean;
  systemAudio: boolean;
  timer: ReturnType<typeof setInterval>;
  last: number;
  warned: boolean;
}
let active: Active | null = null;
let recCounter = 0;

// ---------------------------------------------------------------- projects

const saved = new Map<string, WireProject>();

function rng(seed: number) {
  return () => {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function hash(s: string) {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
}

const hoursAgo = (h: number) => Date.now() - h * 3600_000;
const RECENT: WireRecentProject[] = [
  { path: `${ROOT}\\Onboarding walkthrough.frostcap`, name: "Onboarding walkthrough", createdAtMs: hoursAgo(0.3), durationMs: 94_000, canvas: { width: 1920, height: 1080 }, status: "complete", interrupted: false },
  { path: `${ROOT}\\Bug repro - save dialog.frostcap`, name: "Bug repro - save dialog", createdAtMs: hoursAgo(5), durationMs: 41_000, canvas: { width: 1920, height: 1080 }, status: "complete", interrupted: false },
  { path: `${ROOT}\\Recording 2026-11-16 18-02.frostcap`, name: "Recording 2026-11-16 18-02", createdAtMs: hoursAgo(20), durationMs: 37_000, canvas: { width: 1920, height: 1080 }, status: "recording", interrupted: true },
  { path: `${ROOT}\\Release 0.4 demo.frostcap`, name: "Release 0.4 demo", createdAtMs: hoursAgo(28), durationMs: 182_000, canvas: { width: 1920, height: 1080 }, status: "complete", interrupted: false },
  { path: `${ROOT}\\API quickstart.frostcap`, name: "API quickstart", createdAtMs: hoursAgo(200), durationMs: 128_000, canvas: { width: 1920, height: 1080 }, status: "complete", interrupted: false },
];

const VK_LABELS: [number, string][] = [
  [0x53, "S"],
  [0x0d, "Enter"],
  [0x4b, "K"],
  [0x50, "P"],
  [0x09, "Tab"],
];

/** A fake recording in the Rust project.json format (pixels, tMs). */
export function generateMockProject(dir: string, durationMs?: number, captureKeys = true, windowTitles = false): WireProject {
  const rand = rng(hash(dir));
  const recent = RECENT.find((r) => r.path === dir);
  const duration = Math.max(8000, Math.round(durationMs ?? recent?.durationMs ?? 42_000));
  const name = recent?.name ?? (dir.split(/[\\/]/).pop() ?? "Recording").replace(/\.frostcap$/, "");
  const W = 1920;
  const H = 1080;

  const clicks: WireClickEvent[] = [];
  let t = 1500 + rand() * 1500;
  while (t < duration - 1500) {
    const burst = 1 + Math.floor(rand() * 3);
    const bx = 0.15 + rand() * 0.7;
    const by = 0.15 + rand() * 0.7;
    for (let i = 0; i < burst && t < duration - 1000; i++) {
      const x = Math.round(Math.min(0.95, Math.max(0.05, bx + (rand() - 0.5) * 0.12)) * W);
      const y = Math.round(Math.min(0.95, Math.max(0.05, by + (rand() - 0.5) * 0.1)) * H);
      const button = rand() > 0.92 ? "right" : "left";
      clicks.push({ tMs: Math.round(t), x, y, button, state: "down" });
      clicks.push({ tMs: Math.round(t) + 90, x, y, button, state: "up" });
      t += 500 + rand() * 900;
    }
    t += 2500 + rand() * 4000;
  }

  const cursor: WireCursorSample[] = [];
  const downs = clicks.filter((c) => c.state === "down");
  const way = [{ tMs: 0, x: W / 2, y: H / 2 }, ...downs, { tMs: duration, x: W * 0.6, y: H * 0.55 }];
  let wi = 0;
  for (let s = 0; s <= duration; s += 8) {
    while (wi < way.length - 2 && way[wi + 1].tMs <= s) wi++;
    const a = way[wi];
    const b = way[wi + 1];
    const span = Math.max(1, b.tMs - a.tMs);
    const moveDur = Math.min(span, 700);
    const k = Math.min(1, Math.max(0, (s - (b.tMs - moveDur)) / moveDur));
    const e = k < 0.5 ? 4 * k * k * k : 1 - Math.pow(-2 * k + 2, 3) / 2;
    const x = Math.round(a.x + (b.x - a.x) * e + Math.sin(s / 310) * 6);
    const y = Math.round(a.y + (b.y - a.y) * e + Math.cos(s / 270) * 5);
    const prev = cursor[cursor.length - 1];
    if (!prev || prev.x !== x || prev.y !== y) cursor.push({ tMs: s, x, y });
  }

  const keys: WireKeyEvent[] = [];
  if (captureKeys) {
    for (let k = 3000; k < duration; k += 4000 + rand() * 6000) {
      const [vk, label] = VK_LABELS[Math.floor(rand() * VK_LABELS.length)];
      keys.push({ tMs: Math.round(k), vk, label, modifiers: { ctrl: rand() > 0.5, shift: false, alt: false, win: false } });
    }
  }

  // Titles are only recorded when the user opted in (start_recording windowTitles).
  const focus: WireFocusChange[] = [{ tMs: 0, windowId: 1, title: windowTitles ? "frostcap — Visual Studio Code" : "", rect: { x: 0, y: 0, width: W, height: H } }];

  const project: WireProject = {
    version: 1,
    status: "complete",
    cursorBaked: false,
    pauses: [],
    name,
    createdAtMs: recent?.createdAtMs ?? Date.now(),
    source: { kind: "monitor", name: "Dell U2720Q", bounds: { x: 0, y: 0, width: W, height: H } },
    canvas: { width: W, height: H },
    fps: 60,
    durationMs: duration,
    media: {
      screen: "screen.mp4",
      micAudio: { file: "audio-mic.wav", offsetMs: -40, sampleRate: 48000, channels: 1, durationMs: duration + 40 },
      systemAudio: { file: "audio-system.wav", offsetMs: 12, sampleRate: 48000, channels: 2, durationMs: duration - 12 },
    },
    tracks: { cursor, clicks, keys, focus },
    edits: {
      zoomSegments: mockAutoZoom({ canvas: { width: W, height: H }, durationMs: duration, clicks }, "balanced"),
      trims: [],
      style: {
        background: "linear-gradient(135deg,#a1c4fd,#c2e9fb)",
        padding: 0.08,
        cornerRadius: 12,
        shadow: 0.5,
        cursorScale: 1,
        cursorSmoothing: true,
        showClicks: true,
        showKeys: false,
        // Typed editor settings, Rust defaults (docs/CONTRACT.md "edits").
        frame: "minimal",
        cursor: { visible: true, smoothing: 0.6, motionBlur: 0.3 },
        webcam: {
          enabled: false,
          shape: "circle",
          size: 0.22,
          position: "bottom-right",
          margin: 28 / 1080,
          borderWidth: 0,
          borderColor: "#ffffff",
          mirror: false,
          brightness: 0,
          contrast: 1,
          saturation: 1,
          shadow: 0.4,
          chromaKey: { enabled: false, color: "#00ff00", similarity: 0.4, smoothness: 0.1, spill: 0.5 },
          background: { kind: "none", color: "#11141c", gradient: "linear-gradient(135deg,#a1c4fd,#c2e9fb)", blur: 0.5 },
        },
        audio: { noiseGate: true, noiseGateThreshold: -50, highPass: true, highPassHz: 80, normalize: true, targetLufs: -16, micGain: 1, systemGain: 1 },
        captions: { enabled: true, fontSize: 40, position: "bottom", background: true },
      },
      captions: [
        { startMs: 600, endMs: 3800, text: "Welcome to Frostcap — let's take a quick tour." },
        { startMs: 4200, endMs: 8200, text: "Click anywhere and the camera follows smoothly." },
      ].filter((c) => c.endMs < duration),
      splits: [],
      speed: [],
      annotations: [],
      ui: { v: 1, zoom: { preset: "balanced", scale: 1.8, easing: "easeInOutCubic", transitionMs: 650 } },
    },
  };
  return project;
}

const PRESET_PARAMS: Record<WireZoomPreset, { scale: number; leadIn: number; hold: number; transition: number; mergeGap: number; minDur: number; easing: WireZoomSegment["easing"] }> = {
  gentle: { scale: 1.5, leadIn: 700, hold: 1800, transition: 900, mergeGap: 3000, minDur: 2500, easing: "easeInOutCubic" },
  balanced: { scale: 1.8, leadIn: 450, hold: 1300, transition: 650, mergeGap: 2200, minDur: 1800, easing: "easeInOutCubic" },
  snappy: { scale: 2.2, leadIn: 250, hold: 900, transition: 350, mergeGap: 1400, minDur: 1200, easing: "easeOutQuint" },
};

/**
 * Simplified click-cluster auto-zoom for the demo only. The real algorithm
 * (clicks + typing + focus changes) is `generate_auto_zoom` in Rust; the UI
 * always calls the backend command and has no copy of its own.
 */
function mockAutoZoom(p: { canvas: { width: number; height: number }; durationMs: number; clicks: WireClickEvent[] }, preset: WireZoomPreset): WireZoomSegment[] {
  const P = PRESET_PARAMS[preset];
  const downs = p.clicks.filter((c) => c.state === "down").sort((a, b) => a.tMs - b.tMs);
  const out: WireZoomSegment[] = [];
  let group: WireClickEvent[] = [];
  const half = 0.5 / P.scale;
  const flush = () => {
    if (!group.length) return;
    const start = Math.max(0, group[0].tMs - P.leadIn);
    let end = Math.min(p.durationMs, group[group.length - 1].tMs + P.hold);
    if (end - start < P.minDur) end = Math.min(p.durationMs, start + P.minDur);
    const cx = group.reduce((s, c) => s + c.x, 0) / group.length / p.canvas.width;
    const cy = group.reduce((s, c) => s + c.y, 0) / group.length / p.canvas.height;
    const prev = out[out.length - 1];
    if (prev && start < prev.endMs) prev.endMs = Math.max(prev.endMs, end);
    else
      out.push({
        startMs: start,
        endMs: end,
        center: { x: Math.min(1 - half, Math.max(half, cx)), y: Math.min(1 - half, Math.max(half, cy)) },
        scale: P.scale,
        easing: P.easing,
        transitionMs: P.transition,
        origin: "auto",
      });
    group = [];
  };
  for (const c of downs) {
    if (group.length && c.tMs - group[group.length - 1].tMs > P.mergeGap) flush();
    group.push(c);
  }
  flush();
  return out;
}

const dirOf = (path: string) => path.replace(/[\\/]+project\.json$/i, "");
const fail = (code: string, message: string) => ({ code, message });
const isProjectPath = (p: string) => /\.frostcap$/i.test(dirOf(p));

function scrub(p: WireProject, keys: boolean, titles: boolean): WireProject {
  const out = structuredClone(p);
  if (keys) out.tracks.keys = [];
  if (titles) out.tracks.focus = out.tracks.focus.map((f) => ({ ...f, title: "" }));
  return out;
}

// ----------------------------------------------------------------- invoke

const COMMANDS: Record<string, (a: Record<string, unknown>) => unknown> = {
  async list_sources(): Promise<WireSourceList> {
    await delay(150);
    return structuredClone(SOURCES);
  },

  async list_cameras() {
    await delay(80);
    return structuredClone(CAMERAS);
  },

  async pick_region(): Promise<WireRegionSelection> {
    await delay(200);
    return { monitorId: SOURCES.monitors[0].id, rect: { x: 0.25, y: 0.25, width: 0.5, height: 0.5 }, screenRect: { x: 640, y: 360, width: 1280, height: 720 } };
  },

  async encoder_capabilities() {
    return { h264: true, av1: false, vp9: false, formats: ["mp4", "gif"] };
  },
  async pick_image() {
    return "C:\\Users\\demo\\Pictures\\wallpaper.png";
  },
  async import_background() {
    return { file: "background-0123456789abcdef.png" };
  },
  async suggest_cuts() {
    await delay(200);
    return { ranges: [{ startMs: 4200, endMs: 7800 }, { startMs: 12000, endMs: 15500 }] };
  },
  async export_batch(a: Record<string, unknown>) {
    const jobs = (a.jobs as { outPath: string; preset: unknown }[]) ?? [];
    await delay(300);
    return { jobId: String(a.jobId ?? ""), results: jobs.map((j, index) => ({ index, ok: true, outPath: j.outPath, bytes: 4_812_345, output: j.preset })) };
  },
  async capture_settings() {
    return { ...capture };
  },
  async set_capture_border(a: Record<string, unknown>) {
    // Like the app: turning the border off asks first; declining rejects with code "cancelled".
    if (a.enabled === false && typeof confirm === "function" && !confirm("Hide the capture border?")) throw { code: "cancelled", message: "cancelled" };
    capture.captureBorder = a.enabled === true;
    return { ...capture };
  },
  async set_exclude_own_windows(a: Record<string, unknown>) {
    capture.excludeOwnWindows = a.enabled === true;
    return { ...capture };
  },
  async launch_options() {
    return { selftest: false };
  },
  async write_selftest_report(a: Record<string, unknown>) {
    if (typeof a.report !== "string") throw { code: "internal", message: "report must be a JSON string" };
    return null;
  },
  async selftest_cleanup() {
    return null;
  },

  async default_output_dir() {
    return ROOT;
  },

  /** Browser: nothing is registered globally. */
  async hotkeys(): Promise<WireHotkeyInfo> {
    return { chords: [{ action: "startStop", chord: BINDINGS.startStop, registered: false }, { action: "pauseResume", chord: BINDINGS.pauseResume, registered: false }] };
  },

  async set_hotkeys(a): Promise<WireHotkeyInfo> {
    const b = a.bindings as { startStop: string; pauseResume: string };
    if (b.startStop.toUpperCase() === b.pauseResume.toUpperCase()) throw fail("internal", "start/stop and pause/resume need different hotkeys");
    BINDINGS.startStop = b.startStop;
    BINDINGS.pauseResume = b.pauseResume;
    return COMMANDS.hotkeys({}) as Promise<WireHotkeyInfo>;
  },

  async start_recording(a): Promise<WireStartResponse> {
    if (active) throw fail("internal", "a recording is already in progress");
    const src = a.source as WireCaptureSource;
    if (src.kind === "window" && !SOURCES.windows.some((w) => w.hwnd === src.hwnd)) throw fail("source_gone", `window ${src.hwnd} no longer exists`);
    recCounter++;
    const stamp = new Date().toISOString().slice(0, 16).replace("T", " ").replace(":", "-");
    const name = `Recording ${stamp}${recCounter > 1 ? ` (${recCounter})` : ""}`;
    const last = performance.now();
    active = {
      dir: `${ROOT}\\${name}.frostcap`,
      name,
      elapsedMs: 0,
      paused: false,
      captureKeys: (a.keys ?? "off") !== "off",
      windowTitles: a.windowTitles === true,
      mic: !!a.mic,
      systemAudio: !!a.systemAudio,
      last,
      warned: false,
      timer: setInterval(() => {
        if (!active) return;
        const now = performance.now();
        if (!active.paused) active.elapsedMs += now - active.last;
        active.last = now;
        emit<WireTick>("recording://tick", { elapsedMs: Math.round(active.elapsedMs), paused: active.paused });
        if (active.systemAudio && !active.warned && active.elapsedMs > 4000) {
          active.warned = true;
          emit<WireIssue>("recording://warning", { code: "warning", message: "system audio has been silent for 4 s; is playback muted or routed to another device?" });
        }
      }, 100),
    };
    return { projectDir: active.dir, canvas: { width: 1920, height: 1080 }, startMs: 120 };
  },

  async pause_recording() {
    if (!active) throw fail("internal", "not recording");
    active.paused = true;
    return true;
  },

  async resume_recording() {
    if (!active) throw fail("internal", "not recording");
    active.paused = false;
    active.last = performance.now();
    return true;
  },

  async stop_recording(): Promise<WireStopResponse> {
    if (!active) throw fail("internal", "not recording");
    const a = active;
    clearInterval(a.timer);
    active = null;
    await delay(400);
    const project = generateMockProject(a.dir, a.elapsedMs, a.captureKeys, a.windowTitles);
    project.name = a.name;
    if (!a.mic) delete project.media.micAudio;
    if (!a.systemAudio) delete project.media.systemAudio;
    saved.set(a.dir, project);
    RECENT.unshift({ path: a.dir, name: a.name, createdAtMs: Date.now(), durationMs: project.durationMs, canvas: project.canvas, status: "complete", interrupted: false });
    return { projectDir: a.dir, durationMs: project.durationMs, warnings: [] };
  },

  async recording_state(): Promise<WireRecordingState> {
    return active
      ? { recording: true, paused: active.paused, elapsedMs: Math.round(active.elapsedMs), projectDir: active.dir }
      : { recording: false, paused: false, elapsedMs: 0, projectDir: null };
  },

  async load_project(a): Promise<WireProject> {
    await delay(120);
    const dir = dirOf(String(a.path));
    if (!isProjectPath(dir)) throw fail("invalid_path", `invalid path: ${dir} is not a .frostcap folder`);
    let p = saved.get(dir);
    if (!p) {
      p = generateMockProject(dir);
      saved.set(dir, p);
    }
    return structuredClone(p);
  },

  async save_project(a) {
    await delay(80);
    const dir = dirOf(String(a.path));
    if (!isProjectPath(dir)) throw fail("invalid_path", `invalid path: ${dir} is not a .frostcap folder`);
    saved.set(dir, structuredClone(a.project) as WireProject);
    return null;
  },

  async list_recent_projects(): Promise<WireRecentProject[]> {
    await delay(100);
    return structuredClone(RECENT);
  },

  async recover_project(a): Promise<WireProject> {
    await delay(600);
    const path = dirOf(String(a.path));
    const r = RECENT.find((x) => x.path === path);
    if (r) {
      r.interrupted = false;
      r.status = "recovered";
    }
    const p = saved.get(path) ?? generateMockProject(path);
    p.status = "recovered";
    saved.set(path, p);
    return structuredClone(p);
  },

  async scrub_project(a): Promise<WireProject> {
    const dir = dirOf(String(a.path));
    const p = scrub(saved.get(dir) ?? generateMockProject(dir), !!a.keys, !!a.titles);
    saved.set(dir, p);
    return structuredClone(p);
  },

  async generate_auto_zoom(a): Promise<WireZoomSegment[]> {
    const p = a.project as WireProject;
    return mockAutoZoom({ canvas: p.canvas, durationMs: p.durationMs, clicks: p.tracks.clicks }, (a.preset as WireZoomPreset) ?? "balanced");
  },

  async export_project(a): Promise<WireExportResult> {
    const outPath = String(a.outPath);
    const path = String(a.path);
    const jobId = typeof a.jobId === "string" && a.jobId ? a.jobId : `job-${++jobSeq}`;
    const preset = a.preset as WireExportPreset;
    exportCancel.delete(jobId);
    const steps = 45;
    const stages = ["decoding", "rendering", "encoding", "muxing"];
    for (let i = 0; i <= steps; i++) {
      if (exportCancel.has(jobId)) {
        exportCancel.delete(jobId);
        throw fail("internal", "export cancelled");
      }
      await delay(a.__fast ? 0 : 70);
      emit<WireExportProgress>("export://progress", { jobId, path, outPath, progress: i / steps, stage: i === steps ? "done" : stages[Math.min(3, Math.floor((i / steps) * 4))] });
    }
    return { jobId, outPath, bytes: 18_400_000, baked: true, preset: typeof preset === "string" ? preset : "raw" };
  },

  async cancel_export(a) {
    exportCancel.add(String(a.jobId));
    return true;
  },

  async pick_project() {
    await delay(200);
    return RECENT[RECENT.length - 1].path;
  },
  async pick_folder() {
    await delay(200);
    return `${ROOT}\\Exports`;
  },
  async pick_save_path(a) {
    await delay(200);
    return `${ROOT}\\${String(a.defaultName)}.${String(a.ext)}`;
  },
  async reveal_in_folder() {
    await delay(50);
    return null;
  },
};

let jobSeq = 0;
const BINDINGS = { startStop: "Ctrl+Alt+Shift+R", pauseResume: "Ctrl+Alt+Shift+P" };
const exportCancel = new Set<string>();

export async function mockInvoke<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  const fn = COMMANDS[cmd];
  if (!fn) throw `Command ${cmd} not found`;
  return (await fn(args)) as T;
}
