import { useEffect, useRef, useState, type MouseEvent } from "react";
import { useEditor } from "../../store/editor";
import { canvasToContent, renderFrame } from "../../lib/render";
import type { AspectPreset } from "../../types";
import type { MediaEngine } from "../../lib/media";
import { isTauri } from "../../backend";

const RATIO: Record<AspectPreset, number> = { "16:9": 16 / 9, "9:16": 9 / 16, "1:1": 1 };

/** Opaque preview canvas. Re-renders on any editor state change (rAF-throttled). */
export function Preview({ aspect, engine }: { aspect: AspectPreset; engine: MediaEngine | null }) {
  const wrap = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [size, setSize] = useState({ w: 640, h: 360 });
  const [mediaError, setMediaError] = useState<string | null>(null);

  useEffect(() => {
    const el = wrap.current!;
    const ro = new ResizeObserver(() => {
      const r = RATIO[aspect];
      const pw = el.clientWidth - 24;
      const ph = el.clientHeight - 24;
      let w = pw;
      let h = w / r;
      if (h > ph) {
        h = ph;
        w = h * r;
      }
      setSize({ w: Math.max(80, Math.floor(w)), h: Math.max(45, Math.floor(h)) });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [aspect]);

  useEffect(() => {
    const c = canvas.current!;
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    c.width = Math.round(size.w * dpr);
    c.height = Math.round(size.h * dpr);
    const ctx = c.getContext("2d", { alpha: false })!;
    let raf = 0;
    const draw = () => {
      raf = 0;
      const { project, playhead, selection } = useEditor.getState();
      if (!project) return;
      renderFrame(ctx, c.width, c.height, project, playhead, {
        showFocus: selection?.kind === "zoom",
        frame: engine?.frame() ?? null,
        webcamFrame: engine?.webcamFrame() ?? null,
      });
    };
    const schedule = () => {
      if (!raf) raf = requestAnimationFrame(draw);
    };
    draw();
    const unsub = useEditor.subscribe(schedule);
    const unFrame = engine?.onFrame(() => {
      schedule();
      if (engine.error) setMediaError(engine.error);
    });
    return () => {
      unsub();
      unFrame?.();
      if (raf) cancelAnimationFrame(raf);
    };
  }, [size, engine]);

  /** With a zoom segment selected, clicking the preview sets its focus point. */
  const onClick = (e: MouseEvent<HTMLCanvasElement>) => {
    const st = useEditor.getState();
    if (!st.project || st.selection?.kind !== "zoom") return;
    const rect = e.currentTarget.getBoundingClientRect();
    const { x: cx, y: cy } = canvasToContent(e.clientX - rect.left, e.clientY - rect.top, rect.width, rect.height, st.project, st.playhead);
    st.updateZoom(st.selection.id, { cx, cy }, { coalesce: undefined });
  };

  const sel = useEditor((s) => s.selection);

  return (
    <div className="preview" ref={wrap}>
      <canvas
        ref={canvas}
        style={{ width: size.w, height: size.h, cursor: sel?.kind === "zoom" ? "crosshair" : "default" }}
        role="img"
        aria-label={sel?.kind === "zoom" ? "Preview. Click to set the zoom focus point." : "Video preview"}
        onClick={onClick}
      />
      {!isTauri && <span className="preview__badge">Demo scene · no video in the browser</span>}
      {mediaError && (
        <span className="preview__badge preview__badge--error" role="alert">
          {mediaError}. Showing the demo scene. Check that the project folder is inside the allowed asset scope.
        </span>
      )}
    </div>
  );
}
