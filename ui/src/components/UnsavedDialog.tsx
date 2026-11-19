import { useEffect, useRef, type KeyboardEvent } from "react";
import { AnimatePresence, m } from "motion/react";
import { useGuard } from "../store/guard";
import { useEditor } from "../store/editor";
import { trapTab } from "../lib/focus";
import { softSpring } from "./ui";

/** Save / Discard / Cancel when leaving a project with unsaved edits. */
export function UnsavedDialog() {
  const pending = useGuard((s) => s.pending);
  const saving = useGuard((s) => s.saving);
  const resolve = useGuard((s) => s.resolve);
  const name = useEditor((s) => s.project?.name ?? "this project");
  const saveRef = useRef<HTMLButtonElement>(null);
  const boxRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!pending) return;
    const opener = document.activeElement as HTMLElement | null;
    saveRef.current?.focus();
    return () => opener?.focus?.();
  }, [pending]);

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      void resolve("cancel");
      return;
    }
    if (e.key !== "Tab") return;
    const f = [...(boxRef.current?.querySelectorAll<HTMLButtonElement>("button:not([disabled])") ?? [])];
    const t = trapTab(f.length, f.indexOf(document.activeElement as HTMLButtonElement), e.shiftKey);
    if (t === null) return;
    e.preventDefault();
    if (t >= 0) f[t].focus();
  };

  const verb = pending?.kind === "close" ? "closing" : "opening another project";

  return (
    <AnimatePresence>
      {pending && (
        <m.div className="scrim" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }}>
          <m.div
            ref={boxRef}
            className="sheet sheet--small glass glass--strong"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="unsaved-title"
            aria-describedby="unsaved-desc"
            onKeyDown={onKey}
            initial={{ opacity: 0, y: 16, scale: 0.97 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 16, scale: 0.97 }}
            transition={softSpring}
          >
            <h2 className="sheet__title" id="unsaved-title">
              Unsaved changes
            </h2>
            <p className="page__sub" id="unsaved-desc" style={{ margin: 0 }}>
              “{name}” has unsaved changes. Save them before {verb}?
            </p>
            <div className="sheet__foot">
              <button type="button" className="btn" onClick={() => void resolve("cancel")} disabled={saving}>
                Cancel
              </button>
              <button type="button" className="btn" onClick={() => void resolve("discard")} disabled={saving}>
                Discard
              </button>
              <button ref={saveRef} type="button" className="btn btn--primary" onClick={() => void resolve("save")} disabled={saving}>
                {saving ? "Saving…" : "Save"}
              </button>
            </div>
          </m.div>
        </m.div>
      )}
    </AnimatePresence>
  );
}
