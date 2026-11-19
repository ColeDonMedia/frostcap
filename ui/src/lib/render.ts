/**
 * Canvas preview renderer. Draws a faux captured screen and applies the
 * project style (background, padding, radius, shadow, frame, zoom segments,
 * cursor smoothing/size/ripple/blur, webcam bubble, captions).
 * This mirrors what the backend compositor is expected to output; it is a
 * preview only and does not decode real video.
 */
import type { Easing, Project, ZoomSegment } from "../types";
import { drawAnnotations, drawWebcam } from "./overlay";
import { cursorAt } from "../store/editor";

const SRC_W = 1600;
const SRC_H = 900;
let screenCache: HTMLCanvasElement | null = null;

function rr(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
  const rad = Math.max(0, Math.min(r, w / 2, h / 2));
  ctx.beginPath();
  ctx.roundRect(x, y, w, h, rad);
}

/** Pre-render a fake "app" screenshot once. */
function fauxScreen(): HTMLCanvasElement {
  if (screenCache) return screenCache;
  const c = document.createElement("canvas");
  c.width = SRC_W;
  c.height = SRC_H;
  const g = c.getContext("2d")!;
  g.fillStyle = "#11141c";
  g.fillRect(0, 0, SRC_W, SRC_H);
  // activity bar + sidebar
  g.fillStyle = "#0b0d13";
  g.fillRect(0, 0, 56, SRC_H);
  g.fillStyle = "#161a24";
  g.fillRect(56, 0, 280, SRC_H);
  for (let i = 0; i < 6; i++) {
    g.fillStyle = i === 1 ? "#4ed2fb" : "#3a4256";
    rr(g, 16, 24 + i * 56, 24, 24, 6);
    g.fill();
  }
  for (let i = 0; i < 16; i++) {
    g.fillStyle = i === 4 ? "#273047" : "transparent";
    g.fillRect(56, 70 + i * 34, 280, 30);
    g.fillStyle = i === 4 ? "#dbe4ff" : "#7d869c";
    rr(g, 84 + (i % 3) * 14, 80 + i * 34, 120 + ((i * 37) % 90), 10, 5);
    g.fill();
  }
  // tabs
  g.fillStyle = "#161a24";
  g.fillRect(336, 0, SRC_W - 336, 44);
  ["#1e2433", "#11141c", "#1e2433"].forEach((col, i) => {
    g.fillStyle = col;
    g.fillRect(336 + i * 170, 0, 168, 44);
    g.fillStyle = i === 1 ? "#e6ebf5" : "#6b7489";
    rr(g, 360 + i * 170, 18, 100, 9, 4);
    g.fill();
  });
  // code lines
  const palette = ["#c792ea", "#82aaff", "#c3e88d", "#f78c6c", "#89ddff", "#d6deeb"];
  for (let i = 0; i < 24; i++) {
    const y = 74 + i * 26;
    g.fillStyle = "#3b4256";
    g.font = "14px Consolas, monospace";
    g.fillText(String(i + 1).padStart(2, " "), 352, y + 9);
    let x = 400 + ((i * 7) % 4) * 28;
    const segs = 2 + ((i * 13) % 5);
    for (let s = 0; s < segs; s++) {
      const w = 30 + ((i * 31 + s * 17) % 110);
      g.fillStyle = palette[(i + s) % palette.length];
      g.globalAlpha = 0.85;
      rr(g, x, y, w, 10, 5);
      g.fill();
      x += w + 12;
    }
    g.globalAlpha = 1;
  }
  // preview panel with chart
  const px = 1080;
  g.fillStyle = "#171b26";
  rr(g, px, 70, 490, 520, 16);
  g.fill();
  const grad = g.createLinearGradient(px, 0, px + 490, 0);
  grad.addColorStop(0, "#4ed2fb");
  grad.addColorStop(0.5, "#389af5");
  grad.addColorStop(1, "#0752f1");
  for (let i = 0; i < 9; i++) {
    const h = 60 + ((i * 53) % 220);
    g.fillStyle = grad;
    rr(g, px + 30 + i * 50, 480 - h, 30, h, 8);
    g.fill();
  }
  g.fillStyle = "#e6ebf5";
  rr(g, px + 30, 100, 180, 14, 7);
  g.fill();
  g.fillStyle = "#6b7489";
  rr(g, px + 30, 126, 120, 10, 5);
  g.fill();
  // buttons
  g.fillStyle = grad;
  rr(g, px + 30, 520, 160, 44, 12);
  g.fill();
  g.fillStyle = "#2a3142";
  rr(g, px + 210, 520, 120, 44, 12);
  g.fill();
  // terminal
  g.fillStyle = "#0d1017";
  g.fillRect(336, 700, SRC_W - 336, 200);
  g.fillStyle = "#4ade80";
  rr(g, 360, 730, 14, 10, 3);
  g.fill();
  for (let i = 0; i < 5; i++) {
    g.fillStyle = i === 0 ? "#c3e88d" : "#6b7489";
    rr(g, 384, 730 + i * 28, 200 + ((i * 97) % 380), 10, 5);
    g.fill();
  }
  // status bar
  g.fillStyle = "#0752f1";
  g.fillRect(0, SRC_H - 24, SRC_W, 24);
  screenCache = c;
  return c;
}

