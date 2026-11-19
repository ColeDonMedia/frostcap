/**
 * The scripted self-test flow. Everything that touches the real world (backend commands,
 * the editor store, <video>/Web Audio probes, timers) comes in through `Deps`, so the
 * sequencing and pass/fail rules are unit-tested with fakes and the real wiring lives in index.ts.
 *
 * Export target: the backend accepts `.mp4` targets that are not inside a project folder and are
 * either inside `Videos\Frostcap` (the default output dir) or exactly what `pick_save_path` returned.
 * Projects live in that output dir, so the file goes next to the project: `<parent of project>/frostcap-selftest-*.mp4`.
 */
import type { CaptionCue, CaptureSource, ExportRequest, ExportResult, Project, RecordingResult, StartRecordingOptions, ZoomSegment } from "../types";
import type { Step } from "./runner";

export interface Deps {
  backend: {
    listSources: () => Promise<CaptureSource[]>;
    startRecording: (o: StartRecordingOptions) => Promise<string>;
    pauseRecording: () => Promise<boolean>;
    resumeRecording: () => Promise<boolean>;
    stopRecording: () => Promise<RecordingResult>;
    loadProject: (path: string) => Promise<Project>;
    generateAutoZoom: (p: Project, preset: "gentle" | "balanced" | "snappy") => Promise<ZoomSegment[]>;
    exportProject: (r: ExportRequest) => Promise<ExportResult>;
    /** Deletes the self-test project folder and export file (only paths the self-test created). */
    cleanup: (paths: string[]) => Promise<void>;
  };
  editor: {
    load: (path: string) => Promise<void>;
    setPlayhead: (ms: number) => void;
    addZoomAtPlayhead: () => void;
    setCaptions: (cues: CaptionCue[]) => void;
    setFrame: (f: "macos") => void;
    save: () => Promise<void>;
  };
  sleep: (ms: number) => Promise<void>;
  /** 440 Hz tone so loopback capture is not silent. */
  tone: { start: () => void; stop: () => void };
  probeVideo: (url: string) => Promise<{ duration: number; seekedTo: number }>;
  probeAudio: (url: string) => Promise<{ peak: number; rms: number }>;
  assetUrl: (absPath: string) => Promise<string | null>;
}

export interface Ctx {
  source?: CaptureSource;
  projectPath?: string;
  recordedMs?: number;
  project?: Project;
  exportPath?: string;
}

export const parentDir = (p: string) => p.replace(/[\\/]+$/, "").replace(/[\\/][^\\/]*$/, "");

function need<T>(v: T | undefined | null, what: string): T {
  if (v === undefined || v === null) throw new Error(`${what} is missing (an earlier step failed)`);
  return v;
}

