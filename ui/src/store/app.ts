import { create } from "zustand";
import { describeIssue, toBackendError } from "../backend";
import type { Issue } from "../types";
import type { WireKeyCapture } from "../contract";

export type View = "recorder" | "editor" | "settings";
export type ThemePref = "system" | "dark" | "light";
export type TransparencyPref = "auto" | "glass" | "solid";

export interface Settings {
  theme: ThemePref;
  transparency: TransparencyPref;
  outputFolder: string;
  fps: number;
  webcam: boolean;
  /** Camera id from list_cameras, null = default. */
  camera: string | null;
  mic: boolean;
  systemAudio: boolean;
  /**
   * Smooth cursor: hide the system cursor in screen.mp4 (`cursor: false` to the
   * backend) and let the editor redraw it from the cursor track.
   */
  cursor: boolean;
  /** Key capture for the keystroke overlay (Rust KeyCapture). Privacy-sensitive: default off. */
  keyCapture: WireKeyCapture;
  /** Keep window titles in recordings (privacy: default off; titles are scrubbed after recording). */
  captureTitles: boolean;
  countdown: boolean;
}

const DEFAULTS: Settings = {
  theme: "system",
  transparency: "auto",
  /** Empty = the backend default (default_output_dir). */
  outputFolder: "",
  fps: 60,
  webcam: false,
  camera: null,
  mic: true,
  systemAudio: true,
  cursor: true,
  keyCapture: "off",
  captureTitles: false,
  countdown: true,
};

const KEY = "frostcap.settings.v1";

/** Older builds stored an unexpanded %USERPROFILE% default and a boolean `captureKeys`. */
function migrate(saved: Record<string, unknown>): Settings {
  const { captureKeys: _old, ...rest } = saved;
  void _old;
  const s = { ...DEFAULTS, ...rest } as Settings;
  if (typeof s.outputFolder !== "string" || s.outputFolder.includes("%")) s.outputFolder = "";
  return s;
}

function load(): Settings {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) return migrate(JSON.parse(raw));
  } catch {
    /* storage unavailable */
  }
  return DEFAULTS;
}

export type ToastKind = "info" | "warning" | "error";
export interface Toast {
  id: number;
  kind: ToastKind;
  text: string;
  action?: { label: string; run: () => void };
}

interface AppState {
  view: View;
  projectPath: string | null;
  settings: Settings;
  setView: (v: View) => void;
  openProject: (path: string) => void;
  setSetting: <K extends keyof Settings>(k: K, v: Settings[K]) => void;
  /** Visible toasts, oldest first (max 3; errors are kept over others). */
  toasts: Toast[];
  showToast: (text: string, kind?: ToastKind, action?: Toast["action"]) => number;
  dismissToast: (id: number) => void;
  /** Hover/focus pauses auto-dismiss. */
  pauseToast: (id: number, paused: boolean) => void;
  /** Toast for a backend error or issue, with actionable text per code. */
  reportIssue: (e: unknown, prefix?: string, kind?: ToastKind) => void;
}

const MAX_TOASTS = 3;
/** Auto-dismiss times; errors stay until dismissed. */
export const TOAST_MS: Record<ToastKind, number | null> = { info: 5000, warning: 10_000, error: null };
const ACTION_MIN_MS = 10_000;

let toastSeq = 0;
const timers = new Map<number, { handle: ReturnType<typeof setTimeout> | null; remaining: number; started: number }>();

export const useApp = create<AppState>((set, get) => {
  const arm = (id: number) => {
    const t = timers.get(id);
    if (!t) return;
    t.started = Date.now();
    t.handle = setTimeout(() => get().dismissToast(id), t.remaining);
  };
  return {
    view: "recorder",
    projectPath: null,
    settings: load(),
    setView: (view) => set({ view }),
    openProject: (projectPath) => set({ projectPath, view: "editor" }),
    setSetting: (k, v) => {
      const settings = { ...get().settings, [k]: v };
      set({ settings });
      try {
        localStorage.setItem(KEY, JSON.stringify(settings));
      } catch {
        /* ignore */
      }
    },
    toasts: [],
    showToast: (text, kind = "info", action) => {
      const id = ++toastSeq;
      let list = [...get().toasts, { id, kind, text, action }];
      while (list.length > MAX_TOASTS) {
        const drop = list.find((t) => t.kind !== "error") ?? list[0];
        list = list.filter((t) => t !== drop);
        const tm = timers.get(drop.id);
        if (tm?.handle) clearTimeout(tm.handle);
        timers.delete(drop.id);
      }
      set({ toasts: list });
      const base = TOAST_MS[kind];
      if (base !== null) {
        timers.set(id, { handle: null, remaining: action ? Math.max(base, ACTION_MIN_MS) : base, started: 0 });
        arm(id);
      }
      return id;
    },
    dismissToast: (id) => {
      const tm = timers.get(id);
      if (tm?.handle) clearTimeout(tm.handle);
      timers.delete(id);
      set({ toasts: get().toasts.filter((t) => t.id !== id) });
    },
    pauseToast: (id, paused) => {
      const tm = timers.get(id);
      if (!tm) return;
      if (paused && tm.handle) {
        clearTimeout(tm.handle);
        tm.handle = null;
        tm.remaining = Math.max(1000, tm.remaining - (Date.now() - tm.started));
      } else if (!paused && !tm.handle) arm(id);
    },
    reportIssue: (e, prefix, kind = "error") => {
      const issue: Issue = e && typeof e === "object" && "code" in e && !(e instanceof Error) ? (e as Issue) : toBackendError(e);
      const text = describeIssue(issue);
      get().showToast(prefix ? `${prefix}: ${text}` : text, kind);
    },
  };
});
