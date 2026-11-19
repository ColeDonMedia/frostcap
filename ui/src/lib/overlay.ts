/**
 * Preview drawing for annotations and the webcam bubble. Mirrors the export semantics as far as a 2D canvas
 * allows: annotations are drawn in screen-content space (after the zoom, below cursor/webcam/captions),
 * spotlight dims only the screen content, blur/pixelate sample the content frame.
 */
import type { Annotation, WebcamStyle } from "../types";
import { annotationAlpha, visibleAnnotations } from "./annotations";
import { chromaKeyPixels } from "./chroma";
import { parseBackground } from "../backend";
import { wrapCaption } from "./render";

export interface Content {
  x: number;
  y: number;
  w: number;
  h: number;
}

function path(ctx: CanvasRenderingContext2D, kind: "rect" | "ellipse", x: number, y: number, w: number, h: number, r: number) {
  ctx.beginPath();
  if (kind === "ellipse") ctx.ellipse(x + w / 2, y + h / 2, Math.max(0.5, w / 2), Math.max(0.5, h / 2), 0, 0, Math.PI * 2);
  else if (r > 0) ctx.roundRect(x, y, w, h, Math.min(r, w / 2, h / 2));
  else ctx.rect(x, y, w, h);
}

let scratch: HTMLCanvasElement | null = null;
const scratchCanvas = (w: number, h: number) => {
  scratch ??= document.createElement("canvas");
  scratch.width = Math.max(1, Math.round(w));
  scratch.height = Math.max(1, Math.round(h));
  return scratch;
};

const size = (src: CanvasImageSource) => {
  const s = src as { videoWidth?: number; videoHeight?: number; width?: number; height?: number };
  return { w: s.videoWidth || s.width || 1, h: s.videoHeight || s.height || 1 };
};

/**
 * Draws the annotations active at `t` (at most 8). `unit` = output height / 1080; `zoom` = the current zoom
 * scale already applied to `ctx`, divided out so strokes keep their size. Returns how many were drawn.
 */
export function drawAnnotations(ctx: CanvasRenderingContext2D, list: Annotation[], t: number, c: Content, unit: number, zoom: number, src: CanvasImageSource): number {
  const vis = visibleAnnotations(list, t);
  const px = unit / zoom;
  for (const a of vis) {
    const alpha = annotationAlpha(a, t);
    if (alpha <= 0) continue;
    const x = c.x + a.x * c.w;
    const y = c.y + a.y * c.h;
    const w = a.w * c.w;
    const h = a.h * c.h;
    ctx.save();
    ctx.globalAlpha = alpha;
    ctx.strokeStyle = a.color;
    ctx.fillStyle = a.color;
    ctx.lineWidth = a.strokeWidth * px;
    ctx.lineJoin = "round";
    ctx.lineCap = "round";
    switch (a.kind) {
      case "arrow": {
        const x2 = c.x + a.x2 * c.w;
        const y2 = c.y + a.y2 * c.h;
        const ang = Math.atan2(y2 - y, x2 - x);
        const head = Math.max(12, a.strokeWidth * 3.5) * px;
        const bx = x2 - Math.cos(ang) * head * 0.8;
        const by = y2 - Math.sin(ang) * head * 0.8;
        ctx.beginPath();
        ctx.moveTo(x, y);
        ctx.lineTo(bx, by);
        ctx.stroke();
        ctx.beginPath();
        ctx.moveTo(x2, y2);
        ctx.lineTo(x2 - Math.cos(ang - 0.45) * head, y2 - Math.sin(ang - 0.45) * head);
        ctx.lineTo(x2 - Math.cos(ang + 0.45) * head, y2 - Math.sin(ang + 0.45) * head);
        ctx.closePath();
        ctx.fill();
        break;
      }
      case "rect":
      case "ellipse":
        path(ctx, a.kind, x, y, w, h, a.cornerRadius * px);
        if (a.fill) {
          ctx.globalAlpha = alpha * 0.35;
          ctx.fill();
          ctx.globalAlpha = alpha;
        }
        ctx.stroke();
        break;
      case "highlight":
        path(ctx, "rect", x, y, w, h, a.cornerRadius * px);
        ctx.fill();
        break;
      case "spotlight": {
        // Dim everything inside the screen content except the box; nothing outside the content is touched.
        ctx.beginPath();
        ctx.rect(c.x, c.y, c.w, c.h);
        ctx.clip();
        ctx.beginPath();
        ctx.rect(c.x, c.y, c.w, c.h);
        if (a.cornerRadius > 0) ctx.roundRect(x, y, w, h, Math.min(a.cornerRadius * px, w / 2, h / 2));
        else ctx.rect(x, y, w, h);
        ctx.fillStyle = "#000000";
        ctx.globalAlpha = a.dim * (alpha / Math.max(0.0001, a.opacity));
        ctx.fill("evenodd");
        break;
      }
      case "blur":
        ctx.beginPath();
        ctx.rect(x, y, w, h);
        ctx.clip();
        ctx.filter = `blur(${Math.max(0.5, a.strength * 30 * px)}px)`;
        ctx.drawImage(src, c.x, c.y, c.w, c.h);
        break;
      case "pixelate": {
        const block = Math.max(2, a.strength * 40 * px);
        const sw = Math.max(1, Math.round(c.w / block));
        const sh = Math.max(1, Math.round(c.h / block));
        const small = scratchCanvas(sw, sh);
        small.getContext("2d")?.drawImage(src, 0, 0, sw, sh);
        ctx.beginPath();
        ctx.rect(x, y, w, h);
        ctx.clip();
        ctx.imageSmoothingEnabled = false;
        ctx.drawImage(small, c.x, c.y, c.w, c.h);
        break;
      }
      case "text": {
        const fs = a.fontSize * px;
        ctx.font = `600 ${fs}px "Segoe UI Variable", "Segoe UI", system-ui, sans-serif`;
        ctx.textAlign = "center";
        ctx.textBaseline = "middle";
        const lines = wrapCaption((s) => ctx.measureText(s).width, a.text, Math.max(w, fs));
        const lh = fs * 1.25;
        ctx.shadowColor = "rgba(0,0,0,0.6)";
        ctx.shadowBlur = 6 * px;
        lines.forEach((l, i) => ctx.fillText(l, x + w / 2, y + h / 2 - ((lines.length - 1) * lh) / 2 + i * lh));
        break;
      }
    }
    ctx.restore();
  }
  return vis.length;
}

