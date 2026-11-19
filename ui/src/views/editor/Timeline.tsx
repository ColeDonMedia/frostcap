import { Fragment, useEffect, useMemo, useRef, useState, type KeyboardEvent, type PointerEvent as RPointerEvent } from "react";
import { Magnet, Minus, Plus, Scissors, ZoomIn, Trash2, Film, MousePointerClick, AudioLines, Maximize2 } from "lucide-react";
import { useEditor } from "../../store/editor";
import { demoPeaks, loadPeaks } from "../../lib/media";
import type { Peaks } from "../../lib/wav";
import type { Project } from "../../types";
import { clamp, fmtTime } from "../../lib/format";

const SNAP_PX = 8;
const MIN_ZOOM_MS = 300;
const MIN_CLIP_MS = 200;

function snapTargets(p: Project, playhead: number, excludeId?: string): number[] {
  const out = [0, p.media.durationMs, playhead];
  for (const c of p.clips) if (c.id !== excludeId) out.push(c.start, c.end);
  for (const z of p.zoomSegments) if (z.id !== excludeId) out.push(z.start, z.end);
  for (const c of p.clicks) if (c.state === "down") out.push(c.t);
  return out;
}

function snapValue(v: number, targets: number[], thresholdMs: number): { v: number; snapped: number | null } {
  let best: number | null = null;
  let bd = thresholdMs;
  for (const t of targets) {
    const d = Math.abs(t - v);
    if (d < bd) {
      bd = d;
      best = t;
    }
  }
  return best === null ? { v, snapped: null } : { v: best, snapped: best };
}

/** Pointer travel (px) before a press on a clip turns into scrubbing. */
const DRAG_THRESHOLD_PX = 4;

type Drag =
  | { kind: "scrub" }
  | { kind: "press"; x0: number }
  | { kind: "zoom-move" | "zoom-l" | "zoom-r"; id: string; x0: number; start: number; end: number }
  | { kind: "clip-l" | "clip-r"; id: string; x0: number; start: number; end: number };

