/**
 * Real media for the editor preview: screen.mp4 (and webcam.mp4) in hidden
 * <video> elements, mic/system WAVs in <audio> elements routed through Web
 * Audio gain nodes, kept in sync with the project's audio offsets.
 *
 * The video element is the master clock while playing. Audio tracks follow it:
 * a track with offsetMs = o plays sample position (t - o) at video time t
 * (o may be negative). In the browser demo there are no URLs and this engine
 * reports `hasVideo = false`; the editor then uses its synthetic scene.
 */
import type { AudioTrackInfo, ProjectMedia } from "../types";
import type { Peaks } from "./wav";
import { t } from "../i18n";

interface Track {
  kind: "mic" | "system";
  info: AudioTrackInfo;
  el: HTMLAudioElement;
  gain: GainNode | null;
  startTimer: ReturnType<typeof setTimeout> | null;
}

const DRIFT_S = 0.06;

export class MediaEngine {
  readonly video: HTMLVideoElement | null;
  readonly webcam: HTMLVideoElement | null;
  private tracks: Track[] = [];
  private ctx: AudioContext | null = null;
  private playing = false;
  private frameListeners = new Set<() => void>();
  private rvfcHandle = 0;
  /** Set when a media element fails (e.g. asset protocol scope / CSP). */
  error: string | null = null;

  constructor(media: ProjectMedia) {
    this.video = media.screen.url ? makeVideo(media.screen.url) : null;
    this.webcam = media.webcam?.url ? makeVideo(media.webcam.url) : null;
    for (const [kind, info] of [["mic", media.mic], ["system", media.systemAudio]] as const) {
      if (!info?.url) continue;
      const el = new Audio();
      el.preload = "auto";
      el.crossOrigin = "anonymous"; // required for Web Audio on asset:// (asset protocol must send CORS headers)
      el.src = info.url;
      this.tracks.push({ kind, info, el, gain: null, startTimer: null });
    }
    const onErr = (what: string) => () => {
      this.error = t("media.loadError", { what });
      this.emitFrame();
    };
    this.video?.addEventListener("error", onErr("screen.mp4"));
    this.video?.addEventListener("loadeddata", () => this.emitFrame());
    this.video?.addEventListener("seeked", () => this.emitFrame());
    this.watchFrames();
  }

  get hasVideo() {
    return !!this.video && !this.error;
  }

  /** Current frame for drawImage, or null if nothing decoded yet. */
  frame(): CanvasImageSource | null {
    const v = this.video;
    return v && v.readyState >= 2 && !this.error ? v : null;
  }

  /** Playback rate for the video, webcam and audio (speed regions); pitch follows speed like the export. */
  setRate(rate: number): void {
    const r = Math.min(4, Math.max(0.25, rate));
    for (const el of [this.video, this.webcam, ...this.tracks.map((t) => t.el)]) {
      if (!el || el.playbackRate === r) continue;
      el.preservesPitch = false; // pitch follows speed, like the export
      (el as HTMLMediaElement & { webkitPreservesPitch?: boolean }).webkitPreservesPitch = false;
      el.playbackRate = r;
    }
  }

  webcamFrame(): CanvasImageSource | null {
    const v = this.webcam;
    return v && v.readyState >= 2 ? v : null;
  }

  timeMs(): number {
    return (this.video?.currentTime ?? 0) * 1000;
  }

  /** Redraw hook: fires for every presented video frame and after seeks. */
  onFrame(cb: () => void): () => void {
    this.frameListeners.add(cb);
    return () => this.frameListeners.delete(cb);
  }

  private emitFrame() {
    this.frameListeners.forEach((f) => f());
  }

  private watchFrames() {
    const v = this.video;
    if (!v) return;
    if ("requestVideoFrameCallback" in v) {
      const loop = () => {
        this.emitFrame();
        this.rvfcHandle = v.requestVideoFrameCallback(loop);
      };
      this.rvfcHandle = v.requestVideoFrameCallback(loop);
    }
  }

  private ensureAudioGraph() {
    if (this.ctx || !this.tracks.length) return;
    try {
      this.ctx = new AudioContext({ latencyHint: "interactive" });
      for (const t of this.tracks) {
        const src = this.ctx.createMediaElementSource(t.el);
        t.gain = this.ctx.createGain();
        src.connect(t.gain).connect(this.ctx.destination);
      }
    } catch {
      this.ctx = null; // fall back to element volume
    }
  }

