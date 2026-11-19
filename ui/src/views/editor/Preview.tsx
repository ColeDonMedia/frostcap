import { useEffect, useRef, useState, type MouseEvent, type PointerEvent as RPointerEvent } from "react";
import { useEditor } from "../../store/editor";
import { useApp } from "../../store/app";
import { canvasToContent, renderFrame } from "../../lib/render";
import { applyDrag, hitTest, visibleAnnotations, type Handle } from "../../lib/annotations";
import { rgbToHex } from "../../lib/chroma";
import { sampleWebcam, webcamRect } from "../../lib/overlay";
import { t } from "../../i18n";
import type { Annotation, AnnotationKind } from "../../types";
import type { AspectPreset } from "../../types";
import type { MediaEngine } from "../../lib/media";
import { backend, isTauri, joinPath } from "../../backend";
import { AnnotationLayer, type Ghost } from "./AnnotationLayer";

const RATIO: Record<AspectPreset, number> = { "16:9": 16 / 9, "9:16": 9 / 16, "1:1": 1 };
/** Pick radius for handles and shapes, in content units. */
const PICK_TOL = 0.015;

type Images = { background: HTMLImageElement | null; webcamBackground: HTMLImageElement | null };

/** Loads the project images (framed-screen background, webcam background replacement) through the asset protocol. */
function useProjectImages(dir: string | undefined, bgFile: string | undefined, camFile: string | undefined): Images {
  const [imgs, setImgs] = useState<Images>({ background: null, webcamBackground: null });
  useEffect(() => {
    let alive = true;
    const load = async (file: string | undefined) => {
      if (!file || !dir || !isTauri) return null;
      try {
        const url = await backend.assetUrl(joinPath(dir, file));
        if (!url) return null;
        const img = new Image();
        img.crossOrigin = "anonymous";
        img.src = url;
        await img.decode();
        return img;
      } catch {
        return null;
      }
    };
    void Promise.all([load(bgFile), load(camFile)]).then(([background, webcamBackground]) => {
      if (alive) {
        setImgs({ background, webcamBackground });
        useEditor.setState({}); // repaint
      }
    });
    return () => {
      alive = false;
    };
  }, [dir, bgFile, camFile]);
  return imgs;
}

type Drag =
  | { mode: "create"; kind: AnnotationKind; p1: { x: number; y: number } }
  | { mode: "edit"; id: string; handle: Handle; last: { x: number; y: number } };

