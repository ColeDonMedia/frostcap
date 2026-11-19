/** Colour-distance chroma key for the webcam bubble preview (a simple CbCr key, no segmentation). */

export function hexToRgb(hex: string): [number, number, number] {
  const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
  return m ? [parseInt(m[1], 16), parseInt(m[2], 16), parseInt(m[3], 16)] : [0, 255, 0];
}

export function rgbToHex(r: number, g: number, b: number): string {
  const h = (v: number) => Math.round(Math.min(255, Math.max(0, v))).toString(16).padStart(2, "0");
  return `#${h(r)}${h(g)}${h(b)}`;
}

const cbcr = (r: number, g: number, b: number) => [-0.169 * r - 0.331 * g + 0.5 * b, 0.5 * r - 0.419 * g - 0.081 * b] as const;
const smooth = (a: number, b: number, x: number) => {
  if (b <= a) return x > a ? 1 : 0;
  const k = Math.min(1, Math.max(0, (x - a) / (b - a)));
  return k * k * (3 - 2 * k);
};

export interface ChromaParams {
  color: string;
  /** 0..1: how close to the key colour still counts as key */
  similarity: number;
  /** 0..1: width of the soft edge above `similarity` */
  smoothness: number;
  /** 0..1: how much of the key colour cast is removed from kept pixels */
  spill: number;
}

/** In place on RGBA bytes: alpha from the CbCr distance to the key, then spill suppression on kept pixels. */
export function chromaKeyPixels(data: Uint8ClampedArray, p: ChromaParams): void {
  const [kr, kg, kb] = hexToRgb(p.color).map((v) => v / 255) as [number, number, number];
  const [kcb, kcr] = cbcr(kr, kg, kb);
  const dom = kr >= kg && kr >= kb ? 0 : kg >= kb ? 1 : 2;
  for (let i = 0; i < data.length; i += 4) {
    const r = data[i] / 255;
    const g = data[i + 1] / 255;
    const b = data[i + 2] / 255;
    const [cb, cr] = cbcr(r, g, b);
    const d = Math.hypot(cb - kcb, cr - kcr) / 0.8;
    const a = smooth(p.similarity, p.similarity + p.smoothness, d);
    data[i + 3] = Math.round(data[i + 3] * a);
    if (p.spill > 0 && a > 0) {
      const c = [r, g, b];
      const others = Math.max(c[(dom + 1) % 3], c[(dom + 2) % 3]);
      const excess = c[dom] - others;
      if (excess > 0) data[i + dom] = Math.round((c[dom] - p.spill * excess) * 255);
    }
  }
}