// ------------------------------------------------------------------ webcam

/** Bubble rect (output pixels): `size` is the diameter / output height, `margin` a fraction of the height. */
export function webcamRect(W: number, H: number, w: Pick<WebcamStyle, "size" | "margin" | "position">) {
  const size = w.size * H;
  const m = w.margin * H;
  const left = w.position.endsWith("left");
  const top = w.position.startsWith("top");
  return { x: left ? m : W - m - size, y: top ? m : H - m - size, size };
}

/** Cover-fit the camera frame into an n x n scratch canvas, optionally chroma-keyed. */
function camToScratch(cam: CanvasImageSource, n: number, key?: WebcamStyle["chromaKey"]): HTMLCanvasElement {
  const cv = scratchCanvas(n, n);
  const c2 = cv.getContext("2d", { willReadFrequently: true })!;
  const { w, h } = size(cam);
  const k = Math.max(n / w, n / h);
  c2.clearRect(0, 0, n, n);
  c2.drawImage(cam, (n - w * k) / 2, (n - h * k) / 2, w * k, h * k);
  if (key?.enabled) {
    const img = c2.getImageData(0, 0, n, n);
    chromaKeyPixels(img.data, key);
    c2.putImageData(img, 0, 0);
  }
  return cv;
}

/** Colour of the raw camera frame at (u, v) in 0..1 of the bubble (before mirroring and keying is irrelevant: raw frame). */
export function sampleWebcam(cam: CanvasImageSource, u: number, v: number, mirror: boolean): [number, number, number] | null {
  try {
    const n = 128;
    const cv = camToScratch(cam, n);
    const x = Math.min(n - 1, Math.max(0, Math.floor((mirror ? 1 - u : u) * n)));
    const y = Math.min(n - 1, Math.max(0, Math.floor(v * n)));
    const d = cv.getContext("2d", { willReadFrequently: true })!.getImageData(x, y, 1, 1).data;
    return [d[0], d[1], d[2]];
  } catch {
    return null;
  }
}

export interface WebcamImages {
  /** webcam.background.image, loaded */
  background?: CanvasImageSource | null;
}

function coverDraw(ctx: CanvasRenderingContext2D, img: CanvasImageSource, x: number, y: number, s: number) {
  const { w, h } = size(img);
  const k = Math.max(s / w, s / h);
  ctx.drawImage(img, x + (s - w * k) / 2, y + (s - h * k) / 2, w * k, h * k);
}

