import { create } from "zustand";
import type { Annotation, AnnotationKind, Clip, Project, ProjectStyle, SpeedSegment, ZoomSegment, CaptionCue } from "../types";
import { backend, toBackendError } from "../backend";
import { t } from "../i18n";
import { useApp } from "./app";
import { clamp, uid } from "../lib/format";
import { duplicateAnnotation, newAnnotation } from "../lib/annotations";
import { addSpeedSegment, outputLength, outputTime, resizeSpeed } from "../lib/speed";
import { LIMITS } from "../backend";
import { removeRanges, type CutRange } from "../lib/cuts";

export type Selection = { kind: "zoom" | "clip" | "caption" | "annotation" | "speed"; id: string } | null;

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
  /** Active annotation tool (draw on the preview), or null for select/move. */
  tool: AnnotationKind | null;
  /** Webcam chroma-key eyedropper: the next click on the webcam bubble in the preview samples the key colour. */
  eyedropper: boolean;
  /** "Suggest cuts" results: shown on the timeline, never applied until accepted. */
  proposals: CutRange[] | null;
  /** Annotation whose text is being edited inline on the preview. */
  textEdit: string | null;
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
  setTool: (t: AnnotationKind | null) => void;
  setEyedropper: (on: boolean) => void;
  setProposals: (r: CutRange[] | null) => void;
  acceptProposal: (startMs: number) => void;
  rejectProposal: (startMs: number) => void;
  acceptAllProposals: () => void;
  dismissProposals: () => void;
  setTextEdit: (id: string | null) => void;
  setSnap: (s: boolean) => void;
  setPxPerSec: (v: number) => void;

  splitAtPlayhead: () => boolean;
  deleteSelection: () => void;
  addZoomAtPlayhead: () => void;
  updateZoom: (id: string, patch: Partial<ZoomSegment>, opts?: { history?: boolean; coalesce?: string }) => void;
  updateClip: (id: string, patch: Partial<Clip>, opts?: { history?: boolean }) => void;
  setCaptions: (cues: CaptionCue[]) => void;
  /** Adds an annotation at the playhead from a drag between two content points (0..1); null at the limit. */
  addAnnotation: (kind: AnnotationKind, p1: { x: number; y: number }, p2: { x: number; y: number }) => string | null;
  updateAnnotation: (id: string, patch: Partial<Annotation>, opts?: { history?: boolean; coalesce?: string }) => void;
  duplicateSelection: () => void;
  addSpeedAtPlayhead: (speed?: number) => boolean;
  updateSpeed: (id: string, patch: Partial<SpeedSegment>, opts?: { history?: boolean; coalesce?: string }) => void;
  /** Toast with an Undo action for the edit that just happened (no-op if more edits follow). */
  notifyUndoable: (text: string) => void;
}