/** Identical to `ease()` in crates/frostcap-render/src/math.rs. */
export function ease(e: Easing | "easeOutCubic", k: number): number {
  k = Math.min(1, Math.max(0, k));
  switch (e) {
    case "linear":
      return k;
    case "easeOutCubic":
      return 1 - Math.pow(1 - k, 3);
    case "easeOutQuint":
      return 1 - Math.pow(1 - k, 5);
    case "spring":
      return k >= 1 ? 1 : 1 - Math.exp(-6.5 * k) * Math.cos(9 * k);
    case "easeInOutCubic":
    default:
      return k < 0.5 ? 4 * k * k * k : 1 - Math.pow(-2 * k + 2, 3) / 2;
  }
}

/** Mirrors `zoom_at()` in math.rs: first segment with start <= t <= end, ramp capped at half length. */
export function zoomAt(segs: ZoomSegment[], t: number): { seg: ZoomSegment | null; e: number } {
  for (const s of segs) {
    if (t < s.start || t > s.end) continue;
    const len = s.end - s.start;
    const tr = Math.min(s.transitionMs, len / 2);
    let k = 1;
    if (tr > 0) {
      if (t < s.start + tr) k = (t - s.start) / tr;
      else if (t > s.end - tr) k = (s.end - t) / tr;
    }
    return { seg: s, e: ease(s.easing, k) };
  }
  return { seg: null, e: 0 };
}

/** Title bar above the screen: none for "none" and the device frames (laptop / phone bring their own bezel). */
export function titleBarHeight(frame: Project["style"]["frame"], unit: number): number {
  return frame === "none" || frame === "laptop" || frame === "phone" ? 0 : frame === "minimal" ? 22 * unit : 34 * unit;
}

/** Room a device frame needs around the screen (bezel, laptop base) so it is never clipped at the canvas edge. */
export function frameInsets(frame: Project["style"]["frame"], unit: number): { x: number; y: number } {
  if (frame === "laptop") return { x: 60 * unit, y: 30 * unit };
  if (frame === "phone") return { x: 22 * unit, y: 22 * unit };
  return { x: 0, y: 0 };
}

