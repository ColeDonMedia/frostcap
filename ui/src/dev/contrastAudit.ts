/**
 * Dev-only contrast audit: for every visible text element, composite the
 * background colours of its ancestors (glass tints are translucent) over each
 * plausible backdrop (app background, and each mesh blob colour at its
 * on-screen strength), and report the worst-case WCAG ratio.
 * Run in the browser console: `__contrastAudit()`.
 */
import { composite, contrastRatio, parseColor, type RGBA } from "../lib/contrast";

export interface AuditRow {
  text: string;
  selector: string;
  fg: string;
  ratio: number;
  large: boolean;
  pass: boolean;
}

function selectorOf(el: Element): string {
  const cls = typeof el.className === "string" ? el.className.split(/\s+/).filter(Boolean).slice(0, 2).join(".") : "";
  return `${el.tagName.toLowerCase()}${cls ? "." + cls : ""}`;
}

export function runContrastAudit(root: ParentNode = document.body): { rows: AuditRow[]; failures: AuditRow[]; summary: string } {
  const rootStyle = getComputedStyle(document.documentElement);
  const appBg = parseColor(rootStyle.getPropertyValue("--app-bg-solid")) ?? { r: 0, g: 0, b: 0, a: 1 };
  const meshOpacity = parseFloat(rootStyle.getPropertyValue("--mesh-opacity")) || 0;
  const surface = document.documentElement.dataset.surface;
  // Blob centre strength ~ blob opacity (0.55) × mesh opacity; use full centre as worst case.
  const backdrops: RGBA[] = [appBg];
  if (surface !== "solid") {
    for (let i = 1; i <= 4; i++) {
      const c = parseColor(rootStyle.getPropertyValue(`--mesh-${i}`));
      if (c) backdrops.push(composite({ ...c, a: 0.55 * meshOpacity }, appBg));
    }
  }

  const rows: AuditRow[] = [];
  const seen = new Set<Element>();
  const walker = document.createTreeWalker(root as Node, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const text = (n.textContent ?? "").trim();
    const el = n.parentElement;
    if (!text || !el || seen.has(el)) continue;
    seen.add(el);
    const cs = getComputedStyle(el);
    const rect = el.getBoundingClientRect();
    if (cs.visibility === "hidden" || cs.display === "none" || rect.width === 0 || rect.height === 0 || parseFloat(cs.opacity) === 0) continue;
    if (el.closest("[aria-hidden=true], .sr-only, canvas, .preview")) continue; // preview badges sit on the opaque canvas, not glass
    if ((el as HTMLButtonElement).disabled || el.closest("button:disabled, [aria-disabled=true]")) continue; // WCAG exempts disabled controls
    const fg = parseColor(cs.color);
    if (!fg) continue;

    // Collect ancestor backgrounds (innermost first). Gradients count as their first stop (approximation).
    const layers: RGBA[] = [];
    let opacity = 1;
    for (let a: Element | null = el; a && a !== document.documentElement; a = a.parentElement) {
      const s = getComputedStyle(a);
      opacity *= parseFloat(s.opacity) || 1;
      let bg = parseColor(s.backgroundColor);
      if ((!bg || bg.a === 0) && s.backgroundImage.startsWith("linear-gradient")) {
        const m = s.backgroundImage.match(/rgba?\([^)]*\)/);
        bg = m ? parseColor(m[0]) : null;
      }
      if (bg && bg.a > 0) layers.push(bg);
      if (bg && bg.a >= 1) break;
    }
    let worst = Infinity;
    for (const base of backdrops) {
      let c = base;
      for (let i = layers.length - 1; i >= 0; i--) c = composite(layers[i], c);
      const fgc = composite({ ...fg, a: fg.a * opacity }, c);
      worst = Math.min(worst, contrastRatio(fgc, c));
    }
    const size = parseFloat(cs.fontSize);
    const weight = parseInt(cs.fontWeight, 10) || 400;
    const large = size >= 24 || (size >= 18.66 && weight >= 700);
    const ratio = Math.round(worst * 100) / 100;
    rows.push({ text: text.slice(0, 40), selector: selectorOf(el), fg: cs.color, ratio, large, pass: ratio >= (large ? 3 : 4.5) });
  }
  rows.sort((a, b) => a.ratio - b.ratio);
  const failures = rows.filter((r) => !r.pass);
  const summary = `${document.documentElement.dataset.theme}/${surface}: ${rows.length} text elements, ${failures.length} below AA, min ${rows[0]?.ratio ?? "-"}:1`;
  return { rows, failures, summary };
}

declare global {
  interface Window {
    __contrastAudit?: typeof runContrastAudit;
  }
}

if (import.meta.env.DEV) window.__contrastAudit = runContrastAudit;
