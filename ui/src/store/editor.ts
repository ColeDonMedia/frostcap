import { create } from "zustand";
import type { Clip, Project, ProjectStyle, ZoomSegment, CaptionCue } from "../types";
import { backend, toBackendError } from "../backend";
import { useApp } from "./app";
import { clamp, uid } from "../lib/format";

export type Selection = { kind: "zoom" | "clip" | "caption"; id: string } | null;

const HISTORY_LIMIT = 200;
const COALESCE_MS = 900;

interface EditorState {
  path: string | null;
  project: Project | null;
  loading: boolean;
  error: string | null;
  /** Path whose load failed (for Retry); the previous project stays loaded. */
  failedPath: string | null;
  errorCode: string | null;
  /** Last loaded/saved project; `dirty` = project !== savedProject. */
  savedProject: Project | null;
  dirty: boolean;
  saving: boolean;

  playhead: number;
  playing: boolean;
  selection: Selection;
  snap: boolean;
  /** Timeline zoom: pixels per second. */
  pxPerSec: number;

  past: Project[];
  future: Project[];

  load: (path: string) => Promise<void>;
  retry: () => Promise<void>;
  clearError: () => void;
  save: () => Promise<void>;
  /** Immutable update. `coalesce` merges rapid edits with the same key into one undo step. */
  apply: (fn: (p: Project) => Project, opts?: { history?: boolean; coalesce?: string }) => void;
  beginGesture: () => void;
  endGesture: () => void;
  undo: () => void;
  redo: () => void;

  setStyle: <K extends keyof ProjectStyle>(key: K, patch: Partial<ProjectStyle[K]>, coalesce?: string) => void;
  setPlayhead: (t: number) => void;
  setPlaying: (p: boolean) => void;
  togglePlay: () => void;
  select: (s: Selection) => void;
  setSnap: (s: boolean) => void;
  setPxPerSec: (v: number) => void;

  splitAtPlayhead: () => boolean;
  deleteSelection: () => void;
  addZoomAtPlayhead: () => void;
  updateZoom: (id: string, patch: Partial<ZoomSegment>, opts?: { history?: boolean; coalesce?: string }) => void;
  updateClip: (id: string, patch: Partial<Clip>, opts?: { history?: boolean }) => void;
  setCaptions: (cues: CaptionCue[]) => void;
  /** Toast with an Undo action for the edit that just happened (no-op if more edits follow). */
  notifyUndoable: (text: string) => void;
}

let lastCoalesce: { key: string; at: number } | null = null;
let gestureBase: Project | null = null;

