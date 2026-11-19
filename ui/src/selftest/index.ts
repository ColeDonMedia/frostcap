/**
 * In-app self-test entry. Loaded only from main.tsx behind a build-time guard
 * (`import.meta.env.DEV` or `VITE_FROSTCAP_SELFTEST=1` at build time), so a normal production
 * bundle never contains it. At runtime it additionally needs the backend to say
 * `launch_options() -> { selftest: true }` (the app was started with a self-test request).
 *
 * Backend contract used here: `launch_options`, `write_selftest_report({report})`,
 * `selftest_cleanup({paths})`, plus the normal recording/project/export commands.
 */
import { backend } from "../backend";
import { useEditor } from "../store/editor";
import { buildFlow, type Deps } from "./flow";
import { describeError, runSteps, type Report } from "./runner";

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

function withTimeout<T>(p: Promise<T>, ms: number, what: string): Promise<T> {
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`${what} timed out after ${ms} ms`)), ms);
    p.then(
      (v) => (clearTimeout(t), resolve(v)),
      (e) => (clearTimeout(t), reject(e)),
    );
  });
}

function once(el: EventTarget, ok: string): Promise<void> {
  return new Promise((resolve, reject) => {
    el.addEventListener(ok, () => resolve(), { once: true });
    el.addEventListener("error", () => reject(new Error(`media element error while waiting for ${ok}`)), { once: true });
  });
}

/** Loads the file in a <video> (asset protocol / WebView2), reads its duration and seeks to the middle. */
async function probeVideo(url: string) {
  const v = document.createElement("video");
  v.muted = true;
  v.preload = "auto";
  v.src = url;
  try {
    await withTimeout(once(v, "loadedmetadata"), 20_000, "video metadata");
    const duration = v.duration;
    if (!Number.isFinite(duration) || duration <= 0) throw new Error(`video reports duration ${duration}`);
    const seeked = once(v, "seeked");
    v.currentTime = duration / 2;
    await withTimeout(seeked, 20_000, "video seek");
    return { duration, seekedTo: v.currentTime };
  } finally {
    v.removeAttribute("src");
    v.load();
  }
}

/** fetch + decode the WAV and measure peak / RMS across all channels. */
async function probeAudio(url: string) {
  const res = await withTimeout(fetch(url), 20_000, "audio fetch");
  if (!res.ok) throw new Error(`fetch ${res.status}`);
  const bytes = await res.arrayBuffer();
  const ctx = new OfflineAudioContext(1, 1, 44_100);
  const buf = await ctx.decodeAudioData(bytes);
  let peak = 0;
  let sum = 0;
  let n = 0;
  for (let c = 0; c < buf.numberOfChannels; c++) {
    const data = buf.getChannelData(c);
    for (let i = 0; i < data.length; i++) {
      const a = Math.abs(data[i]);
      if (a > peak) peak = a;
      sum += data[i] * data[i];
    }
    n += data.length;
  }
  return { peak, rms: n ? Math.sqrt(sum / n) : 0 };
}

function makeTone() {
  let ctx: AudioContext | null = null;
  let osc: OscillatorNode | null = null;
  return {
    start() {
      ctx = new AudioContext();
      void ctx.resume();
      osc = ctx.createOscillator();
      osc.type = "sine";
      osc.frequency.value = 440;
      const gain = ctx.createGain();
      gain.gain.value = 0.3;
      osc.connect(gain).connect(ctx.destination);
      osc.start();
    },
    stop() {
      osc?.stop();
      void ctx?.close();
      osc = null;
      ctx = null;
    },
  };
}

function realDeps(): Deps {
  const ed = () => useEditor.getState();
  return {
    backend: {
      listSources: () => backend.listSources(),
      startRecording: (o) => backend.startRecording(o),
      pauseRecording: () => backend.pauseRecording(),
      resumeRecording: () => backend.resumeRecording(),
      stopRecording: () => backend.stopRecording(),
      loadProject: (p) => backend.loadProject(p),
      generateAutoZoom: (p, preset) => backend.generateAutoZoom(p, preset),
      exportProject: (r) => backend.exportProject(r),
      cleanup: (paths) => backend.selftestCleanup(paths),
    },
    editor: {
      load: (p) => ed().load(p),
      setPlayhead: (ms) => ed().setPlayhead(ms),
      addZoomAtPlayhead: () => ed().addZoomAtPlayhead(),
      setCaptions: (c) => ed().setCaptions(c),
      setFrame: (f) => ed().apply((p) => ({ ...p, style: { ...p.style, frame: f } })),
      save: () => ed().save(),
    },
    sleep,
    tone: makeTone(),
    probeVideo,
    probeAudio,
    assetUrl: (p) => backend.assetUrl(p),
  };
}

async function runDefault(): Promise<Report> {
  return runSteps(buildFlow(realDeps()), {}, { env: { userAgent: navigator.userAgent } });
}

/** Returns true when a self-test was requested (and has finished). Never throws. */
export async function maybeRunSelfTest(run: () => Promise<Report> = runDefault): Promise<boolean> {
  const opts = await backend.launchOptions();
  if (!opts.selftest) return false;
  let report: Report;
  try {
    report = await run();
  } catch (e) {
    const at = new Date().toISOString();
    report = { ok: false, startedAt: at, finishedAt: at, steps: [{ name: "runner", ok: false, ms: 0, error: describeError(e) }], env: {} };
  }
  console.log(`[selftest] ${JSON.stringify(report)}`);
  try {
    await backend.writeSelftestReport(report);
  } catch (e) {
    console.warn("[selftest] write_selftest_report failed:", describeError(e));
  }
  return true;
}