/** Content (captured screen) rect in unzoomed canvas space — used for click-to-focus. */
export function contentRect(W: number, H: number, p: Project) {
  const st = p.style;
  const unit = H / 1080;
  const pad = (st.padding / 100) * Math.min(W, H * 1.78);
  const barH = titleBarHeight(st.frame, unit);
  const ins = frameInsets(st.frame, unit);
  const availW = W - pad * 2 - ins.x * 2;
  const availH = H - pad * 2 - ins.y * 2;
  const srcAspect = p.media.width / p.media.height;
  let cw = availW;
  let ch = cw / srcAspect;
  if (ch + barH > availH) {
    ch = availH - barH;
    cw = ch * srcAspect;
  }
  return { x: (W - cw) / 2, y: (H - ch - barH) / 2 + barH, w: cw, h: ch };
}

/** Zoom at time t as "scale S about canvas centre, looking at (tx, ty)"; null when not zoomed. */
export function zoomTransform(W: number, H: number, p: Project, t: number): { S: number; tx: number; ty: number } | null {
  const { seg, e } = zoomAt(p.zoomSegments, t);
  if (!seg || e <= 0) return null;
  const cr = contentRect(W, H, p);
  const S = 1 + (seg.scale - 1) * e;
  const fx = cr.x + seg.cx * cr.w;
  const fy = cr.y + seg.cy * cr.h;
  const minX = W / (2 * S);
  const minY = H / (2 * S);
  const tx = Math.min(W - minX, Math.max(minX, W / 2 + (fx - W / 2) * Math.min(1, e)));
  const ty = Math.min(H - minY, Math.max(minY, H / 2 + (fy - H / 2) * Math.min(1, e)));
  return { S, tx, ty };
}

/** Inverse of the preview layout + zoom: canvas pixel -> normalized content point (clamped 0..1). */
export function canvasToContent(px: number, py: number, W: number, H: number, p: Project, t: number): { x: number; y: number } {
  const cr = contentRect(W, H, p);
  const xf = zoomTransform(W, H, p, t);
  const wx = xf ? (px - W / 2) / xf.S + xf.tx : px;
  const wy = xf ? (py - H / 2) / xf.S + xf.ty : py;
  const c01 = (v: number) => Math.min(1, Math.max(0, v));
  return { x: c01((wx - cr.x) / cr.w), y: c01((wy - cr.y) / cr.h) };
}

/** Forward map: normalized content point -> canvas pixel (through the layout and the current zoom). */
export function contentToCanvas(x: number, y: number, W: number, H: number, p: Project, t: number): { x: number; y: number } {
  const cr = contentRect(W, H, p);
  const xf = zoomTransform(W, H, p, t);
  const wx = cr.x + x * cr.w;
  const wy = cr.y + y * cr.h;
  return xf ? { x: (wx - xf.tx) * xf.S + W / 2, y: (wy - xf.ty) * xf.S + H / 2 } : { x: wx, y: wy };
}

/** Export semantics: smoothing applies only while the switch is on; hidden or baked cursors are never drawn. */
export function effectiveCursor(p: Project): { draw: boolean; smoothing: number } {
  const c = p.style.cursor;
  return { draw: c.visible && !p.cursorBaked, smoothing: c.smooth ? c.smoothing : 0 };
}

/** Like the exporter: the first enabled cue at t with visible text and non-zero length. */
export function activeCaption(p: Project, t: number) {
  if (!p.style.captions.enabled) return null;
  return p.captions.find((c) => c.end > c.start && c.text.trim() !== "" && t >= c.start && t <= c.end) ?? null;
}

/** The exporter skips the bubble without a webcam stream; demo projects (no media URLs) show the placeholder. */
export function webcamVisible(p: Project): boolean {
  if (!p.style.webcam.enabled) return false;
  return !!p.media.webcam || !p.media.screen?.url;
}

export interface RenderOpts {
  /** Highlight the zoom focus (when a segment is selected). */
  showFocus?: boolean;
  /** Real video frame (screen.mp4). Falls back to the synthetic demo screen. */
  frame?: CanvasImageSource | null;
  /** Real webcam frame (webcam.mp4). */
  webcamFrame?: CanvasImageSource | null;
  /** Loaded project images: the framed-screen background and the webcam background replacement. */
  images?: { background?: CanvasImageSource | null; webcamBackground?: CanvasImageSource | null };
}

