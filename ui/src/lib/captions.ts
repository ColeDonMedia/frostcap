import type { CaptionCue } from "../types";
import { uid } from "./format";

function parseTs(ts: string): number {
  // 00:01:02,345 | 00:01:02.345 | 01:02.345
  const m = ts.trim().match(/(?:(\d+):)?(\d{1,2}):(\d{1,2})[.,](\d{1,3})/);
  if (!m) return NaN;
  const [, h, mi, s, ms] = m;
  return (Number(h ?? 0) * 3600 + Number(mi) * 60 + Number(s)) * 1000 + Number(ms.padEnd(3, "0"));
}

/** Parse SRT or WebVTT text into cues. Unknown blocks are skipped. */
export function parseSubtitles(text: string): CaptionCue[] {
  const blocks = text.replace(/\r/g, "").replace(/^﻿/, "").split(/\n{2,}/);
  const cues: CaptionCue[] = [];
  for (const block of blocks) {
    const lines = block.split("\n").filter((l) => l.trim() !== "");
    const i = lines.findIndex((l) => l.includes("-->"));
    if (i < 0) continue;
    const [a, b] = lines[i].split("-->");
    const start = parseTs(a);
    const end = parseTs(b.split(/\s+/).filter(Boolean)[0] ?? "");
    if (!Number.isFinite(start) || !Number.isFinite(end) || end <= start) continue;
    const body = lines
      .slice(i + 1)
      .join(" ")
      .replace(/<[^>]+>/g, "")
      .trim();
    if (body) cues.push({ id: uid("c"), start, end, text: body });
  }
  return cues.sort((x, y) => x.start - y.start);
}