export function buildFlow(d: Deps): Step<Ctx>[] {
  return [
    {
      name: "list_sources",
      async run(c) {
        const sources = await d.backend.listSources();
        const src = sources.find((s) => s.kind === "display" && s.primary) ?? sources.find((s) => s.kind === "display");
        if (!src) throw new Error("no display source listed");
        c.source = src;
        return { sources: sources.length, chosen: src.name };
      },
    },
    {
      name: "record",
      async run(c) {
        const source = need(c.source, "source");
        d.tone.start();
        try {
          c.projectPath = await d.backend.startRecording({ source, fps: 30, cursor: false, mic: false, systemAudio: true, keys: "off", camera: null });
          await d.sleep(1200);
          if (!(await d.backend.pauseRecording())) throw new Error("pause_recording returned false");
          await d.sleep(500);
          if (!(await d.backend.resumeRecording())) throw new Error("resume_recording returned false");
          await d.sleep(1300);
          const res = await d.backend.stopRecording();
          c.projectPath = res.projectPath || c.projectPath;
          c.recordedMs = res.durationMs;
          if (res.durationMs < 1800) throw new Error(`recording is only ${res.durationMs} ms (expected about 2500)`);
          return { projectPath: c.projectPath, durationMs: res.durationMs, warnings: res.warnings };
        } finally {
          d.tone.stop();
        }
      },
    },
    {
      name: "load_project",
      async run(c) {
        const p = await d.backend.loadProject(need(c.projectPath, "projectPath"));
        c.project = p;
        if (!p.media.screen.url) throw new Error("screen.mp4 has no asset URL");
        if (!p.media.systemAudio) throw new Error("project has no system audio track");
        return { name: p.name, durationMs: p.media.durationMs };
      },
    },
    {
      name: "video",
      async run(c) {
        const url = need(need(c.project, "project").media.screen.url, "screen url");
        const v = await d.probeVideo(url);
        const want = need(c.recordedMs, "recordedMs") / 1000;
        if (!(v.duration > 0.5 * want)) throw new Error(`video duration ${v.duration}s is far below the recorded ${want}s`);
        if (Math.abs(v.seekedTo - v.duration / 2) > 0.3) throw new Error(`seek to the middle landed at ${v.seekedTo}s (duration ${v.duration}s)`);
        return { duration: v.duration, seekedTo: v.seekedTo };
      },
    },
    {
      name: "system_audio",
      async run(c) {
        const url = need(need(c.project, "project").media.systemAudio?.url, "system audio url");
        const a = await d.probeAudio(url);
        if (a.peak < 0.01) throw new Error(`system audio is silent (peak ${a.peak.toFixed(4)}): loopback did not capture the 440 Hz tone`);
        return { peak: a.peak, rms: a.rms };
      },
    },
    {
      name: "auto_zoom",
      async run(c) {
        const segs = await d.backend.generateAutoZoom(need(c.project, "project"), "balanced");
        if (!Array.isArray(segs)) throw new Error("generate_auto_zoom did not return a list");
        return { segments: segs.length };
      },
    },
    {
      name: "edit",
      async run(c) {
        const path = need(c.projectPath, "projectPath");
        await d.editor.load(path);
        d.editor.setPlayhead(Math.round(need(c.recordedMs, "recordedMs") / 2));
        d.editor.addZoomAtPlayhead();
        d.editor.setCaptions([{ id: "selftest-cue", start: 200, end: 1200, text: "Hello from the self-test" }]);
        d.editor.setFrame("macos");
        await d.editor.save();
        return { saved: true };
      },
    },
    {
      name: "reload",
      async run(c) {
        const p = await d.backend.loadProject(need(c.projectPath, "projectPath"));
        c.project = p;
        if (p.zoomSegments.length < 1) throw new Error("the added zoom did not survive save/load");
        if (p.captions[0]?.text !== "Hello from the self-test") throw new Error("the caption did not survive save/load");
        if (p.style.frame !== "macos") throw new Error(`frame is ${p.style.frame}, expected macos`);
        return { zooms: p.zoomSegments.length, captions: p.captions.length, frame: p.style.frame };
      },
    },
    {
      name: "export",
      async run(c) {
        const path = need(c.projectPath, "projectPath");
        const sep = path.includes("\\") ? "\\" : "/";
        const out = `${parentDir(path)}${sep}frostcap-selftest-${Date.now().toString(36)}.mp4`;
        c.exportPath = out;
        const res = await d.backend.exportProject({ path, preset: { aspect: "16:9", width: 1280, height: 720, fps: 30, format: "mp4" }, outPath: out, jobId: `selftest-${Date.now().toString(36)}` });
        if (!(res.bytes > 0)) throw new Error("export produced an empty file");
        const url = await d.assetUrl(res.outPath);
        if (!url) throw new Error("no asset URL for the exported file");
        const v = await d.probeVideo(url);
        const want = need(c.project, "project").media.durationMs / 1000;
        if (Math.abs(v.duration - want) > Math.max(0.5, want * 0.15)) throw new Error(`exported duration ${v.duration}s differs from the project's ${want}s`);
        return { outPath: res.outPath, bytes: res.bytes, duration: v.duration };
      },
    },
    {
      name: "cleanup",
      cleanup: true,
      async run(c) {
        const paths = [c.projectPath, c.exportPath].filter((p): p is string => !!p);
        if (paths.length) await d.backend.cleanup(paths);
        return { deleted: paths };
      },
    },
  ];
}