export function Timeline() {
  const project = useEditor((s) => s.project)!;
  const playhead = useEditor((s) => s.playhead);
  const playing = useEditor((s) => s.playing);
  const selection = useEditor((s) => s.selection);
  const snap = useEditor((s) => s.snap);
  const pps = useEditor((s) => s.pxPerSec);
  const ed = useEditor.getState;
  const scrollRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const drag = useRef<Drag | null>(null);
  const [snapLine, setSnapLine] = useState<number | null>(null);

  const dur = project.media.durationMs;
  const msToPx = (ms: number) => (ms / 1000) * pps;
  const width = msToPx(dur) + 40;

  // Fit the timeline to its container on first load.
  useEffect(() => {
    const el = scrollRef.current;
    if (el) ed().setPxPerSec(Math.max(8, (el.clientWidth - 40) / (dur / 1000)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [project.wire]);

  // Follow the playhead while playing.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el || !playing) return;
    const x = msToPx(playhead);
    if (x > el.scrollLeft + el.clientWidth - 60 || x < el.scrollLeft) el.scrollLeft = x - 60;
  });

  // Ctrl+wheel zooms the timeline (non-passive listener so we can preventDefault).
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      ed().setPxPerSec(ed().pxPerSec * (e.deltaY < 0 ? 1.15 : 1 / 1.15));
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [ed]);

  const xToMs = (clientX: number) => {
    const r = contentRef.current!.getBoundingClientRect();
    return clamp(((clientX - r.left) / pps) * 1000, 0, dur);
  };

  const startDrag = (e: RPointerEvent, d: Drag) => {
    e.stopPropagation();
    (e.currentTarget as Element).setPointerCapture?.(e.pointerId);
    drag.current = d;
    if (d.kind === "press") ed().setPlaying(false);
    else if (d.kind !== "scrub") ed().beginGesture();
    else {
      ed().setPlaying(false);
      ed().setPlayhead(xToMs(e.clientX));
    }
  };

  const onMove = (e: RPointerEvent) => {
    let d = drag.current;
    if (!d) return;
    if (d.kind === "press") {
      if (Math.abs(e.clientX - d.x0) < DRAG_THRESHOLD_PX) return;
      d = drag.current = { kind: "scrub" };
    }
    const p = ed().project!;
    const thr = snap && !e.altKey ? (SNAP_PX / pps) * 1000 : 0;
    if (d.kind === "scrub") {
      const t = xToMs(e.clientX);
      const s = thr ? snapValue(t, snapTargets(p, -1e9), thr) : { v: t, snapped: null };
      setSnapLine(s.snapped);
      ed().setPlayhead(s.v);
      return;
    }
    const dms = ((e.clientX - d.x0) / pps) * 1000;
    const targets = thr ? snapTargets(p, ed().playhead, d.id) : [];
    if (d.kind.startsWith("zoom")) {
      const others = p.zoomSegments.filter((z) => z.id !== d.id);
      const prevEnd = Math.max(0, ...others.filter((z) => z.end <= d.start + 1).map((z) => z.end));
      const nextStart = Math.min(dur, ...others.filter((z) => z.start >= d.end - 1).map((z) => z.start));
      let start = d.start;
      let end = d.end;
      let snapped: number | null = null;
      if (d.kind === "zoom-move") {
        const len = d.end - d.start;
        start = d.start + dms;
        if (thr) {
          const a = snapValue(start, targets, thr);
          const b = snapValue(start + len, targets, thr);
          if (a.snapped !== null) {
            start = a.v;
            snapped = a.snapped;
          } else if (b.snapped !== null) {
            start = b.v - len;
            snapped = b.snapped;
          }
        }
        start = clamp(start, prevEnd, nextStart - len);
        end = start + len;
      } else if (d.kind === "zoom-l") {
        const s = thr ? snapValue(d.start + dms, targets, thr) : { v: d.start + dms, snapped: null };
        snapped = s.snapped;
        start = clamp(s.v, prevEnd, d.end - MIN_ZOOM_MS);
      } else {
        const s = thr ? snapValue(d.end + dms, targets, thr) : { v: d.end + dms, snapped: null };
        snapped = s.snapped;
        end = clamp(s.v, d.start + MIN_ZOOM_MS, nextStart);
      }
      setSnapLine(snapped);
      ed().updateZoom(d.id, { start: Math.round(start), end: Math.round(end) }, { history: false });
    } else {
      const idx = p.clips.findIndex((c) => c.id === d.id);
      const prevEnd = idx > 0 ? p.clips[idx - 1].end : 0;
      const nextStart = idx < p.clips.length - 1 ? p.clips[idx + 1].start : dur;
      const base = d.kind === "clip-l" ? d.start : d.end;
      const s = thr ? snapValue(base + dms, targets, thr) : { v: base + dms, snapped: null };
      setSnapLine(s.snapped);
      if (d.kind === "clip-l") ed().updateClip(d.id, { start: Math.round(clamp(s.v, prevEnd, d.end - MIN_CLIP_MS)) }, { history: false });
      else ed().updateClip(d.id, { end: Math.round(clamp(s.v, d.start + MIN_CLIP_MS, nextStart)) }, { history: false });
    }
  };

  const onUp = (e: RPointerEvent) => {
    // A click (no drag) on a clip places the playhead there, so S splits at the click.
    if (drag.current?.kind === "press") ed().setPlayhead(xToMs(e.clientX));
    else if (drag.current && drag.current.kind !== "scrub") ed().endGesture();
    drag.current = null;
    setSnapLine(null);
  };

  // Keyboard on a zoom segment: ←/→ move 100 ms (Shift 1 s); Alt+←/→ resize end.
  const zoomKey = (e: KeyboardEvent, id: string) => {
    if (e.key === "Delete" || e.key === "Backspace") {
      e.preventDefault();
      e.stopPropagation();
      ed().select({ kind: "zoom", id });
      ed().deleteSelection();
      return;
    }
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    e.stopPropagation();
    const p = ed().project!;
    const z = p.zoomSegments.find((x) => x.id === id)!;
    const others = p.zoomSegments.filter((x) => x.id !== id);
    const prevEnd = Math.max(0, ...others.filter((x) => x.end <= z.start + 1).map((x) => x.end));
    const nextStart = Math.min(dur, ...others.filter((x) => x.start >= z.end - 1).map((x) => x.start));
    const d = (e.shiftKey ? 1000 : 100) * (e.key === "ArrowLeft" ? -1 : 1);
    if (e.altKey) {
      ed().updateZoom(id, { end: clamp(z.end + d, z.start + MIN_ZOOM_MS, nextStart) }, { coalesce: `zk.${id}` });
    } else {
      const len = z.end - z.start;
      const start = clamp(z.start + d, prevEnd, nextStart - len);
      ed().updateZoom(id, { start, end: start + len }, { coalesce: `zk.${id}` });
    }
  };

  const clipKey = (e: KeyboardEvent, id: string, side: "l" | "r") => {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    e.stopPropagation();
    const p = ed().project!;
    const idx = p.clips.findIndex((c) => c.id === id);
    const c = p.clips[idx];
    const prevEnd = idx > 0 ? p.clips[idx - 1].end : 0;
    const nextStart = idx < p.clips.length - 1 ? p.clips[idx + 1].start : dur;
    const d = (e.shiftKey ? 1000 : 100) * (e.key === "ArrowLeft" ? -1 : 1);
    if (side === "l") ed().apply((pp) => ({ ...pp, clips: pp.clips.map((x) => (x.id === id ? { ...x, start: clamp(c.start + d, prevEnd, c.end - MIN_CLIP_MS) } : x)) }), { coalesce: `ck.${id}.l` });
    else ed().apply((pp) => ({ ...pp, clips: pp.clips.map((x) => (x.id === id ? { ...x, end: clamp(c.end + d, c.start + MIN_CLIP_MS, nextStart) } : x)) }), { coalesce: `ck.${id}.r` });
  };

  // Ruler ticks
  const ticks = useMemo(() => {
    const steps = [250, 500, 1000, 2000, 5000, 10000, 15000, 30000, 60000];
    const major = steps.find((s) => (s / 1000) * pps >= 70) ?? 60000;
    const out: { t: number; major: boolean }[] = [];
    const minor = major / 4;
    for (let t = 0; t <= dur; t += minor) out.push({ t, major: t % major === 0 });
    return out;
  }, [pps, dur]);

  // Gaps (trimmed regions) between clips
  const gaps = useMemo(() => {
    const g: { start: number; end: number }[] = [];
    let cur = 0;
    for (const c of project.clips) {
      if (c.start > cur) g.push({ start: cur, end: c.start });
      cur = c.end;
    }
    if (cur < dur) g.push({ start: cur, end: dur });
    return g;
  }, [project.clips, dur]);

  // Waveform: real peaks from the mic (else system) WAV via a worker; demo peaks in the browser.
  const audioTrack = project.media.mic ?? project.media.systemAudio;
  const [wavePeaks, setWavePeaks] = useState<{ peaks: Peaks; offsetMs: number; real: boolean } | null>(null);
  useEffect(() => {
    let alive = true;
    if (audioTrack?.url) {
      loadPeaks(audioTrack.url).then(
        (pk) => alive && setWavePeaks({ peaks: pk, offsetMs: audioTrack.offsetMs, real: true }),
        () => alive && setWavePeaks(null),
      );
    } else if (audioTrack) setWavePeaks({ peaks: demoPeaks(audioTrack.durationMs), offsetMs: audioTrack.offsetMs, real: false });
    else setWavePeaks(null);
    return () => {
      alive = false;
    };
  }, [audioTrack]);

  const wave = useMemo(() => {
    if (!wavePeaks) return "";
    const { peaks, bucketMs } = wavePeaks.peaks;
    const bars = Math.max(10, Math.floor(width / 3));
    let d = "";
    for (let i = 0; i < bars; i++) {
      const t0 = ((i * 3) / pps) * 1000 - wavePeaks.offsetMs;
      const t1 = (((i + 1) * 3) / pps) * 1000 - wavePeaks.offsetMs;
      const a = Math.max(0, Math.floor(t0 / bucketMs));
      const b = Math.min(peaks.length, Math.ceil(t1 / bucketMs));
      let m = 0;
      for (let k = a; k < b; k++) if (peaks[k] > m) m = peaks[k];
      if (b <= a) continue;
      const h = Math.max(0.6, Math.sqrt(m) * 11);
      d += `M${i * 3 + 1} ${12 - h}V${12 + h}`;
    }
    return d;
  }, [wavePeaks, width, pps]);

  const selectedZoom = selection?.kind === "zoom";
  const downClicks = useMemo(() => project.clicks.filter((c) => c.state === "down"), [project.clicks]);

  return (
    <section className="timeline glass" aria-label="Timeline">
      <div className="timeline__head" role="toolbar" aria-label="Timeline tools">
        <button type="button" className="iconbtn" aria-label="Split at playhead" title="Split (S)" onClick={() => ed().splitAtPlayhead()}>
          <Scissors size={16} />
        </button>
        <button type="button" className="iconbtn" aria-label="Add zoom at playhead" title="Add zoom (Z)" onClick={() => ed().addZoomAtPlayhead()}>
          <ZoomIn size={16} />
        </button>
        <button type="button" className="iconbtn" aria-label="Delete selection" title="Delete (Del)" disabled={!selection} onClick={() => ed().deleteSelection()}>
          <Trash2 size={16} />
        </button>
        <span className="toolbar__sep" aria-hidden="true" />
        <button type="button" className="iconbtn" aria-label="Snapping" aria-pressed={snap} title="Snap (hold Alt to bypass)" onClick={() => ed().setSnap(!snap)}>
          <Magnet size={16} />
        </button>
        <span className="field__hint timeline__hint" style={{ marginLeft: 6 }}>
          {selectedZoom ? "Zoom selected (click preview: focus)" : selection?.kind === "clip" ? "Clip selected" : `${project.clips.length} clip${project.clips.length > 1 ? "s" : ""} · ${project.zoomSegments.length} zooms`}
        </span>
        <span className="spacer" />
        <button type="button" className="iconbtn" aria-label="Zoom timeline out" onClick={() => ed().setPxPerSec(pps / 1.4)}>
          <Minus size={16} />
        </button>
        <input
          className="range timeline__zoom"
          type="range"
          aria-label="Timeline zoom"
          min={Math.log(8)}
          max={Math.log(400)}
          step={0.01}
          value={Math.log(pps)}
          style={{ ["--p" as string]: `${((Math.log(pps) - Math.log(8)) / (Math.log(400) - Math.log(8))) * 100}%` }}
          onChange={(e) => ed().setPxPerSec(Math.exp(Number(e.target.value)))}
        />
        <button type="button" className="iconbtn" aria-label="Zoom timeline in" onClick={() => ed().setPxPerSec(pps * 1.4)}>
          <Plus size={16} />
        </button>
        <button
          type="button"
          className="iconbtn"
          aria-label="Fit timeline"
          title="Fit"
          onClick={() => {
            const el = scrollRef.current;
            if (el) ed().setPxPerSec((el.clientWidth - 40) / (dur / 1000));
          }}
        >
          <Maximize2 size={15} />
        </button>
      </div>
      <div className="timeline__body">
        <div className="timeline__labels" aria-hidden="true">
          <div className="tracklabel">
            <Film size={14} /> Video
          </div>
          <div className="tracklabel">
            <ZoomIn size={14} /> Zoom
          </div>
          <div className="tracklabel tracklabel--sm">
            <MousePointerClick size={14} /> Input
          </div>
          <div className="tracklabel">
            <AudioLines size={14} /> Audio
          </div>
        </div>
        <div className="timeline__scroll scroll" ref={scrollRef}>
          <div
            className="timeline__content"
            ref={contentRef}
            style={{ width }}
            onPointerMove={onMove}
            onPointerUp={onUp}
            onPointerCancel={onUp}
          >
            <div className="ruler" onPointerDown={(e) => startDrag(e, { kind: "scrub" })} role="presentation">
              {ticks.map((t) => (
                <div key={t.t} className={`ruler__tick${t.major ? "" : " ruler__tick--minor"}`} style={{ left: msToPx(t.t) }}>
                  {t.major ? fmtTime(t.t) : ""}
                </div>
              ))}
            </div>

            {/* Video track */}
            <div className="track" onPointerDown={(e) => { startDrag(e, { kind: "scrub" }); ed().select(null); }} role="group" aria-label="Video clips">
              {gaps.map((g, i) => (
                <div key={i} className="gap" style={{ left: msToPx(g.start), width: msToPx(g.end - g.start) }} />
              ))}
              {project.clips.map((c, i) => {
                const sel = selection?.kind === "clip" && selection.id === c.id;
                return (
                  <Fragment key={c.id}>
                  <div
                    className="clip"
                    role="button"
                    aria-pressed={sel}
                    tabIndex={0}
                    aria-label={`Clip ${i + 1}, ${fmtTime(c.start)} to ${fmtTime(c.end)}`}
                    style={{ left: msToPx(c.start), width: Math.max(4, msToPx(c.end - c.start)) }}
                    onPointerDown={(e) => {
                      ed().select({ kind: "clip", id: c.id });
                      startDrag(e, { kind: "press", x0: e.clientX });
                    }}
                    onFocus={() => ed().select({ kind: "clip", id: c.id })}
                  >
                    <div className="clip__frames" />
                    <span className="clip__label">Clip {i + 1}</span>
                  </div>
                    <div
                      className="handle handle--l handle--clip"
                      style={{ left: msToPx(c.start) }}
                      role="slider"
                      tabIndex={0}
                      aria-label={`Trim start of clip ${i + 1}`}
                      aria-valuemin={0}
                      aria-valuemax={dur}
                      aria-valuenow={c.start}
                      aria-valuetext={fmtTime(c.start, true)}
                      onPointerDown={(e) => {
                        ed().select({ kind: "clip", id: c.id });
                        startDrag(e, { kind: "clip-l", id: c.id, x0: e.clientX, start: c.start, end: c.end });
                      }}
                      onKeyDown={(e) => clipKey(e, c.id, "l")}
                    />
                    <div
                      className="handle handle--r handle--clip"
                      style={{ left: msToPx(c.end) - 10 }}
                      role="slider"
                      tabIndex={0}
                      aria-label={`Trim end of clip ${i + 1}`}
                      aria-valuemin={0}
                      aria-valuemax={dur}
                      aria-valuenow={c.end}
                      aria-valuetext={fmtTime(c.end, true)}
                      onPointerDown={(e) => {
                        ed().select({ kind: "clip", id: c.id });
                        startDrag(e, { kind: "clip-r", id: c.id, x0: e.clientX, start: c.start, end: c.end });
                      }}
                      onKeyDown={(e) => clipKey(e, c.id, "r")}
                    />
                  </Fragment>
                );
              })}
            </div>

            {/* Zoom track */}
            <div
              className="track"
              role="group"
              aria-label="Zoom segments. Double-click empty space to add."
              onPointerDown={(e) => {
                startDrag(e, { kind: "scrub" });
                ed().select(null);
              }}
              onDoubleClick={(e) => {
                ed().setPlayhead(xToMs(e.clientX));
                ed().addZoomAtPlayhead();
              }}
            >
              {project.zoomSegments.map((z) => {
                const sel = selection?.kind === "zoom" && selection.id === z.id;
                const w = msToPx(z.end - z.start);
                return (
                  <div
                    key={z.id}
                    className="zseg"
                    role="button"
                    tabIndex={0}
                    aria-pressed={sel}
                    aria-label={`Zoom ${z.scale.toFixed(1)}x from ${fmtTime(z.start)} to ${fmtTime(z.end)}. Arrow keys move, Alt+arrows resize.`}
                    style={{ left: msToPx(z.start), width: Math.max(6, w) }}
                    onPointerDown={(e) => {
                      ed().select({ kind: "zoom", id: z.id });
                      startDrag(e, { kind: "zoom-move", id: z.id, x0: e.clientX, start: z.start, end: z.end });
                    }}
                    onDoubleClick={(e) => e.stopPropagation()}
                    onFocus={() => ed().select({ kind: "zoom", id: z.id })}
                    onKeyDown={(e) => zoomKey(e, z.id)}
                  >
                    <div
                      className="handle handle--l"
                      aria-hidden="true"
                      onPointerDown={(e) => {
                        ed().select({ kind: "zoom", id: z.id });
                        startDrag(e, { kind: "zoom-l", id: z.id, x0: e.clientX, start: z.start, end: z.end });
                      }}
                    />
                    {w > 50 && (
                      <span>
                        {z.scale.toFixed(1)}×{z.auto && w > 90 ? " · auto" : ""}
                      </span>
                    )}
                    <div
                      className="handle handle--r"
                      aria-hidden="true"
                      onPointerDown={(e) => {
                        ed().select({ kind: "zoom", id: z.id });
                        startDrag(e, { kind: "zoom-r", id: z.id, x0: e.clientX, start: z.start, end: z.end });
                      }}
                    />
                  </div>
                );
              })}
            </div>

            {/* Clicks / keys */}
            <div className="track track--sm" aria-label={`${downClicks.length} clicks, ${project.keys.length} key presses`} role="img">
              {downClicks.map((c, i) => (
                <span key={i} className={`marker${c.button === "right" ? " marker--right" : ""}`} style={{ left: msToPx(c.t) }} />
              ))}
              {pps > 25 &&
                project.keys.map((k, i) => (
                  <span key={`k${i}`} className="marker marker--key" style={{ left: msToPx(k.t) }}>
                    {k.modifiers.ctrl ? "⌃" : ""}
                    {k.label.length > 3 ? k.label.slice(0, 3) : k.label}
                  </span>
                ))}
            </div>

            {/* Audio (placeholder waveform) */}
            <div
              className="track"
              role="img"
              aria-label={!audioTrack ? "No audio track" : wavePeaks?.real ? "Audio waveform" : "Audio waveform (demo data)"}
            >
              {!audioTrack && <span className="track__empty">No audio recorded</span>}
              <svg className="wave" width={width} height={32} viewBox={`0 0 ${width} 24`} preserveAspectRatio="none" aria-hidden="true">
                <path d={wave} stroke="var(--accent-line)" strokeWidth="1.6" strokeLinecap="round" opacity="0.75" />
              </svg>
            </div>

            {snapLine !== null && <div className="snapline" style={{ left: msToPx(snapLine) }} />}
            <div className="playhead" style={{ left: msToPx(playhead) }}>
              <div
                className="playhead__knob"
                onPointerDown={(e) => startDrag(e, { kind: "scrub" })}
                role="slider"
                tabIndex={0}
                aria-label="Playhead"
                aria-valuemin={0}
                aria-valuemax={dur}
                aria-valuenow={Math.round(playhead)}
                aria-valuetext={fmtTime(playhead, true)}
                onKeyDown={(e) => {
                  if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
                  e.preventDefault();
                  e.stopPropagation();
                  ed().setPlayhead(playhead + (e.shiftKey ? 1000 : 100) * (e.key === "ArrowLeft" ? -1 : 1));
                }}
              />
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
