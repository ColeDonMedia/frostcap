import { useRef, type KeyboardEvent, type PointerEvent as RPointerEvent } from "react";
import { Gauge } from "lucide-react";
import { useEditor } from "../../store/editor";
import { clamp, fmtTime } from "../../lib/format";
import { t, type MessageKey } from "../../i18n";

const MIN_MS = 100;

interface BarProps {
  id: string;
  kind: "annotation" | "speed";
  start: number;
  end: number;
  label: string;
  aria: string;
  className: string;
  pps: number;
  dur: number;
  /** Persist new edges (history is merged into one gesture by the caller). */
  onEdges: (start: number, end: number) => void;
  /** Keyboard edits (own undo step per burst). */
  onKeyEdges: (start: number, end: number, key: string) => void;
}

/** A draggable, resizable, keyboard-operable bar on a range track. */
function Bar({ id, kind, start, end, label, aria, className, pps, dur, onEdges, onKeyEdges }: BarProps) {
  const selected = useEditor((s) => s.selection?.kind === kind && s.selection.id === id);
  const drag = useRef<{ mode: "move" | "l" | "r"; x0: number; start: number; end: number } | null>(null);
  const ed = useEditor.getState;
  const msToPx = (ms: number) => (ms / 1000) * pps;

  const down = (e: RPointerEvent, mode: "move" | "l" | "r") => {
    e.stopPropagation();
    (e.currentTarget as Element).setPointerCapture?.(e.pointerId);
    ed().select({ kind, id });
    if (kind === "annotation" && (ed().playhead < start || ed().playhead >= end)) ed().setPlayhead(start);
    ed().beginGesture();
    drag.current = { mode, x0: e.clientX, start, end };
  };
  const move = (e: RPointerEvent) => {
    const d = drag.current;
    if (!d) return;
    const dms = ((e.clientX - d.x0) / pps) * 1000;
    if (d.mode === "move") {
      const len = d.end - d.start;
      const s = clamp(d.start + dms, 0, dur - len);
      onEdges(Math.round(s), Math.round(s + len));
    } else if (d.mode === "l") onEdges(Math.round(clamp(d.start + dms, 0, d.end - MIN_MS)), d.end);
    else onEdges(d.start, Math.round(clamp(d.end + dms, d.start + MIN_MS, dur)));
  };
  const up = () => {
    if (drag.current) ed().endGesture();
    drag.current = null;
  };

  const key = (e: KeyboardEvent) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      e.stopPropagation();
      ed().select({ kind, id });
      // Selecting from the timeline brings the playhead into the range so the preview shows it.
      if (ed().playhead < start || ed().playhead >= end) ed().setPlayhead(start);
      return;
    }
    if (e.key === "Delete" || e.key === "Backspace") {
      e.preventDefault();
      e.stopPropagation();
      ed().select({ kind, id });
      ed().deleteSelection();
      return;
    }
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    e.stopPropagation();
    const d = (e.shiftKey ? 1000 : 100) * (e.key === "ArrowLeft" ? -1 : 1);
    if (e.altKey) onKeyEdges(start, clamp(end + d, start + MIN_MS, dur), e.key);
    else {
      const len = end - start;
      const s = clamp(start + d, 0, dur - len);
      onKeyEdges(s, s + len, e.key);
    }
  };

  const w = msToPx(end - start);
  return (
    <div
      className={`rbar ${className}`}
      role="button"
      tabIndex={0}
      aria-pressed={selected}
      aria-label={aria}
      style={{ left: msToPx(start), width: Math.max(8, w) }}
      onPointerDown={(e) => down(e, "move")}
      onPointerMove={move}
      onPointerUp={up}
      onPointerCancel={up}
      aria-keyshortcuts="ArrowLeft ArrowRight Alt+ArrowLeft Alt+ArrowRight Delete Enter"
      onKeyDown={key}
      onDoubleClick={(e) => e.stopPropagation()}
    >
      <div className="handle handle--l" aria-hidden="true" onPointerDown={(e) => down(e, "l")} onPointerMove={move} onPointerUp={up} />
      {w > 36 && <span>{label}</span>}
      <div className="handle handle--r" aria-hidden="true" onPointerDown={(e) => down(e, "r")} onPointerMove={move} onPointerUp={up} />
    </div>
  );
}

export function AnnotationTrack({ pps, dur }: { pps: number; dur: number }) {
  const list = useEditor((s) => s.project!.annotations);
  const ed = useEditor.getState;
  return (
    <div className="track track--sm" role="group" aria-label={t("timeline.annotations")} data-testid="track-annotations" onPointerDown={() => ed().select(null)}>
      {list.map((a) => (
        <Bar
          key={a.id}
          id={a.id}
          kind="annotation"
          start={a.start}
          end={a.end}
          label={t(`ann.kind.${a.kind}` as MessageKey)}
          aria={t("ann.barLabel", { kind: t(`ann.kind.${a.kind}` as MessageKey), start: fmtTime(a.start), end: fmtTime(a.end) })}
          className="rbar--ann"
          pps={pps}
          dur={dur}
          onEdges={(start, end) => ed().updateAnnotation(a.id, { start, end }, { history: false })}
          onKeyEdges={(start, end) => ed().updateAnnotation(a.id, { start, end }, { coalesce: `annbar.${a.id}` })}
        />
      ))}
    </div>
  );
}

export function SpeedTrack({ pps, dur }: { pps: number; dur: number }) {
  const list = useEditor((s) => s.project!.speed);
  const ed = useEditor.getState;
  return (
    <div className="track track--sm" role="group" aria-label={t("timeline.speedRegions")} data-testid="track-speed" onPointerDown={() => ed().select(null)}>
      {list.length === 0 && (
        <button type="button" className="track__empty track__empty--btn" data-testid="speed-empty-add" onPointerDown={(e) => e.stopPropagation()} onClick={() => ed().addSpeedAtPlayhead(2)}>
          <Gauge size={13} aria-hidden="true" /> {t("speed.emptyTrack")}
        </button>
      )}
      {list.map((s) => (
        <Bar
          key={s.id}
          id={s.id}
          kind="speed"
          start={s.start}
          end={s.end}
          label={`${s.speed}×`}
          aria={t("timeline.speedSeg", { speed: s.speed, start: fmtTime(s.start), end: fmtTime(s.end) })}
          className="rbar--speed"
          pps={pps}
          dur={dur}
          onEdges={(start, end) => ed().updateSpeed(s.id, { start, end }, { history: false })}
          onKeyEdges={(start, end) => ed().updateSpeed(s.id, { start, end }, { coalesce: `spdbar.${s.id}` })}
        />
      ))}
    </div>
  );
}