export const useEditor = create<EditorState>((set, get) => ({
  path: null,
  project: null,
  loading: false,
  error: null,
  failedPath: null,
  errorCode: null,
  savedProject: null,
  dirty: false,
  saving: false,
  playhead: 0,
  playing: false,
  selection: null,
  snap: true,
  pxPerSec: 40,
  past: [],
  future: [],

  async load(path) {
    if (get().path === path && get().project) return;
    set({ loading: true, error: null, failedPath: null, playing: false });
    try {
      const project = await backend.loadProject(path);
      set({
        path,
        project,
        savedProject: project,
        loading: false,
        playhead: project.clips[0]?.start ?? 0,
        past: [],
        future: [],
        dirty: false,
        selection: null,
      });
    } catch (e) {
      const be = toBackendError(e);
      set({ loading: false, error: be.message, errorCode: be.code, failedPath: path });
    }
  },

  async retry() {
    const p = get().failedPath;
    if (p) await get().load(p);
  },

  clearError() {
    set({ error: null, errorCode: null, failedPath: null });
  },

  async save() {
    const { path, project } = get();
    if (!path || !project) return;
    set({ saving: true });
    try {
      await backend.saveProject(path, project);
      set({ savedProject: project, dirty: get().project !== project });
    } finally {
      set({ saving: false });
    }
  },

  apply(fn, opts = {}) {
    const { project, past } = get();
    if (!project) return;
    const next = fn(project);
    if (next === project) return;
    const history = opts.history ?? true;
    if (!history || gestureBase) {
      set({ project: next, dirty: next !== get().savedProject });
      return;
    }
    const now = performance.now();
    const merge = opts.coalesce && lastCoalesce && lastCoalesce.key === opts.coalesce && now - lastCoalesce.at < COALESCE_MS;
    lastCoalesce = opts.coalesce ? { key: opts.coalesce, at: now } : null;
    set({
      project: next,
      past: merge ? past : [...past, project].slice(-HISTORY_LIMIT),
      future: [],
      dirty: next !== get().savedProject,
    });
  },

  beginGesture() {
    gestureBase = get().project;
  },

  endGesture() {
    const base = gestureBase;
    gestureBase = null;
    const { project, past } = get();
    if (base && project && base !== project) {
      lastCoalesce = null;
      set({ past: [...past, base].slice(-HISTORY_LIMIT), future: [] });
    }
  },

  undo() {
    const { past, future, project } = get();
    if (!past.length || !project) return;
    lastCoalesce = null;
    const prev = past[past.length - 1];
    set({ project: prev, past: past.slice(0, -1), future: [project, ...future], dirty: prev !== get().savedProject });
    fixSelection();
  },

  redo() {
    const { past, future, project } = get();
    if (!future.length || !project) return;
    lastCoalesce = null;
    const [next, ...rest] = future;
    set({ project: next, past: [...past, project], future: rest, dirty: next !== get().savedProject });
    fixSelection();
  },

  setStyle(key, patch, coalesce) {
    get().apply(
      (p) => ({ ...p, style: { ...p.style, [key]: { ...(p.style[key] as object), ...patch } } }),
      { coalesce: coalesce ?? `style.${String(key)}.${Object.keys(patch).join(",")}` },
    );
  },

  setPlayhead(t) {
    const p = get().project;
    if (!p) return;
    set({ playhead: clamp(t, 0, p.media.durationMs) });
  },
  setPlaying(playing) {
    const p = get().project;
    if (!p) return;
    if (playing) {
      const last = p.clips[p.clips.length - 1];
      if (last && get().playhead >= last.end - 20) set({ playhead: p.clips[0].start });
    }
    set({ playing });
  },
  togglePlay() {
    get().setPlaying(!get().playing);
  },
  select(selection) {
    set({ selection });
  },
  setSnap(snap) {
    set({ snap });
  },
  setPxPerSec(v) {
    set({ pxPerSec: clamp(v, 8, 400) });
  },

  splitAtPlayhead() {
    const { project, playhead } = get();
    if (!project) return false;
    const idx = project.clips.findIndex((c) => playhead > c.start + 50 && playhead < c.end - 50);
    if (idx < 0) return false;
    const c = project.clips[idx];
    const a: Clip = { ...c, end: Math.round(playhead) };
    const b: Clip = { ...c, id: uid("clip"), start: Math.round(playhead) };
    get().apply((p) => ({ ...p, clips: [...p.clips.slice(0, idx), a, b, ...p.clips.slice(idx + 1)] }));
    set({ selection: { kind: "clip", id: b.id } });
    return true;
  },

  deleteSelection() {
    const { selection, project } = get();
    if (!selection || !project) return;
    if (selection.kind === "zoom") {
      get().apply((p) => ({ ...p, zoomSegments: p.zoomSegments.filter((z) => z.id !== selection.id) }));
    } else if (selection.kind === "clip") {
      if (project.clips.length <= 1) return; // keep at least one clip
      get().apply((p) => ({ ...p, clips: p.clips.filter((c) => c.id !== selection.id) }));
    } else {
      get().apply((p) => ({ ...p, captions: p.captions.filter((c) => c.id !== selection.id) }));
    }
    set({ selection: null });
    get().notifyUndoable(selection.kind === "zoom" ? "Zoom segment deleted" : selection.kind === "clip" ? "Clip deleted" : "Caption deleted");
  },

  addZoomAtPlayhead() {
    const { project, playhead } = get();
    if (!project) return;
    const z = project.style.zoom;
    const len = 2500;
    let start = Math.round(playhead);
    let end = Math.min(project.media.durationMs, start + len);
    // avoid overlap: shrink to fit the next segment
    const next = project.zoomSegments.filter((s) => s.start >= start).sort((a, b) => a.start - b.start)[0];
    if (next) end = Math.min(end, next.start);
    if (project.zoomSegments.some((s) => start >= s.start && start < s.end) || end - start < 300) return;
    const cur = cursorAt(project, playhead);
    const seg: ZoomSegment = { id: uid("z"), start, end, cx: cur.x, cy: cur.y, scale: z.scale, easing: z.easing, transitionMs: z.transitionMs, auto: false };
    get().apply((p) => ({ ...p, zoomSegments: [...p.zoomSegments, seg].sort((a, b) => a.start - b.start) }));
    set({ selection: { kind: "zoom", id: seg.id } });
  },

  updateZoom(id, patch, opts) {
    get().apply(
      (p) => ({ ...p, zoomSegments: p.zoomSegments.map((z) => (z.id === id ? { ...z, ...patch } : z)) }),
      opts ?? { coalesce: `zoom.${id}.${Object.keys(patch).join(",")}` },
    );
  },

  updateClip(id, patch, opts) {
    get().apply((p) => ({ ...p, clips: p.clips.map((c) => (c.id === id ? { ...c, ...patch } : c)) }), opts);
  },

  setCaptions(cues) {
    get().apply((p) => ({ ...p, captions: cues }));
  },

  notifyUndoable(text) {
    const after = get().project;
    useApp.getState().showToast(text, "info", {
      label: "Undo",
      run: () => {
        if (get().project === after) get().undo();
      },
    });
  },
}));

function fixSelection() {
  const { selection, project } = useEditor.getState();
  if (!selection || !project) return;
  const list = selection.kind === "zoom" ? project.zoomSegments : selection.kind === "clip" ? project.clips : project.captions;
  if (!list.some((x) => x.id === selection.id)) useEditor.setState({ selection: null });
}

/** Binary-search the cursor sample at time t (linear interpolation). */
export function cursorAt(p: Project, t: number): { x: number; y: number } {
  const s = p.cursor;
  if (!s.length) return { x: 0.5, y: 0.5 };
  let lo = 0;
  let hi = s.length - 1;
  if (t <= s[0].t) return s[0];
  if (t >= s[hi].t) return s[hi];
  while (hi - lo > 1) {
    const mid = (lo + hi) >> 1;
    if (s[mid].t <= t) lo = mid;
    else hi = mid;
  }
  const a = s[lo];
  const b = s[hi];
  const k = (t - a.t) / Math.max(1, b.t - a.t);
  return { x: a.x + (b.x - a.x) * k, y: a.y + (b.y - a.y) * k };
}

/** Total output duration (sum of kept clips). */
export function outputDuration(p: Project): number {
  return p.clips.reduce((s, c) => s + (c.end - c.start), 0);
}

/** Map a source time to output time (time spent in clips before t). */
export function sourceToOutput(p: Project, t: number): number {
  let acc = 0;
  for (const c of p.clips) {
    if (t >= c.end) acc += c.end - c.start;
    else if (t > c.start) return acc + (t - c.start);
    else break;
  }
  return acc;
}