export function renderFrame(ctx: CanvasRenderingContext2D, W: number, H: number, p: Project, t: number, opts: RenderOpts = {}) {
  const st = p.style;
  const unit = H / 1080; // style values are expressed at 1080p
  ctx.save();
  ctx.clearRect(0, 0, W, H);

  // 1. Background gradient
  const ang = (st.background.angle * Math.PI) / 180;
  const dx = Math.sin(ang);
  const dy = -Math.cos(ang);
  const half = (Math.abs(W * dx) + Math.abs(H * dy)) / 2;
  const bg = ctx.createLinearGradient(W / 2 - dx * half, H / 2 - dy * half, W / 2 + dx * half, H / 2 + dy * half);
  st.background.stops.forEach((c, i, a) => bg.addColorStop(a.length === 1 ? 0 : i / (a.length - 1), c));
  ctx.fillStyle = bg;
  ctx.fillRect(0, 0, W, H);
  if (st.backgroundImage && opts.images?.background) {
    const img = opts.images.background as CanvasImageSource & { naturalWidth?: number; naturalHeight?: number; width?: number; height?: number };
    const iw = img.naturalWidth || (img.width as number) || 1;
    const ih = img.naturalHeight || (img.height as number) || 1;
    const k = st.backgroundImage.fit === "contain" ? Math.min(W / iw, H / ih) : Math.max(W / iw, H / ih);
    ctx.save();
    if (st.backgroundImage.blur > 0) ctx.filter = `blur(${st.backgroundImage.blur * unit}px)`;
    ctx.drawImage(img, (W - iw * k) / 2, (H - ih * k) / 2, iw * k, ih * k);
    ctx.restore();
  }

  // 2. Content rect (fit source aspect inside padded area)
  const pad = (st.padding / 100) * Math.min(W, H * 1.78);
  const barH = titleBarHeight(st.frame, unit);
  const ins = frameInsets(st.frame, unit);
  const availW = W - pad * 2 - ins.x * 2;
  const availH = H - pad * 2 - ins.y * 2;
  const srcAspect = p.media.width / p.media.height;
  let cw = availW;
  let ch = cw / srcAspect;
  if (ch + barH > availH) {
    ch = availH - barH;
    cw = ch * srcAspect;
  }
  const rx = (W - cw) / 2;
  const ry = (H - ch - barH) / 2;
  const radius = st.radius * unit;

  // 3. Zoom transform
  const { seg } = zoomAt(p.zoomSegments, t);
  const xf = zoomTransform(W, H, p, t);
  if (xf) {
    ctx.translate(W / 2, H / 2);
    ctx.scale(xf.S, xf.S);
    ctx.translate(-xf.tx, -xf.ty);
  }

  // 4. Shadow + window
  if (st.shadow > 0) {
    ctx.save();
    ctx.shadowColor = `rgba(0,0,0,${0.15 + (st.shadow / 100) * 0.45})`;
    ctx.shadowBlur = (st.shadow / 100) * 70 * unit;
    ctx.shadowOffsetY = (st.shadow / 100) * 24 * unit;
    rr(ctx, rx, ry, cw, ch + barH, radius);
    ctx.fillStyle = "#11141c";
    ctx.fill();
    ctx.restore();
  }
  if (st.frame === "laptop" || st.frame === "phone") {
    ctx.save();
    ctx.fillStyle = "#0b0d14";
    if (st.frame === "laptop") {
      const b = 14 * unit;
      rr(ctx, rx - b, ry - b, cw + b * 2, ch + b * 2, 16 * unit);
      ctx.fill();
      ctx.fillStyle = "#2a2f3d";
      rr(ctx, rx - b - 46 * unit, ry + ch + b, cw + b * 2 + 92 * unit, 16 * unit, 8 * unit);
      ctx.fill();
      ctx.fillStyle = "#171a24";
      rr(ctx, rx + cw / 2 - 60 * unit, ry + ch + b, 120 * unit, 6 * unit, 3 * unit);
      ctx.fill();
    } else {
      const b = 22 * unit;
      rr(ctx, rx - b, ry - b, cw + b * 2, ch + b * 2, 44 * unit);
      ctx.fill();
      ctx.fillStyle = "#1c2030";
      rr(ctx, rx + cw / 2 - 40 * unit, ry - b / 2 - 5 * unit, 80 * unit, 10 * unit, 5 * unit);
      ctx.fill();
    }
    ctx.restore();
  }
  ctx.save();
  rr(ctx, rx, ry, cw, ch + barH, radius);
  ctx.clip();
  if (barH > 0) {
    ctx.fillStyle = st.frame === "windows" ? "#202020" : "#1c2030";
    ctx.fillRect(rx, ry, cw, barH);
    if (st.frame === "macos") {
      ["#ff5f57", "#febc2e", "#28c840"].forEach((c, i) => {
        ctx.fillStyle = c;
        ctx.beginPath();
        ctx.arc(rx + 18 * unit + i * 18 * unit, ry + barH / 2, 5.5 * unit, 0, Math.PI * 2);
        ctx.fill();
      });
    } else if (st.frame === "windows") {
      ctx.strokeStyle = "#c8c8c8";
      ctx.lineWidth = 1.2 * unit;
      const y = ry + barH / 2;
      const x0 = rx + cw - 20 * unit;
      ctx.beginPath();
      ctx.moveTo(x0 - 5 * unit, y - 5 * unit);
      ctx.lineTo(x0 + 5 * unit, y + 5 * unit);
      ctx.moveTo(x0 + 5 * unit, y - 5 * unit);
      ctx.lineTo(x0 - 5 * unit, y + 5 * unit);
      ctx.strokeRect(x0 - 50 * unit, y - 5 * unit, 10 * unit, 10 * unit);
      ctx.moveTo(x0 - 100 * unit, y);
      ctx.lineTo(x0 - 90 * unit, y);
      ctx.stroke();
    } else {
      ctx.fillStyle = "rgba(255,255,255,0.14)";
      rr(ctx, rx + cw / 2 - 40 * unit, ry + barH / 2 - 3 * unit, 80 * unit, 6 * unit, 3 * unit);
      ctx.fill();
    }
  }
  ctx.drawImage(opts.frame ?? fauxScreen(), rx, ry + barH, cw, ch);
  ctx.restore();

  if (opts.showFocus && seg) {
    ctx.save();
    ctx.strokeStyle = "rgba(78,210,251,0.9)";
    ctx.setLineDash([6, 4]);
    ctx.lineWidth = 2;
    const fx = rx + seg.cx * cw;
    const fy = ry + barH + seg.cy * ch;
    ctx.beginPath();
    ctx.arc(fx, fy, 18 * unit, 0, Math.PI * 2);
    ctx.stroke();
    ctx.restore();
  }

  // 4b. Annotations: in screen-content space (they follow the zoom), under the cursor, webcam and captions
  if (p.annotations?.length) drawAnnotations(ctx, p.annotations, t, { x: rx, y: ry + barH, w: cw, h: ch }, unit, xf?.S ?? 1, opts.frame ?? fauxScreen());

  // 5. Clicks + cursor (in content space, so they zoom with the screen)
  const toCanvas = (x: number, y: number) => [rx + x * cw, ry + barH + y * ch] as const;
  if (st.cursor.clickRipple) {
    for (const c of p.clicks) {
      if (c.state !== "down") continue;
      const age = t - c.t;
      if (age < 0 || age > 550) continue;
      const k = age / 550;
      const [x, y] = toCanvas(c.x, c.y);
      ctx.beginPath();
      ctx.arc(x, y, (8 + 34 * ease("easeOutCubic", k)) * unit * st.cursor.size * 0.8, 0, Math.PI * 2);
      ctx.strokeStyle = `rgba(78,210,251,${0.9 * (1 - k)})`;
      ctx.lineWidth = 3 * unit;
      ctx.stroke();
      ctx.fillStyle = `rgba(78,210,251,${0.25 * (1 - k)})`;
      ctx.fill();
    }
  }
  const eff = effectiveCursor(p);
  if (eff.draw) {
    const smoothWin = eff.smoothing * 220;
    const pos = (tt: number) => {
      if (smoothWin < 5) return cursorAt(p, tt);
      let sx = 0;
      let sy = 0;
      const n = 7;
      for (let i = 0; i < n; i++) {
        const q = cursorAt(p, tt - smoothWin + (i / (n - 1)) * smoothWin);
        sx += q.x;
        sy += q.y;
      }
      return { x: sx / n, y: sy / n };
    };
    const size = st.cursor.size * unit;
    if (st.cursor.motionBlur > 0.02) {
      for (let i = 3; i >= 1; i--) {
        const q = pos(t - i * 18 * st.cursor.motionBlur * 2);
        const [x, y] = toCanvas(q.x, q.y);
        drawCursor(ctx, x, y, size, 0.18 * st.cursor.motionBlur * (4 - i));
      }
    }
    const q = pos(t);
    const [x, y] = toCanvas(q.x, q.y);
    drawCursor(ctx, x, y, size, 1);
  }
  ctx.restore();

  // 6. Webcam (unzoomed overlay)
  if (webcamVisible(p)) drawWebcam(ctx, W, H, st.webcam, opts.webcamFrame, { background: opts.images?.webcamBackground });

  // 6b. Keystroke overlay
  drawKeys(ctx, W, H, p, t);

  // 7. Captions
  {
    const cue = activeCaption(p, t);
    if (cue) {
      const fs = st.captions.fontSize * unit;
      ctx.save();
      ctx.font = `600 ${fs}px "Segoe UI Variable", "Segoe UI", system-ui, sans-serif`;
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      const maxW = W * 0.8;
      const lines = wrapCaption((s) => ctx.measureText(s).width, cue.text, maxW);
      const lh = fs * 1.3;
      const blockH = lines.length * lh;
      const cy = st.captions.position === "bottom" ? H - 48 * unit - blockH / 2 : 48 * unit + blockH / 2;
      if (st.captions.background) {
        const bw = Math.max(...lines.map((l) => ctx.measureText(l).width)) + fs * 1.2;
        ctx.fillStyle = "rgba(8,10,18,0.72)";
        rr(ctx, W / 2 - bw / 2, cy - blockH / 2 - fs * 0.35, bw, blockH + fs * 0.7, fs * 0.45);
        ctx.fill();
      }
      ctx.fillStyle = "#ffffff";
      if (!st.captions.background) {
        ctx.shadowColor = "rgba(0,0,0,0.8)";
        ctx.shadowBlur = 8 * unit;
      }
      lines.forEach((l, i) => ctx.fillText(l, W / 2, cy - blockH / 2 + lh / 2 + i * lh));
      ctx.restore();
    }
  }
}