/** Applies suggested cuts as one history step. When they would remove the whole recording nothing changes and the user is told. */
function applyCutsImpl(ranges: CutRange[]): boolean {
  const st = useEditor.getState();
  const p = st.project;
  if (!p) return false;
  const next = removeRanges(p.clips, ranges);
  if (next === p.clips) {
    useApp.getState().showToast(t("timeline.cutAll"), "warning");
    return false;
  }
  st.apply((q) => ({ ...q, clips: next }));
  return true;
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
  tool: null,
  eyedropper: false,
  proposals: null,
  textEdit: null,
  snap: true,
  pxPerSec: 40,
  past: [],
  future: [],

  async load(path) {
    if (get().path === path && get().project) return;
    set({ proposals: null, loading: true, error: null, failedPath: null, playing: false });
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
  setTool(tool) {
    set({ tool, eyedropper: false });
  },
  setEyedropper(eyedropper) {
    set({ eyedropper, ...(eyedropper ? { tool: null } : {}) });
  },
  setTextEdit(textEdit) {
    set({ textEdit });
  },
  setProposals(proposals) {
    set({ proposals: proposals && proposals.length ? proposals : null });
  },
  acceptProposal(startMs) {
    const applyCuts = applyCutsImpl;
    const { proposals } = get();
    const r = proposals?.find((x) => x.startMs === startMs);
    if (!proposals || !r) return;
    if (!applyCuts([r])) return;
    get().setProposals(proposals.filter((x) => x !== r));
  },
  rejectProposal(startMs) {
    const { proposals } = get();
    if (proposals) get().setProposals(proposals.filter((x) => x.startMs !== startMs));
  },
  acceptAllProposals() {
    const applyCuts = applyCutsImpl;
    const { proposals } = get();
    if (!proposals) return;
    if (!applyCuts(proposals)) return;
    set({ proposals: null });
  },
  dismissProposals() {
    set({ proposals: null });
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
    } else if (selection.kind === "annotation") {
      get().apply((p) => ({ ...p, annotations: p.annotations.filter((a) => a.id !== selection.id) }));
    } else if (selection.kind === "speed") {
      get().apply((p) => ({ ...p, speed: p.speed.filter((s) => s.id !== selection.id) }));
    } else if (selection.kind === "clip") {
      if (project.clips.length <= 1) return; // keep at least one clip
      get().apply((p) => ({ ...p, clips: p.clips.filter((c) => c.id !== selection.id) }));
    } else {
      get().apply((p) => ({ ...p, captions: p.captions.filter((c) => c.id !== selection.id) }));
    }
    set({ selection: null });
    get().notifyUndoable(
      selection.kind === "zoom"
        ? t("editor.zoomDeleted")
        : selection.kind === "clip"
          ? t("editor.clipDeleted")
          : selection.kind === "annotation"
            ? t("editor.annotationDeleted")
            : selection.kind === "speed"
              ? t("editor.speedDeleted")
              : t("editor.captionDeleted"),
    );
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

  addAnnotation(kind, p1, p2) {
    const { project, playhead } = get();
    if (!project || project.annotations.length >= LIMITS.maxAnnotations) return null;
    const start = Math.round(playhead);
    const end = Math.min(project.media.durationMs, start + 3000);
    const a = newAnnotation(kind, uid("ann"), start, Math.max(end, start + 500), p1, p2);
    get().apply((p) => ({ ...p, annotations: [...p.annotations, a] }));
    set({ selection: { kind: "annotation", id: a.id }, tool: null });
    return a.id;
  },

  updateAnnotation(id, patch, opts) {
    get().apply((p) => ({ ...p, annotations: p.annotations.map((a) => (a.id === id ? { ...a, ...patch } : a)) }), opts ?? { coalesce: `ann.${id}.${Object.keys(patch).join(",")}` });
  },

  duplicateSelection() {
    const { selection, project } = get();
    if (!project || selection?.kind !== "annotation" || project.annotations.length >= LIMITS.maxAnnotations) return;
    const src = project.annotations.find((a) => a.id === selection.id);
    if (!src) return;
    const copy = duplicateAnnotation(src, uid("ann"));
    get().apply((p) => ({ ...p, annotations: [...p.annotations, copy] }));
    set({ selection: { kind: "annotation", id: copy.id } });
  },

  addSpeedAtPlayhead(speed = 2) {
    const { project, playhead } = get();
    if (!project) return false;
    const start = Math.max(0, Math.round(playhead));
    const next = addSpeedSegment(project.speed, start, Math.min(project.media.durationMs, start + 2000), speed, project.media.durationMs);
    if (!next) return false;
    const seg = next.find((s) => s.start === start && !project.speed.includes(s));
    if (!seg) return false;
    get().apply((p) => ({ ...p, speed: next }));
    set({ selection: { kind: "speed", id: seg.id } });
    return true;
  },

  updateSpeed(id, patch, opts) {
    get().apply(
      (p) => {
        const { speed: sp, start, end } = patch;
        let list = p.speed;
        if (start !== undefined || end !== undefined) list = resizeSpeed(list, id, { start, end }, p.media.durationMs);
        if (sp !== undefined) list = list.map((s) => (s.id === id ? { ...s, speed: clamp(sp, LIMITS.speed[0], LIMITS.speed[1]) } : s));
        return { ...p, speed: list };
      },
      opts ?? { coalesce: `speed.${id}.${Object.keys(patch).join(",")}` },
    );
  },

  notifyUndoable(text) {
    const after = get().project;
    useApp.getState().showToast(text, "info", {
      label: t("common.undo"),
      run: () => {
        if (get().project === after) get().undo();
      },
    });
  },
}));

function fixSelection() {
  const { selection, project } = useEditor.getState();
  if (!selection || !project) return;
  const list =
    selection.kind === "zoom"
      ? project.zoomSegments
      : selection.kind === "clip"
        ? project.clips
        : selection.kind === "annotation"
          ? project.annotations
          : selection.kind === "speed"
            ? project.speed
            : project.captions;
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
  return outputLength(p.clips, p.speed ?? []);
}

/** Map a source time to output time (time spent in clips before t). */
export function sourceToOutput(p: Project, t: number): number {
  return outputTime(p.clips, p.speed ?? [], t);
}