  setGains(mic: number, system: number) {
    for (const t of this.tracks) {
      const g = t.kind === "mic" ? mic : system;
      if (t.gain) t.gain.gain.value = g;
      else t.el.volume = Math.max(0, Math.min(1, g));
    }
  }

  seek(ms: number) {
    if (this.video) this.video.currentTime = Math.max(0, ms / 1000);
    if (this.webcam) this.webcam.currentTime = Math.max(0, ms / 1000);
    this.syncAudio(ms, true);
  }

  async play(fromMs: number) {
    this.ensureAudioGraph();
    void this.ctx?.resume();
    this.playing = true;
    if (this.video) {
      this.video.currentTime = fromMs / 1000;
      if (this.webcam) this.webcam.currentTime = fromMs / 1000;
      await this.video.play().catch((e) => {
        this.error = String(e);
      });
      void this.webcam?.play().catch(() => undefined);
    }
    this.syncAudio(fromMs, true);
  }

  pause() {
    this.playing = false;
    this.video?.pause();
    this.webcam?.pause();
    for (const t of this.tracks) {
      if (t.startTimer) clearTimeout(t.startTimer);
      t.startTimer = null;
      t.el.pause();
    }
  }

  /** Call each tick while playing (and after seeks) to keep audio aligned with the video clock. */
  syncAudio(videoMs: number, force = false) {
    for (const t of this.tracks) {
      const pos = (videoMs - t.info.offsetMs) / 1000;
      const dur = t.info.durationMs / 1000;
      if (!this.playing) {
        if (pos >= 0 && pos <= dur) t.el.currentTime = pos;
        continue;
      }
      if (pos < 0) {
        // Track starts later on the video clock: schedule it.
        t.el.pause();
        if (!t.startTimer) {
          t.el.currentTime = 0;
          t.startTimer = setTimeout(() => {
            t.startTimer = null;
            if (this.playing) void t.el.play().catch(() => undefined);
          }, -pos * 1000);
        }
        continue;
      }
      if (pos > dur) {
        t.el.pause();
        continue;
      }
      if (force || Math.abs(t.el.currentTime - pos) > DRIFT_S) t.el.currentTime = pos;
      if (t.el.paused) void t.el.play().catch(() => undefined);
    }
  }

  destroy() {
    this.pause();
    if (this.video && this.rvfcHandle && "cancelVideoFrameCallback" in this.video) this.video.cancelVideoFrameCallback(this.rvfcHandle);
    for (const el of [this.video, this.webcam, ...this.tracks.map((t) => t.el)]) {
      if (!el) continue;
      el.removeAttribute("src");
      el.load();
    }
    void this.ctx?.close();
    this.frameListeners.clear();
  }
}

function makeVideo(url: string): HTMLVideoElement {
  const v = document.createElement("video");
  v.muted = true; // audio comes from the WAV tracks
  v.playsInline = true;
  v.preload = "auto";
  v.crossOrigin = "anonymous";
  v.src = url;
  return v;
}

// ------------------------------------------------------------------ peaks

let worker: Worker | null = null;
let seq = 0;
const pending = new Map<number, (r: Peaks | Error) => void>();

/** Waveform peaks for a WAV URL, computed in a worker. */
export function loadPeaks(url: string, bucketMs = 10): Promise<Peaks> {
  if (!worker) {
    worker = new Worker(new URL("./peaks.worker.ts", import.meta.url), { type: "module" });
    worker.onmessage = (e: MessageEvent<{ id: number; bucketMs?: number; peaks?: Float32Array; error?: string }>) => {
      const cb = pending.get(e.data.id);
      pending.delete(e.data.id);
      if (!cb) return;
      cb(e.data.error ? new Error(e.data.error) : { bucketMs: e.data.bucketMs!, peaks: e.data.peaks! });
    };
  }
  const id = ++seq;
  return new Promise((resolve, reject) => {
    pending.set(id, (r) => (r instanceof Error ? reject(r) : resolve(r)));
    worker!.postMessage({ id, url, bucketMs });
  });
}

/** Deterministic stand-in waveform for the browser demo. */
export function demoPeaks(durationMs: number, bucketMs = 10, seed = 7): Peaks {
  const n = Math.max(1, Math.ceil(durationMs / bucketMs));
  const out = new Float32Array(n);
  let s = seed;
  for (let i = 0; i < n; i++) {
    s = (s * 16807) % 2147483647;
    const env = 0.3 + 0.7 * Math.abs(Math.sin(i / 230) * Math.cos(i / 610));
    out[i] = (s / 2147483647) * env;
  }
  return { bucketMs, peaks: out };
}