/** Keystroke overlay: the last few key presses within 1.5 s, as one pill. */
export function drawKeys(ctx: CanvasRenderingContext2D, W: number, H: number, p: Project, t: number) {
  if (!p.style.keys.show || !p.keys.length) return;
  const recent = p.keys.filter((k) => k.t <= t && t - k.t < 1500).slice(-4);
  if (!recent.length) return;
  const unit = H / 1080;
  const label = recent
    .map((k) => [k.modifiers.ctrl && "Ctrl", k.modifiers.alt && "Alt", k.modifiers.shift && "Shift", k.modifiers.win && "Win", k.label].filter(Boolean).join(" + "))
    .join("  ");
  const age = t - recent[recent.length - 1].t;
  const alpha = age > 1200 ? 1 - (age - 1200) / 300 : 1;
  const fs = 30 * unit;
  ctx.save();
  ctx.globalAlpha = Math.max(0, alpha);
  ctx.font = `600 ${fs}px "Segoe UI Variable", "Segoe UI", system-ui, sans-serif`;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  const w = ctx.measureText(label).width + fs * 1.4;
  const h = fs * 1.9;
  const y = p.style.captions.position === "bottom" && p.style.captions.enabled ? 40 * unit + h / 2 : H - 40 * unit - h / 2;
  ctx.fillStyle = "rgba(8,10,18,0.78)";
  ctx.beginPath();
  ctx.roundRect(W / 2 - w / 2, y - h / 2, w, h, h / 2);
  ctx.fill();
  ctx.fillStyle = "#fcfdfc";
  ctx.fillText(label, W / 2, y);
  ctx.restore();
}

