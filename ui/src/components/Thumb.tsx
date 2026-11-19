import { useId } from "react";
import type { SourceKind } from "../types";

const HUES: [string, string][] = [
  ["#1e3a8a", "#6d28d9"],
  ["#0e7490", "#1e40af"],
  ["#7c2d12", "#be185d"],
  ["#134e4a", "#0f766e"],
  ["#312e81", "#0369a1"],
  ["#3f3f46", "#1e1b4b"],
];

function hash(s: string) {
  let h = 0;
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) | 0;
  return Math.abs(h);
}

/** Generated placeholder thumbnail (no images shipped). */
export function Thumb({ seed, kind = "display" }: { seed: string; kind?: SourceKind | "region" }) {
  const id = useId().replace(/:/g, "");
  const h = hash(seed);
  const [a, b] = HUES[h % HUES.length];
  const win = (x: number, y: number, w: number, hgt: number, k: number) => (
    <g key={k}>
      <rect x={x} y={y} width={w} height={hgt} rx="5" fill="#0f131d" opacity="0.92" />
      <rect x={x} y={y} width={w} height="9" rx="5" fill="#1d2232" />
      <circle cx={x + 6} cy={y + 4.5} r="1.8" fill="#ff5f57" />
      <circle cx={x + 12} cy={y + 4.5} r="1.8" fill="#febc2e" />
      <circle cx={x + 18} cy={y + 4.5} r="1.8" fill="#28c840" />
      {[0, 1, 2, 3, 4].map((i) => (
        <rect
          key={i}
          x={x + 8}
          y={y + 16 + i * 8}
          width={Math.max(10, (w - 20) * (0.4 + (((h >> i) & 7) / 7) * 0.6))}
          height="3.5"
          rx="1.75"
          fill={i === 0 ? "#4ed2fb" : "#4b5470"}
          opacity="0.9"
        />
      ))}
    </g>
  );
  return (
    <svg viewBox="0 0 160 90" preserveAspectRatio="xMidYMid slice" aria-hidden="true">
      <defs>
        <linearGradient id={`g${id}`} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor={a} />
          <stop offset="1" stopColor={b} />
        </linearGradient>
        <radialGradient id={`r${id}`} cx="0.8" cy="0.1" r="0.8">
          <stop offset="0" stopColor="#fff" stopOpacity="0.28" />
          <stop offset="1" stopColor="#fff" stopOpacity="0" />
        </radialGradient>
      </defs>
      <rect width="160" height="90" fill={`url(#g${id})`} />
      <rect width="160" height="90" fill={`url(#r${id})`} />
      {kind === "display" && (
        <>
          {win(14, 12, 78, 56, 1)}
          {win(70, 26, 76, 52, 2)}
          <rect x="0" y="82" width="160" height="8" fill="#0b0e16" opacity="0.7" />
        </>
      )}
      {kind === "window" && win(20, 10, 120, 70, 1)}
      {kind === "region" && (
        <>
          {win(8, 8, 90, 60, 1)}
          <rect x="40" y="22" width="90" height="52" rx="3" fill="rgba(78,210,251,0.14)" stroke="#4ed2fb" strokeWidth="1.5" strokeDasharray="5 3" />
          {[
            [40, 22],
            [130, 22],
            [40, 74],
            [130, 74],
          ].map(([x, y], i) => (
            <rect key={i} x={x - 2.5} y={y - 2.5} width="5" height="5" rx="1" fill="#fff" />
          ))}
        </>
      )}
    </svg>
  );
}
