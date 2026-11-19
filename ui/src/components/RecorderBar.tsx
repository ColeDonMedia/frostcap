import { AnimatePresence, m } from "motion/react";
import { Keyboard, Pause, Play, Square, X } from "lucide-react";
import { useRecording } from "../store/recording";
import { fmtClock } from "../lib/format";
import { softSpring } from "./ui";
import { useEffect, useRef, useState } from "react";
import { backend } from "../backend";
import { useApp } from "../store/app";
import { t } from "../i18n";

/** "Pause (Ctrl+Alt+Shift+P)" when the chord is registered. */
const withChord = (label: string, chord?: string) => (chord ? t("recbar.withChord", { label, chord }) : label);

export function RecorderBar() {
  const phase = useRecording((s) => s.phase);
  const elapsed = useRecording((s) => s.elapsedMs);
  const pause = useRecording((s) => s.pause);
  const resume = useRecording((s) => s.resume);
  const stop = useRecording((s) => s.stop);
  const pauseSupported = useRecording((s) => s.pauseSupported);
  const capturingKeys = useRecording((s) => s.capturingKeys);
  const allKeys = useApp((s) => s.settings.keyCapture === "all");
  const chords = useRegisteredHotkeys();
  const show = phase === "recording" || phase === "paused" || phase === "stopping";
  const paused = phase === "paused";

  return (
    <AnimatePresence>
      {show && (
        <m.div
          className="recbar glass glass--strong"
          role="toolbar"
          aria-label={t("recbar.controls")}
          initial={{ opacity: 0, y: 30, scale: 0.9 }}
          animate={{ opacity: 1, y: 0, scale: 1 }}
          exit={{ opacity: 0, y: 30, scale: 0.9 }}
          transition={softSpring}
        >
          <m.span
            className="recbar__dot"
            aria-hidden="true"
            animate={paused ? { opacity: 0.35 } : { opacity: [1, 0.35, 1] }}
            transition={paused ? { duration: 0.2 } : { duration: 1.4, repeat: Infinity }}
          />
          <span className="recbar__time" role="timer" aria-live="off" aria-label={t("recbar.elapsed", { time: fmtClock(elapsed) })}>
            {fmtClock(elapsed)}
          </span>
          {capturingKeys && (
            <span
              className={`recbar__keys${allKeys ? " recbar__keys--all" : ""}`}
              title={allKeys ? t("recbar.keysAllTitle") : t("recbar.keysShortcutsTitle")}
              role="status"
            >
              <Keyboard size={13} aria-hidden="true" /> {allKeys ? t("recbar.keysAll") : t("recbar.keysOn")}
            </span>
          )}
          <span className="sr-only" aria-live="polite">
            {paused ? t("recbar.statePaused") : phase === "stopping" ? t("recbar.stateSaving") : t("recbar.stateRecording")}
          </span>
          <span className="recbar__sep" aria-hidden="true" />
          {pauseSupported && (
            <button
              type="button"
              className="iconbtn iconbtn--lg"
              onClick={() => (paused ? resume() : pause())}
              disabled={phase === "stopping"}
              aria-label={paused ? t("recbar.resumeLabel") : t("recbar.pauseLabel")}
              title={withChord(paused ? t("recbar.resume") : t("recbar.pause"), chords.pauseResume)}
              aria-keyshortcuts={chords.pauseResume ?? undefined}
            >
              {paused ? <Play size={18} /> : <Pause size={18} />}
            </button>
          )}
          <m.button
            type="button"
            className="recbar__stop"
            onClick={() => stop()}
            disabled={phase === "stopping"}
            aria-label={t("recbar.stopLabel")}
            title={withChord(t("recbar.stop"), chords.startStop)}
            aria-keyshortcuts={chords.startStop ?? undefined}
            whileHover={{ scale: 1.08 }}
            whileTap={{ scale: 0.92 }}
            transition={softSpring}
          >
            {phase === "stopping" ? <span className="spinner" style={{ width: 16, height: 16, borderWidth: 2 }} /> : <Square size={14} fill="currentColor" />}
          </m.button>
        </m.div>
      )}
    </AnimatePresence>
  );
}

/** Global hotkeys the backend actually registered (none in the browser demo). */
function useRegisteredHotkeys() {
  const [chords, setChords] = useState<Partial<Record<"startStop" | "pauseResume", string>>>({});
  useEffect(() => {
    let alive = true;
    backend.hotkeys().then(
      (list) => alive && setChords(Object.fromEntries(list.filter((h) => h.registered).map((h) => [h.action, h.chord]))),
      () => undefined,
    );
    return () => {
      alive = false;
    };
  }, []);
  return chords;
}

export function Countdown() {
  const phase = useRecording((s) => s.phase);
  const n = useRecording((s) => s.countdown);
  const cancel = useRecording((s) => s.cancelCountdown);
  const ref = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (phase !== "countdown") return;
    const opener = document.activeElement as HTMLElement | null;
    ref.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") cancel();
      // Focus trap: Cancel is the only control in this modal.
      if (e.key === "Tab") {
        e.preventDefault();
        ref.current?.focus();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      opener?.focus?.();
    };
  }, [phase, cancel]);

  const active = phase === "countdown";
  return (
    <>
      {/* Persistent live region: mounted before the first number so every update is announced. */}
      <span id="countdown-live" className="sr-only" aria-live="assertive" aria-atomic="true">
        {active && n > 0 ? t("countdown.live", { n }) : ""}
      </span>
      <AnimatePresence>
        {active && (
          <m.div className="countdown" role="alertdialog" aria-modal="true" aria-label={t("countdown.dialog")} initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }}>
            <div style={{ display: "grid", justifyItems: "center", gap: 18 }}>
              <AnimatePresence mode="popLayout">
                <m.div
                  key={n}
                  className="countdown__num glass glass--strong"
                  aria-hidden="true"
                  initial={{ scale: 0.6, opacity: 0 }}
                  animate={{ scale: 1, opacity: 1 }}
                  exit={{ scale: 1.4, opacity: 0 }}
                  transition={softSpring}
                >
                  {n}
                </m.div>
              </AnimatePresence>
              <button ref={ref} type="button" className="btn glass" onClick={cancel}>
                <X size={14} aria-hidden="true" /> {t("countdown.cancel")}
              </button>
            </div>
          </m.div>
        )}
      </AnimatePresence>
    </>
  );
}
