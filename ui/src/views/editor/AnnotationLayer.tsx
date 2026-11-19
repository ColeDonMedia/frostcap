import { useEffect, useRef, type KeyboardEvent } from "react";
import { useEditor } from "../../store/editor";
import { contentToCanvas } from "../../lib/render";
import { applyDrag, handlesOf, type Handle } from "../../lib/annotations";
import { t, type MessageKey } from "../../i18n";
import type { Annotation, AnnotationKind } from "../../types";

export interface Ghost {
  kind: AnnotationKind;
  p1: { x: number; y: number };
  p2: { x: number; y: number };
}

/**
 * Visual layer over the preview canvas: the selected annotation's outline and handles, the rubber band while
 * drawing, and the inline text editor. Pointer input is handled by the preview canvas itself. The outline group is
 * focusable: arrows move (Shift: faster), Alt+arrows resize, Enter edits text, Ctrl+D duplicates, Delete removes.
 */
export function AnnotationLayer({ w, h, ghost }: { w: number; h: number; ghost: Ghost | null }) {
  const project = useEditor((s) => s.project);
  const playhead = useEditor((s) => s.playhead);
  const selection = useEditor((s) => s.selection);
  const editing = useEditor((s) => s.textEdit);
  const setEditing = useEditor((s) => s.setTextEdit);
  const ed = useEditor.getState;
  const groupRef = useRef<SVGGElement>(null);

  const a = project && selection?.kind === "annotation" ? project.annotations.find((x) => x.id === selection.id) : undefined;
  const active = !!a && playhead >= a.start && playhead < a.end;

  useEffect(() => {
    if (!a) setEditing(null);
  }, [a, setEditing]);

  if (!project) return null;
  const map = (x: number, y: number) => contentToCanvas(x, y, w, h, project, playhead);

  const onKey = (e: KeyboardEvent<SVGGElement>) => {
    if (!a) return;
    if (e.key === "Delete" || e.key === "Backspace") {
      e.preventDefault();
      e.stopPropagation();
      ed().deleteSelection();
      return;
    }
    if (e.key === "Enter" && a.kind === "text") {
      e.preventDefault();
      e.stopPropagation();
      setEditing(a.id);
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "d") {
      e.preventDefault();
      e.stopPropagation();
      ed().duplicateSelection();
      return;
    }
    const dir = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[e.key];
    if (!dir) return;
    e.preventDefault();
    e.stopPropagation();
    const step = e.shiftKey ? 0.05 : 0.005;
    const handle: Handle = e.altKey ? (a.kind === "arrow" ? "end" : "se") : "body";
    const n = applyDrag(a, handle, dir[0] * step, dir[1] * step);
    ed().updateAnnotation(a.id, { x: n.x, y: n.y, w: n.w, h: n.h, x2: n.x2, y2: n.y2 }, { coalesce: `annk.${a.id}` });
  };

  const box = a && a.kind !== "arrow" ? { tl: map(a.x, a.y), br: map(a.x + a.w, a.y + a.h) } : null;
  const kindName = (k: AnnotationKind) => t(`ann.kind.${k}` as MessageKey);

  return (
    <>
      <svg className="annot-layer" width={w} height={h} viewBox={`0 0 ${w} ${h}`} aria-hidden={a ? undefined : true}>
        {ghost && <GhostShape g={ghost} map={map} />}
        {a && active && (
          <g ref={groupRef} role="group" tabIndex={0} aria-label={t("ann.preview", { kind: kindName(a.kind) })} onKeyDown={onKey} className="annot-sel">
            <Outline a={a} map={map} />
            {handlesOf(a).map((hd) => {
              const p = map(hd.x, hd.y);
              return <circle key={hd.handle} cx={p.x} cy={p.y} r={5} className="annot-handle" />;
            })}
          </g>
        )}
      </svg>
      {a && editing === a.id && box && (
        <textarea
          className="annot-text-edit"
          aria-label={t("ann.textEdit")}
          autoFocus
          defaultValue={a.text}
          maxLength={500}
          style={{ left: box.tl.x, top: box.tl.y, width: Math.max(80, box.br.x - box.tl.x), height: Math.max(36, box.br.y - box.tl.y) }}
          onBlur={(e) => {
            ed().updateAnnotation(a.id, { text: e.currentTarget.value }, { coalesce: `anntext.${a.id}` });
            setEditing(null);
            groupRef.current?.focus();
          }}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Escape") setEditing(null);
            else if (e.key === "Enter" && !e.shiftKey) e.currentTarget.blur();
          }}
        />
      )}
    </>
  );
}

type Map = (x: number, y: number) => { x: number; y: number };

function Outline({ a, map }: { a: Annotation; map: Map }) {
  if (a.kind === "arrow") {
    const s = map(a.x, a.y);
    const e = map(a.x2, a.y2);
    return <line x1={s.x} y1={s.y} x2={e.x} y2={e.y} className="annot-outline" />;
  }
  const tl = map(a.x, a.y);
  const br = map(a.x + a.w, a.y + a.h);
  return <rect x={tl.x} y={tl.y} width={br.x - tl.x} height={br.y - tl.y} className="annot-outline" />;
}

function GhostShape({ g, map }: { g: Ghost; map: Map }) {
  const a = map(g.p1.x, g.p1.y);
  const b = map(g.p2.x, g.p2.y);
  if (g.kind === "arrow") return <line x1={a.x} y1={a.y} x2={b.x} y2={b.y} className="annot-ghost" />;
  return <rect x={Math.min(a.x, b.x)} y={Math.min(a.y, b.y)} width={Math.abs(b.x - a.x)} height={Math.abs(b.y - a.y)} className="annot-ghost" />;
}
