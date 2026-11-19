import { create } from "zustand";
import { backend, isTauri, type Unlisten } from "../backend";
import type { CaptureSource } from "../types";
import { useApp } from "./app";
import { openProjectGuarded } from "./guard";
import { t } from "../i18n";

type Phase = "idle" | "countdown" | "recording" | "paused" | "stopping";

interface RecState {
  phase: Phase;
  countdown: number;
  elapsedMs: number;
  /** Key capture is active for the running recording (drives the on-screen indicator). */
  capturingKeys: boolean;
  pauseSupported: boolean;
  /** `forceCountdown`: always show the visible 3-2-1 (used for hotkey starts). */
  start: (source: CaptureSource, opts?: { forceCountdown?: boolean }) => Promise<void>;
  cancelCountdown: () => void;
  pause: () => Promise<void>;
  resume: () => Promise<void>;
  stop: () => Promise<void>;
  /** Source the recorder view has selected (used by the global Start hotkey). */
  source: CaptureSource | null;
  setSource: (s: CaptureSource | null) => void;
  /** Listen to hotkey://pressed for the app's lifetime; returns the unlisten function. */
  listenHotkeys: () => Promise<Unlisten>;
  /** Re-attach to a recording that is still running in the backend (e.g. after a UI reload). */
  syncWithBackend: () => Promise<void>;
}

let unlisteners: Unlisten[] = [];

/** Is the main window visible and not minimized? (Tauri window state; document visibility in the browser.) */
let windowVisible = async (): Promise<boolean> => {
  if (isTauri) {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const w = getCurrentWindow();
    return (await w.isVisible()) && !(await w.isMinimized());
  }
  return typeof document !== "undefined" && document.visibilityState === "visible";
};
/** Tests: replace the visibility probe. */
export function __setWindowVisibleProbe(fn: () => Promise<boolean>) {
  windowVisible = fn;
}
const primarySource = (): CaptureSource => ({ id: "primary", kind: "display", name: t("recorder.primaryDisplay"), width: 0, height: 0, wire: { kind: "primaryMonitor" } });
let cdTimer: ReturnType<typeof setInterval> | null = null;

async function attachListeners(set: (p: Partial<RecState>) => void) {
  detach();
  const app = useApp.getState();
  unlisteners = await Promise.all([
    backend.onRecordingTick((elapsedMs, paused) => {
      const ph = useRecording.getState().phase;
      if (ph === "recording" || ph === "paused") set({ elapsedMs, phase: paused ? "paused" : "recording" });
      else set({ elapsedMs });
    }),
    backend.onRecordingWarning((i) => app.reportIssue(i, t("rec.warning"), "warning")),
    // Fatal errors arrive as recording://error followed by recording://stopped; the latter decides the outcome.
    backend.onRecordingError((i) => app.reportIssue(i, t("rec.problem"), "error")),
    backend.onRecordingStopped(({ result, error }) => {
      // A user stop is handled by stop(); this covers auto-stops (fatal error, source closed, hotkey).
      if (useRecording.getState().phase === "stopping") return;
      detach();
      set({ phase: "idle", elapsedMs: 0, capturingKeys: false });
      if (error) app.reportIssue(error, t("rec.stopped"), "error");
      if (result) {
        void openProjectGuarded(result.projectPath);
        if (!error) app.showToast(t("rec.saved"));
      }
    }),
  ]);
}

function detach() {
  unlisteners.forEach((u) => u());
  unlisteners = [];
}

export const useRecording = create<RecState>((set, get) => ({
  phase: "idle",
  countdown: 0,
  elapsedMs: 0,
  capturingKeys: false,
  pauseSupported: true,
  source: null,
  setSource: (source) => set({ source }),

  async listenHotkeys() {
    return backend.onHotkey((action, handled) => {
      // The backend handles Stop and Pause/Resume itself while recording (handled: true).
      // While idle it reports Start/Stop as unhandled: start the selected source (or the primary display).
      if (handled || action !== "startStop" || get().phase !== "idle") return;
      // Security: never start silently. Only with the main window visible, and always via the countdown.
      void windowVisible().then((ok) => {
        if (ok && get().phase === "idle") void get().start(get().source ?? primarySource(), { forceCountdown: true });
      });
    });
  },

  async start(source, opts) {
    if (get().phase !== "idle") return;
    const s = useApp.getState().settings;
    const begin = async () => {
      try {
        await attachListeners(set);
        await backend.startRecording({
          source,
          fps: s.fps,
          // "Smooth cursor" means: do not bake the system cursor into the video.
          cursor: !s.cursor,
          mic: s.mic,
          systemAudio: s.systemAudio,
          keys: s.keyCapture,
          camera: s.webcam ? s.camera : null,
          windowTitles: s.captureTitles,
        });
        set({ phase: "recording", elapsedMs: 0, capturingKeys: s.keyCapture !== "off" });
      } catch (e) {
        detach();
        set({ phase: "idle" });
        useApp.getState().reportIssue(e, t("rec.startFailed"));
      }
    };
    if (!s.countdown && !opts?.forceCountdown) return begin();
    set({ phase: "countdown", countdown: 3 });
    cdTimer = setInterval(() => {
      const n = get().countdown - 1;
      if (n <= 0) {
        clearInterval(cdTimer!);
        cdTimer = null;
        set({ countdown: 0 });
        void begin();
      } else set({ countdown: n });
    }, 1000);
  },

  cancelCountdown() {
    if (cdTimer) clearInterval(cdTimer);
    cdTimer = null;
    set({ phase: "idle", countdown: 0 });
  },

  async pause() {
    try {
      await backend.pauseRecording();
      set({ phase: "paused" });
    } catch (e) {
      set({ pauseSupported: false });
      useApp.getState().reportIssue(e, t("rec.pauseFailed"), "warning");
    }
  },

  async resume() {
    try {
      await backend.resumeRecording();
      set({ phase: "recording" });
    } catch (e) {
      useApp.getState().reportIssue(e, t("rec.resumeFailed"));
    }
  },

  async stop() {
    const ph = get().phase;
    if (ph !== "recording" && ph !== "paused") return;
    set({ phase: "stopping" });
    try {
      const res = await backend.stopRecording();
      await openProjectGuarded(res.projectPath);
      if (res.warnings.length) useApp.getState().showToast(t("rec.savedWarnings", { warnings: res.warnings.join("; ") }), "warning");
      else useApp.getState().showToast(t("rec.saved"));
    } catch (e) {
      useApp.getState().reportIssue(e, t("rec.stopFailed"));
    } finally {
      detach();
      set({ phase: "idle", elapsedMs: 0, capturingKeys: false });
    }
  },

  async syncWithBackend() {
    try {
      const st = await backend.recordingState();
      if (st.status === "idle" || get().phase !== "idle") return;
      await attachListeners(set);
      set({ phase: st.status, elapsedMs: st.elapsedMs, capturingKeys: useApp.getState().settings.keyCapture !== "off" });
    } catch {
      /* backend unavailable */
    }
  },
}));
