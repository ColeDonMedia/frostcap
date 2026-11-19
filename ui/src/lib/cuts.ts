import type { Clip } from "../types";
import { uid } from "./format";

export interface CutRange {
  startMs: number;
  endMs: number;
}

/** Removes the ranges from the kept clips (source clock). The first piece of a split clip keeps its id. At least one clip always remains. */
export function removeRanges(clips: Clip[], ranges: CutRange[]): Clip[] {
  const cuts = ranges
    .filter((r) => r.endMs > r.startMs)
    .sort((a, b) => a.startMs - b.startMs)
    .reduce<CutRange[]>((acc, r) => {
      const last = acc.at(-1);
      if (last && r.startMs <= last.endMs) last.endMs = Math.max(last.endMs, r.endMs);
      else acc.push({ ...r });
      return acc;
    }, []);
  const out: Clip[] = [];
  for (const c of clips) {
    let from = c.start;
    let first = true;
    const piece = (start: number, end: number) => {
      if (end - start <= 0) return;
      out.push({ ...c, id: first ? c.id : uid("clip"), start, end });
      first = false;
    };
    for (const r of cuts) {
      if (r.endMs <= from || r.startMs >= c.end) continue;
      piece(from, Math.min(r.startMs, c.end));
      from = Math.max(from, r.endMs);
    }
    piece(from, c.end);
  }
  return out.length ? out : clips;
}
