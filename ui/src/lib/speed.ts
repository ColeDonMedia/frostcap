import type { Clip, SpeedSegment } from "../types";
import { LIMITS } from "../backend";
import { clamp, uid } from "./format";

const MIN_LEN = 100;
const [MIN_SPEED, MAX_SPEED] = LIMITS.speed;

/** Speed at source time t (1 outside every segment; segment end is exclusive). */
export function speedAt(segs: SpeedSegment[], t: number): number {
  for (const s of segs) if (t >= s.start && t < s.end) return s.speed;
  return 1;
}

/**
 * Walks the kept clips (cuts apply first) and the speed segments on the source clock.
 * `until` stops the walk at that source time (default: the end).
 */
function walk(clips: Clip[], segs: SpeedSegment[], until = Infinity): number {
  const sorted = [...segs].sort((a, b) => a.start - b.start);
  let out = 0;
  for (const c of clips) {
    const end = Math.min(c.end, until);
    if (end <= c.start) break;
    let cur = c.start;
    for (const s of sorted) {
      if (s.end <= cur || s.start >= end) continue;
      const a = Math.max(s.start, cur);
      const b = Math.min(s.end, end);
      out += a - cur; // normal-speed part before the segment
      out += (b - a) / s.speed;
      cur = b;
    }
    out += Math.max(0, end - cur);
    if (until < c.end) break;
  }
  return out;
}

/** Output length: kept parts, each divided by its speed. */
export function outputLength(clips: Clip[], segs: SpeedSegment[]): number {
  return walk(clips, segs);
}

/** Source time -> output time through cuts and speed. */
export function outputTime(clips: Clip[], segs: SpeedSegment[], t: number): number {
  return walk(clips, segs, t);
}

/** New segment list, or null when it would overlap an existing one. Touching is allowed. */
export function addSpeedSegment(list: SpeedSegment[], start: number, end: number, speed: number, durationMs: number): SpeedSegment[] | null {
  const s = Math.round(clamp(start, 0, durationMs));
  const e = Math.round(clamp(end, 0, durationMs));
  if (e - s < MIN_LEN) return null;
  if (list.some((x) => s < x.end && x.start < e)) return null;
  return [...list, { id: uid("spd"), start: s, end: e, speed: clamp(speed, MIN_SPEED, MAX_SPEED) }].sort((a, b) => a.start - b.start);
}

/** Moves a segment's edges without crossing its neighbours or the recording, and keeps a minimum length. */
export function resizeSpeed(list: SpeedSegment[], id: string, patch: { start?: number; end?: number }, durationMs: number): SpeedSegment[] {
  const sorted = [...list].sort((a, b) => a.start - b.start);
  const i = sorted.findIndex((s) => s.id === id);
  if (i < 0) return list;
  const cur = sorted[i];
  const lo = sorted[i - 1]?.end ?? 0;
  const hi = sorted[i + 1]?.start ?? durationMs;
  if (patch.start !== undefined && patch.end !== undefined) {
    // Both edges: a move. The length is kept and the segment stops at its neighbours.
    const len = Math.max(MIN_LEN, patch.end - patch.start);
    if (len > hi - lo) return list; // does not fit between the neighbours: reject rather than overlap them
    const start = clamp(patch.start, lo, Math.max(lo, hi - len));
    return list.map((s) => (s.id === id ? { ...s, start: Math.round(start), end: Math.round(start + len) } : s));
  }
  let start = patch.start ?? cur.start;
  let end = patch.end ?? cur.end;
  // Guard short segments / tight neighbours: never an inverted clamp, never overlap.
  if (patch.start !== undefined) start = clamp(start, lo, Math.max(lo, cur.end - MIN_LEN));
  if (patch.end !== undefined) end = hi - cur.start < MIN_LEN ? cur.end : clamp(end, cur.start + MIN_LEN, hi);
  return list.map((s) => (s.id === id ? { ...s, start: Math.round(start), end: Math.round(end) } : s));
}