/** The webcam bubble with border, shadow, colour adjustments, mirror, chroma key and background replacement. */
export function drawWebcam(ctx: CanvasRenderingContext2D, W: number, H: number, wc: WebcamStyle, cam: CanvasImageSource | null | undefined, images: WebcamImages = {}) {
  const unit = H / 1080;
  const { x: wx, y: wy, size: sz } = webcamRect(W, H, wc);
  const bubble = () => {
    ctx.beginPath();
    if (wc.shape === "circle") ctx.arc(wx + sz / 2, wy + sz / 2, sz / 2, 0, Math.PI * 2);
    else ctx.roundRect(wx, wy, sz, sz, wc.shape === "rounded" ? sz * 0.22 : 6 * unit);
  };
  ctx.save();
  if (wc.shadow > 0) {
    ctx.shadowColor = `rgba(0,0,0,${Math.min(1, wc.shadow)})`;
    ctx.shadowBlur = 24 * unit;
  }
  bubble();
  const wg = ctx.createLinearGradient(wx, wy, wx + sz, wy + sz);
  wg.addColorStop(0, "#3b4a6b");
  wg.addColorStop(1, "#1b2236");
  ctx.fillStyle = wg;
  ctx.fill();
  ctx.shadowColor = "transparent";
  ctx.clip();

  const keyed = wc.chromaKey.enabled && !!cam;
  if (keyed) {
    // What shows where the camera was keyed out.
    const bg = wc.background;
    if (bg.kind === "color") {
      ctx.fillStyle = bg.color;
      ctx.fillRect(wx, wy, sz, sz);
    } else if (bg.kind === "gradient") {
      const g = parseBackground(bg.gradient);
      const ang = (g.angle * Math.PI) / 180;
      const dx = Math.sin(ang);
      const dy = -Math.cos(ang);
      const half = (Math.abs(sz * dx) + Math.abs(sz * dy)) / 2;
      const lg = ctx.createLinearGradient(wx + sz / 2 - dx * half, wy + sz / 2 - dy * half, wx + sz / 2 + dx * half, wy + sz / 2 + dy * half);
      g.stops.forEach((c, i, a) => lg.addColorStop(a.length === 1 ? 0 : i / (a.length - 1), c));
      ctx.fillStyle = lg;
      ctx.fillRect(wx, wy, sz, sz);
    } else if (bg.kind === "image" && images.background) {
      coverDraw(ctx, images.background, wx, wy, sz);
    } else if (bg.kind === "blur" && cam) {
      ctx.filter = `blur(${Math.max(1, bg.blur * 24 * unit)}px) saturate(0.6)`;
      coverDraw(ctx, camToScratch(cam, 96), wx, wy, sz);
      ctx.filter = "none";
    }
  }

  ctx.save();
  if (wc.mirror) {
    ctx.translate(wx + sz / 2, 0);
    ctx.scale(-1, 1);
    ctx.translate(-(wx + sz / 2), 0);
  }
  ctx.filter = `brightness(${1 + wc.brightness}) contrast(${wc.contrast}) saturate(${wc.saturation})`;
  if (cam) {
    ctx.drawImage(camToScratch(cam, Math.min(320, Math.max(64, Math.round(sz))), keyed ? wc.chromaKey : undefined), wx, wy, sz, sz);
  } else {
    ctx.fillStyle = "#8fa3c8";
    ctx.beginPath();
    ctx.arc(wx + sz / 2, wy + sz * 0.42, sz * 0.17, 0, Math.PI * 2);
    ctx.fill();
    ctx.beginPath();
    ctx.ellipse(wx + sz / 2, wy + sz * 0.95, sz * 0.32, sz * 0.28, 0, 0, Math.PI * 2);
    ctx.fill();
  }
  ctx.restore();
  ctx.restore();

  if (wc.borderWidth > 0) {
    ctx.save();
    ctx.strokeStyle = wc.borderColor;
    ctx.lineWidth = wc.borderWidth * unit;
    const inset = (wc.borderWidth * unit) / 2;
    ctx.beginPath();
    if (wc.shape === "circle") ctx.arc(wx + sz / 2, wy + sz / 2, sz / 2 - inset, 0, Math.PI * 2);
    else ctx.roundRect(wx + inset, wy + inset, sz - inset * 2, sz - inset * 2, wc.shape === "rounded" ? sz * 0.22 : 6 * unit);
    ctx.stroke();
    ctx.restore();
  }
}
