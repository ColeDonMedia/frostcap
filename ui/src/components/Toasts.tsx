import { useEffect, useRef } from "react";
import { AnimatePresence, m } from "motion/react";
import { AlertTriangle, Info, X } from "lucide-react";
import { useApp } from "../store/app";
import { softSpring } from "./ui";
import { t as tr } from "../i18n";

/**
 * Toast stack. Errors use role=alert and persist until dismissed; others
 * auto-dismiss (paused on hover/focus). F6 moves focus into the newest toast
 * (its action if it has one) so keyboard users can reach "Undo" etc.
 */
export function Toasts() {
  const toasts = useApp((s) => s.toasts);
  const dismiss = useApp((s) => s.dismissToast);
  const pause = useApp((s) => s.pauseToast);
  const regionRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "F6" || e.ctrlKey || e.altKey) return;
      const items = regionRef.current?.querySelectorAll<HTMLElement>(".toast");
      const last = items?.[items.length - 1];
      if (!last) return;
      e.preventDefault();
      (last.querySelector<HTMLElement>("[data-toast-action]") ?? last.querySelector<HTMLElement>("button"))?.focus();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="toasts" role="region" aria-label={tr("toasts.region")} ref={regionRef}>
      <AnimatePresence initial={false}>
        {toasts.map((t) => (
          <m.div
            key={t.id}
            layout={false}
            className={`toast toast--${t.kind} glass glass--strong`}
            role={t.kind === "error" ? "alert" : "status"}
            aria-live={t.kind === "error" ? "assertive" : "polite"}
            initial={{ opacity: 0, y: -12, scale: 0.95 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: -12, scale: 0.95 }}
            transition={softSpring}
            onMouseOver={() => pause(t.id, true)}
            onMouseLeave={() => pause(t.id, false)}
            onFocus={() => pause(t.id, true)}
            onBlur={(e) => !e.currentTarget.contains(e.relatedTarget as Node) && pause(t.id, false)}
          >
            {t.kind === "info" ? <Info size={15} aria-hidden="true" /> : <AlertTriangle size={15} aria-hidden="true" />}
            <span className="toast__text">{t.text}</span>
            {t.action && (
              <>
                <span className="sr-only">{tr("toasts.f6", { label: t.action.label })}</span>
                <button
                  type="button"
                  className="btn btn--sm"
                  data-toast-action
                  onClick={() => {
                    t.action!.run();
                    dismiss(t.id);
                  }}
                >
                  {t.action.label}
                </button>
              </>
            )}
            <button type="button" className="iconbtn" style={{ width: 26, height: 26 }} aria-label={tr("common.dismiss")} onClick={() => dismiss(t.id)}>
              <X size={14} />
            </button>
          </m.div>
        ))}
      </AnimatePresence>
    </div>
  );
}