/** Characters of a cue that are ever laid out (matches the Rust renderer). */
export const CAPTION_MAX_CHARS = 500;
export const CAPTION_MAX_LINES = 3;

/**
 * Greedy word wrap to `maxW`, at most 3 lines, over the first 500 chars.
 * Linear: each word is measured once and line widths are summed (a word wider
 * than a line is hard-broken using cached per-char widths); stops once 3 lines
 * are full. `measure` is ctx.measureText(s).width in the renderer.
 */
export function wrapCaption(measure: (s: string) => number, text: string, maxW: number): string[] {
  const capped = Array.from(text.slice(0, CAPTION_MAX_CHARS * 2)).slice(0, CAPTION_MAX_CHARS).join("");
  const words = capped.split(/\s+/).filter(Boolean);
  const out: string[] = [];
  if (!words.length) return out;
  const spaceW = measure(" ");
  const charW = new Map<string, number>();
  const cw = (ch: string) => {
    let w = charW.get(ch);
    if (w === undefined) charW.set(ch, (w = measure(ch)));
    return w;
  };
  let line = "";
  let lineW = 0;
  const push = (s: string) => out.push(s) >= CAPTION_MAX_LINES;
  for (const word of words) {
    const w = measure(word);
    if (w > maxW) {
      if (line && push(line)) return out;
      let chunk = "";
      let chunkW = 0;
      for (const ch of word) {
        const c = cw(ch);
        if (chunk && chunkW + c > maxW) {
          if (push(chunk)) return out;
          chunk = "";
          chunkW = 0;
        }
        chunk += ch;
        chunkW += c;
      }
      line = chunk;
      lineW = chunkW;
    } else if (line && lineW + spaceW + w > maxW) {
      if (push(line)) return out;
      line = word;
      lineW = w;
    } else {
      lineW = line ? lineW + spaceW + w : w;
      line = line ? `${line} ${word}` : word;
    }
  }
  if (line) push(line);
  return out;
}

function drawCursor(ctx: CanvasRenderingContext2D, x: number, y: number, s: number, alpha: number) {
  ctx.save();
  ctx.globalAlpha = alpha;
  ctx.translate(x, y);
  ctx.scale(s, s);
  ctx.beginPath();
  ctx.moveTo(0, 0);
  ctx.lineTo(0, 22);
  ctx.lineTo(5.5, 17);
  ctx.lineTo(9.5, 26);
  ctx.lineTo(13, 24.5);
  ctx.lineTo(9, 15.5);
  ctx.lineTo(16, 15.5);
  ctx.closePath();
  ctx.fillStyle = "#ffffff";
  ctx.strokeStyle = "#0b0d13";
  ctx.lineWidth = 1.6;
  ctx.lineJoin = "round";
  ctx.shadowColor = "rgba(0,0,0,0.35)";
  ctx.shadowBlur = 4;
  ctx.shadowOffsetY = 1;
  ctx.fill();
  ctx.shadowColor = "transparent";
  ctx.stroke();
  ctx.restore();
}