/** Opaque preview canvas. Re-renders on any editor state change (rAF-throttled). */
export function Preview({ aspect, engine }: { aspect: AspectPreset; engine: MediaEngine | null }) {
  const wrap = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [size, setSize] = useState({ w: 640, h: 360 });
  const [mediaError, setMediaError] = useState<string | null>(null);
  const [ghost, setGhost] = useState<Ghost | null>(null);
  const drag = useRef<Drag | null>(null);
  const suppressClick = useRef(false);
  const project = useEditor((s) => s.project);
  const images = useProjectImages(project?.media.dir, project?.style.backgroundImage?.file, project?.style.webcam.background.image);
  const imagesRef = useRef(images);
  imagesRef.current = images;

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
        images: { background: imagesRef.current.background, webcamBackground: imagesRef.current.webcamBackground },
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

  // Eyedropper: Esc cancels.
  const eyedropper = useEditor((s) => s.eyedropper);
  useEffect(() => {
    if (!eyedropper) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        useEditor.getState().setEyedropper(false);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [eyedropper]);

  const point = (e: { clientX: number; clientY: number }, el: HTMLElement) => {
    const rect = el.getBoundingClientRect();
    const st = useEditor.getState();
    return { rect, st, ...canvasToContent(e.clientX - rect.left, e.clientY - rect.top, rect.width, rect.height, st.project!, st.playhead) };
  };

  const sampleEyedropper = (e: RPointerEvent<HTMLCanvasElement>) => {
    const { rect, st } = point(e, e.currentTarget);
    const wc = st.project!.style.webcam;
    const r = webcamRect(rect.width, rect.height, wc);
    const px = e.clientX - rect.left;
    const py = e.clientY - rect.top;
    const inside = px >= r.x && px <= r.x + r.size && py >= r.y && py <= r.y + r.size;
    let rgb: [number, number, number] | null = null;
    const cam = engine?.webcamFrame();
    if (inside && cam) rgb = sampleWebcam(cam, (px - r.x) / r.size, (py - r.y) / r.size, wc.mirror);
    else if (inside) {
      // Demo scene (no camera frame): read the pixel the preview shows.
      const c = canvas.current!;
      const d = c.getContext("2d")!.getImageData(Math.round((px / rect.width) * c.width), Math.round((py / rect.height) * c.height), 1, 1).data;
      rgb = [d[0], d[1], d[2]];
    }
    if (!rgb) {
      useApp.getState().showToast(t("webcam.eyedropperFail"), "warning");
      return;
    }
    st.setStyle("webcam", { chromaKey: { ...wc.chromaKey, color: rgbToHex(...rgb) } }, "webcam.key.color");
    st.setEyedropper(false);
  };

  const onPointerDown = (e: RPointerEvent<HTMLCanvasElement>) => {
    const st0 = useEditor.getState();
    if (!st0.project || e.button !== 0) return;
    if (st0.eyedropper) {
      suppressClick.current = true;
      sampleEyedropper(e);
      return;
    }
    const { st, x, y } = point(e, e.currentTarget);
    if (st.tool) {
      e.currentTarget.setPointerCapture?.(e.pointerId);
      suppressClick.current = true;
      drag.current = { mode: "create", kind: st.tool, p1: { x, y } };
      setGhost({ kind: st.tool, p1: { x, y }, p2: { x, y } });
      return;
    }
    // Selected annotation first (its handles), then the topmost visible one under the pointer.
    const vis = visibleAnnotations(st.project!.annotations, st.playhead);
    const sel = st.selection?.kind === "annotation" ? vis.find((a) => a.id === st.selection!.id) : undefined;
    const order: Annotation[] = [...(sel ? [sel] : []), ...vis.filter((a) => a !== sel).reverse()];
    for (const a of order) {
      const h = hitTest(a, x, y, PICK_TOL);
      if (!h) continue;
      e.currentTarget.setPointerCapture?.(e.pointerId);
      suppressClick.current = true;
      st.select({ kind: "annotation", id: a.id });
      st.setPlaying(false);
      st.beginGesture();
      drag.current = { mode: "edit", id: a.id, handle: h, last: { x, y } };
      return;
    }
  };

  const onPointerMove = (e: RPointerEvent<HTMLCanvasElement>) => {
    const d = drag.current;
    if (!d) return;
    const { st, x, y } = point(e, e.currentTarget);
    if (d.mode === "create") {
      setGhost({ kind: d.kind, p1: d.p1, p2: { x, y } });
      return;
    }
    const a = st.project!.annotations.find((q) => q.id === d.id);
    if (!a) return;
    const n = applyDrag(a, d.handle, x - d.last.x, y - d.last.y);
    d.last = { x, y };
    st.updateAnnotation(a.id, { x: n.x, y: n.y, w: n.w, h: n.h, x2: n.x2, y2: n.y2 }, { history: false });
  };

  const onPointerUp = (e: RPointerEvent<HTMLCanvasElement>) => {
    const d = drag.current;
    drag.current = null;
    if (!d) return;
    const st = useEditor.getState();
    if (d.mode === "create") {
      const { x, y } = point(e, e.currentTarget);
      setGhost(null);
      st.addAnnotation(d.kind, d.p1, { x, y });
    } else {
      st.endGesture();
    }
    queueMicrotask(() => (suppressClick.current = false));
  };

  /** With a zoom segment selected, clicking the preview sets its focus point. */
  const onClick = (e: MouseEvent<HTMLCanvasElement>) => {
    if (suppressClick.current) return;
    const st = useEditor.getState();
    if (!st.project || st.selection?.kind !== "zoom") return;
    const rect = e.currentTarget.getBoundingClientRect();
    const { x: cx, y: cy } = canvasToContent(e.clientX - rect.left, e.clientY - rect.top, rect.width, rect.height, st.project, st.playhead);
    st.updateZoom(st.selection.id, { cx, cy }, { coalesce: undefined });
  };

  const onDoubleClick = (e: MouseEvent<HTMLCanvasElement>) => {
    const { st, x, y } = point(e, e.currentTarget);
    const a = visibleAnnotations(st.project!.annotations, st.playhead)
      .reverse()
      .find((q) => q.kind === "text" && hitTest(q, x, y, PICK_TOL));
    if (a) {
      st.select({ kind: "annotation", id: a.id });
      st.setTextEdit(a.id);
    }
  };

  const sel = useEditor((s) => s.selection);
  const tool = useEditor((s) => s.tool);

  return (
    <div className="preview" ref={wrap}>
      <div style={{ position: "relative", width: size.w, height: size.h }}>
        <canvas
          ref={canvas}
          style={{ width: size.w, height: size.h, cursor: eyedropper ? "crosshair" : tool || sel?.kind === "zoom" ? "crosshair" : "default", touchAction: "none" }}
          role="img"
          aria-label={sel?.kind === "zoom" ? t("preview.zoomFocus") : t("preview.video")}
          onClick={onClick}
          onDoubleClick={onDoubleClick}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
        />
        <AnnotationLayer w={size.w} h={size.h} ghost={ghost} />
      </div>
      {!isTauri && <span className="preview__badge">{t("preview.demo")}</span>}
      {mediaError && (
        <span className="preview__badge preview__badge--error" role="alert">
          {t("preview.mediaError", { error: mediaError })}
        </span>
      )}
    </div>
  );
}
