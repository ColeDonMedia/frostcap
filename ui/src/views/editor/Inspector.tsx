import { useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { ZoomIn, MousePointer2, AppWindow, Camera, AudioLines, Captions, Upload, Wand2, Trash2, Circle, Square, RectangleHorizontal, MoveUpRight, Type, Highlighter, Focus, Droplets, Grid3x3, Gauge, Pipette, Copy, Shapes } from "lucide-react";
import { useEditor } from "../../store/editor";
import { useApp } from "../../store/app";
import { Segmented, Select, Slider, ToggleRow } from "../../components/ui";
import { slideStyle } from "../../lib/indicator";
import { EASINGS, GRADIENTS, ZOOM_PRESETS, gradientCss } from "../../lib/presets";
import { backend, formatBackground, isTauri, joinPath } from "../../backend";
import { visibleAnnotations } from "../../lib/annotations";
import { ANNOTATION_KINDS } from "../../lib/annotations";
import { parseSubtitles } from "../../lib/captions";
import { fmtTime } from "../../lib/format";
import type { Annotation, AnnotationKind, Corner, Easing, WebcamBackgroundKind, WebcamShape, WindowFrame, ZoomPreset } from "../../types";
import { t, type MessageKey } from "../../i18n";

type TabId = "zoom" | "annotate" | "cursor" | "frame" | "webcam" | "audio" | "captions";
const TABS: { id: TabId; label: MessageKey; icon: typeof ZoomIn }[] = [
  { id: "zoom", label: "tab.zoom", icon: ZoomIn },
  { id: "annotate", label: "tab.annotate", icon: Shapes },
  { id: "cursor", label: "tab.cursor", icon: MousePointer2 },
  { id: "frame", label: "tab.frame", icon: AppWindow },
  { id: "webcam", label: "tab.webcam", icon: Camera },
  { id: "audio", label: "tab.audio", icon: AudioLines },
  { id: "captions", label: "tab.captions", icon: Captions },
];

export function Inspector() {
  const [tab, setTab] = useState<TabId>("zoom");
  const tabsRef = useRef<HTMLDivElement>(null);

  const onKey = (e: KeyboardEvent) => {
    const i = TABS.findIndex((t) => t.id === tab);
    let n = i;
    if (e.key === "ArrowRight") n = (i + 1) % TABS.length;
    else if (e.key === "ArrowLeft") n = (i - 1 + TABS.length) % TABS.length;
    else if (e.key === "Home") n = 0;
    else if (e.key === "End") n = TABS.length - 1;
    else return;
    e.preventDefault();
    e.stopPropagation();
    setTab(TABS[n].id);
    tabsRef.current?.querySelectorAll<HTMLButtonElement>("[role=tab]")[n]?.focus();
  };

  return (
    <aside className="inspector glass" aria-label={t("inspector.label")}>
      <div className="tabs" role="tablist" aria-label={t("inspector.sections")} ref={tabsRef} onKeyDown={onKey}>
        <span className="tab__bg" aria-hidden="true" style={slideStyle(TABS.findIndex((t) => t.id === tab), TABS.length, { padPx: 8, gapPx: 2 })} />
        {TABS.map((tb) => {
          const Icon = tb.icon;
          const sel = tb.id === tab;
          return (
            <button
              key={tb.id}
              type="button"
              role="tab"
              id={`tab-${tb.id}`}
              aria-selected={sel}
              aria-controls={sel ? `panel-${tb.id}` : undefined}
              tabIndex={sel ? 0 : -1}
              className="tab"
              data-tip={t(tb.label)}
              onClick={() => setTab(tb.id)}
            >
              <Icon size={16} aria-hidden="true" />
              <span className="sr-only">{t(tb.label)}</span>
            </button>
          );
        })}
      </div>
      <div className="tabpanel scroll" role="tabpanel" id={`panel-${tab}`} aria-labelledby={`tab-${tab}`} tabIndex={0}>
        <div className="tabpanel__title" aria-hidden="true" data-testid="tab-title">
          {t(TABS.find((x) => x.id === tab)!.label)}
        </div>
        {tab === "zoom" && <ZoomPanel />}
        {tab === "annotate" && <AnnotatePanel />}
        {tab === "cursor" && <CursorPanel />}
        {tab === "frame" && <FramePanel />}
        {tab === "webcam" && <WebcamPanel />}
        {tab === "audio" && <AudioPanel />}
        {tab === "captions" && <CaptionsPanel />}
      </div>
    </aside>
  );
}

function Group({ title, children, action }: { title: string; children: ReactNode; action?: ReactNode }) {
  return (
    <section className="group" aria-label={title}>
      <h3 className="group__title">
        {title}
        {action}
      </h3>
      {children}
    </section>
  );
}

const pct = (v: number) => `${Math.round(v * 100)}%`;
const easingOptions = () => EASINGS.map((e) => ({ value: e, label: t(`easing.${e}`) }));


// ------------------------------------------------------------ Annotate

const TOOL_ICON: Record<AnnotationKind, typeof Type> = { arrow: MoveUpRight, text: Type, rect: Square, ellipse: Circle, highlight: Highlighter, spotlight: Focus, blur: Droplets, pixelate: Grid3x3 };

function ColorField({ label, value, onChange }: { label: string; value: string; onChange: (v: string) => void }) {
  const id = useId();
  return (
    <div className="field">
      <label className="field__label" htmlFor={id}>
        {label}
      </label>
      <div className="colorfield">
        <input id={id} type="color" value={/^#[0-9a-fA-F]{6}$/.test(value) ? value : "#000000"} onChange={(e) => onChange(e.target.value.toLowerCase())} />
        <span className="field__hint" style={{ fontFamily: "var(--mono)" }}>
          {value}
        </span>
      </div>
    </div>
  );
}

function AnnotatePanel() {
  const tool = useEditor((s) => s.tool);
  const a = useEditor((s) => (s.selection?.kind === "annotation" ? s.project!.annotations.find((x) => x.id === s.selection!.id) : undefined));
  const ed = useEditor.getState;
  const set = (patch: Partial<Annotation>) => a && ed().updateAnnotation(a.id, patch);
  const box = a && a.kind !== "arrow";
  const showing = useEditor((s) => visibleAnnotations(s.project!.annotations, s.playhead).length);
  return (
    <>
      <Group title={t("ann.tools")}>
        <div className="tool-grid" role="radiogroup" aria-label={t("ann.tools")}>
          {ANNOTATION_KINDS.map((k) => {
            const Icon = TOOL_ICON[k];
            return (
              <button key={k} type="button" role="radio" aria-checked={tool === k} className="tool" data-testid={`tool-${k}`} onClick={() => ed().setTool(tool === k ? null : k)}>
                <Icon size={18} aria-hidden="true" />
                {t(`ann.kind.${k}` as MessageKey)}
              </button>
            );
          })}
        </div>
        <p className="field__hint" style={{ margin: 0 }}>
          {t("ann.toolHint")}
        </p>
        {showing >= 8 && (
          <p className="field__hint" role="status" data-testid="ann-overlap" style={{ margin: 0 }}>
            {t("ann.overlapNote")}
          </p>
        )}
      </Group>
      <Group
        title={a ? t("ann.selected") : t("timeline.annotations")}
        action={
          a && (
            <span style={{ display: "inline-flex", gap: 4 }}>
              <button type="button" className="iconbtn" style={{ width: 26, height: 26 }} aria-label={t("ann.duplicate")} title={t("ann.duplicate")} onClick={() => ed().duplicateSelection()}>
                <Copy size={14} />
              </button>
              <button type="button" className="iconbtn" style={{ width: 26, height: 26 }} aria-label={t("ann.delete")} title={t("ann.delete")} onClick={() => ed().deleteSelection()}>
                <Trash2 size={14} />
              </button>
            </span>
          )
        }
      >
        {a ? (
          <>
            <div className="field__hint">{t("ann.range", { start: fmtTime(a.start, true), end: fmtTime(a.end, true) })}</div>
            {a.kind === "text" && (
              <div className="field">
                <label className="field__label" htmlFor="ann-text">
                  {t("ann.text")}
                </label>
                <textarea id="ann-text" className="input" rows={3} maxLength={500} value={a.text} onChange={(e) => set({ text: e.target.value })} />
              </div>
            )}
            {a.kind !== "blur" && a.kind !== "pixelate" && a.kind !== "spotlight" && <ColorField label={t("ann.color")} value={a.color} onChange={(v) => set({ color: v })} />}
            {(a.kind === "arrow" || a.kind === "rect" || a.kind === "ellipse") && (
              <Slider label={t("ann.stroke")} value={a.strokeWidth} min={1} max={40} step={1} format={(v) => `${v} px`} onChange={(v) => set({ strokeWidth: v })} />
            )}
            {(a.kind === "rect" || a.kind === "ellipse") && <ToggleRow label={t("ann.fill")} checked={a.fill} onChange={(v) => set({ fill: v })} />}
            {a.kind === "text" && <Slider label={t("ann.fontSize")} value={a.fontSize} min={8} max={200} step={1} format={(v) => `${v} px`} onChange={(v) => set({ fontSize: v })} />}
            {(a.kind === "blur" || a.kind === "pixelate") && <Slider label={t("ann.strength")} value={a.strength} min={0} max={1} step={0.01} format={pct} onChange={(v) => set({ strength: v })} />}
            {a.kind === "spotlight" && <Slider label={t("ann.dim")} value={a.dim} min={0} max={1} step={0.01} format={pct} onChange={(v) => set({ dim: v })} />}
            {box && (a.kind === "rect" || a.kind === "highlight" || a.kind === "spotlight") && (
              <Slider label={t("ann.corner")} value={a.cornerRadius} min={0} max={200} step={1} format={(v) => `${v} px`} onChange={(v) => set({ cornerRadius: v })} />
            )}
            <Slider label={t("ann.opacity")} value={a.opacity} min={0} max={1} step={0.01} format={pct} onChange={(v) => set({ opacity: v })} />
            <Slider label={t("ann.fade")} value={a.fadeMs} min={0} max={2000} step={50} format={(v) => `${v} ms`} onChange={(v) => set({ fadeMs: v })} />
            <p className="field__hint" style={{ margin: 0 }} data-testid="ann-shortcuts">
              {t("ann.shortcuts")}
            </p>
            <p className="field__hint" style={{ margin: 0 }}>
              {t("ann.drawnNote")}
            </p>
          </>
        ) : (
          <div className="empty-note">{t("ann.none")}</div>
        )}
      </Group>
    </>
  );
}

// --------------------------------------------------------------- Speed

const SPEED_PRESETS = ["0.5", "1", "1.5", "2", "3", "4"];

function SpeedGroup() {
  const seg = useEditor((s) => (s.selection?.kind === "speed" ? s.project!.speed.find((x) => x.id === s.selection!.id) : undefined));
  const ed = useEditor.getState;
  return (
    <Group
      title={t("speed.group")}
      action={
        seg && (
          <button type="button" className="iconbtn" style={{ width: 26, height: 26 }} aria-label={t("editor.speedDeleted")} title={t("editor.speedDeleted")} onClick={() => ed().deleteSelection()}>
            <Trash2 size={14} />
          </button>
        )
      }
    >
      {seg ? (
        <>
          <div className="field__hint">{t("ann.range", { start: fmtTime(seg.start, true), end: fmtTime(seg.end, true) })}</div>
          <Slider label={t("speed.value")} value={seg.speed} min={0.25} max={4} step={0.05} format={(v) => `${v.toFixed(2)}×`} onChange={(v) => ed().updateSpeed(seg.id, { speed: v })} />
          <Segmented<string>
            label={t("speed.presets")}
            value={SPEED_PRESETS.includes(String(seg.speed)) ? String(seg.speed) : ""}
            onChange={(v) => ed().updateSpeed(seg.id, { speed: Number(v) })}
            options={SPEED_PRESETS.map((v) => ({ value: v, label: `${v}×` }))}
          />
        </>
      ) : (
        <div className="empty-note">{t("speed.empty")}</div>
      )}
      <button
        type="button"
        className="btn btn--sm"
        onClick={() => {
          if (!ed().addSpeedAtPlayhead(2)) useApp.getState().showToast(t("speed.overlap"), "warning");
        }}
      >
        <Gauge size={13} aria-hidden="true" /> {t("speed.add")}
      </button>
    </Group>
  );
}

// ------------------------------------------------- image picking (shells)

/** Small preview of a project image (asset protocol, Tauri only); a neutral tile in the browser demo. */
function ImageThumb({ file }: { file: string }) {
  const dir = useEditor((s) => s.project!.media.dir);
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    if (!isTauri) return;
    void backend.assetUrl(joinPath(dir, file)).then((u) => alive && setUrl(u), () => undefined);
    return () => {
      alive = false;
    };
  }, [dir, file]);
  return url ? <img className="imgthumb" src={url} alt="" /> : <span className="imgthumb imgthumb--empty" aria-hidden="true" />;
}

/** Native image dialog, then the backend copies it into the project; returns the project-relative basename. */
async function chooseProjectImage(onError: string): Promise<string | null> {
  const path = useEditor.getState().path;
  if (!path) return null;
  try {
    const src = await backend.pickImage();
    if (!src) return null;
    return await backend.importBackground(path, src);
  } catch (e) {
    useApp.getState().reportIssue(e, onError);
    return null;
  }
}

function WebcamEffects() {
  const w = useEditor((s) => s.project!.style.webcam);
  const eyedropper = useEditor((s) => s.eyedropper);
  const ed = useEditor.getState;
  // Leaving the Webcam tab ends the sampling mode.
  useEffect(() => () => useEditor.getState().setEyedropper(false), []);
  const set = (patch: Partial<typeof w>) => ed().setStyle("webcam", patch);
  const setKey = (patch: Partial<typeof w.chromaKey>) => set({ chromaKey: { ...w.chromaKey, ...patch } });
  const setBg = (patch: Partial<typeof w.background>) => set({ background: { ...w.background, ...patch } });
  const keyOn = w.chromaKey.enabled;
  const bg = w.background;
  const [busy, setBusy] = useState(false);
  return (
    <>
      <Group title={t("webcam.look")}>
        <Slider label={t("webcam.margin")} value={w.margin} min={0} max={0.2} step={0.001} format={(v) => `${(v * 100).toFixed(1)}%`} onChange={(v) => set({ margin: v })} />
        <ToggleRow label={t("webcam.mirror")} checked={w.mirror} onChange={(v) => set({ mirror: v })} />
        <Slider label={t("webcam.borderWidth")} value={w.borderWidth} min={0} max={20} step={0.5} format={(v) => `${v} px`} onChange={(v) => set({ borderWidth: v })} />
        <ColorField label={t("webcam.borderColor")} value={w.borderColor} onChange={(v) => set({ borderColor: v })} />
        <Slider label={t("webcam.brightness")} value={w.brightness} min={-1} max={1} step={0.01} format={(v) => v.toFixed(2)} onChange={(v) => set({ brightness: v })} />
        <Slider label={t("webcam.contrast")} value={w.contrast} min={0} max={2} step={0.01} format={(v) => v.toFixed(2)} onChange={(v) => set({ contrast: v })} />
        <Slider label={t("webcam.saturation")} value={w.saturation} min={0} max={2} step={0.01} format={(v) => v.toFixed(2)} onChange={(v) => set({ saturation: v })} />
        <Slider label={t("webcam.shadow")} value={w.shadow} min={0} max={1} step={0.01} format={pct} onChange={(v) => set({ shadow: v })} />
      </Group>
      <Group title={t("webcam.keyGroup")}>
        <ToggleRow label={t("webcam.keyEnable")} checked={keyOn} onChange={(v) => setKey({ enabled: v })} />
        <ColorField label={t("webcam.keyColor")} value={w.chromaKey.color} onChange={(v) => setKey({ color: v })} />
        <button type="button" className="btn btn--sm" data-testid="eyedropper" aria-pressed={eyedropper} onClick={() => ed().setEyedropper(!eyedropper)}>
          <Pipette size={13} aria-hidden="true" /> {t("webcam.eyedropper")}
        </button>
        {eyedropper && (
          <p className="field__hint" role="status" style={{ margin: 0 }}>
            {t("webcam.eyedropperActive")}
          </p>
        )}
        <Slider label={t("webcam.similarity")} value={w.chromaKey.similarity} min={0} max={1} step={0.01} format={pct} disabled={!keyOn} onChange={(v) => setKey({ similarity: v })} />
        <Slider label={t("webcam.smoothness")} value={w.chromaKey.smoothness} min={0} max={1} step={0.01} format={pct} disabled={!keyOn} onChange={(v) => setKey({ smoothness: v })} />
        <Slider label={t("webcam.spill")} value={w.chromaKey.spill} min={0} max={1} step={0.01} format={pct} disabled={!keyOn} onChange={(v) => setKey({ spill: v })} />
      </Group>
      <Group title={t("webcam.bgGroup")}>
        {!keyOn && (
          <p className="field__hint" style={{ margin: 0 }} data-testid="bg-needs-key">
            {t("webcam.bgNeedsKey")}
          </p>
        )}
        <Select<WebcamBackgroundKind>
          label={t("webcam.bgKind")}
          value={bg.kind}
          disabled={!keyOn}
          onChange={(v) => setBg({ kind: v })}
          options={(["none", "color", "gradient", "image", "blur"] as const).map((k) => ({ value: k, label: t(`webcam.bg.${k}` as MessageKey) }))}
        />
        {bg.kind === "color" && <ColorField label={t("webcam.bgColor")} value={bg.color} onChange={(v) => setBg({ color: v })} />}
        {bg.kind === "gradient" && (
          <Select<string>
            label={t("webcam.bgGradient")}
            value={GRADIENTS.find((g) => formatBackground(g) === bg.gradient)?.id ?? "custom"}
            disabled={!keyOn}
            onChange={(id) => {
              const g = GRADIENTS.find((x) => x.id === id);
              if (g) setBg({ gradient: formatBackground(g) });
            }}
            options={[...GRADIENTS.map((g) => ({ value: g.id, label: t(`gradient.${g.id}` as MessageKey) })), ...(GRADIENTS.some((g) => formatBackground(g) === bg.gradient) ? [] : [{ value: "custom", label: t("webcam.bgGradient") }])]}
          />
        )}
        {bg.kind === "image" && (
          <div style={{ display: "grid", gap: 6 }}>
            <span className="field__hint" style={{ display: "flex", alignItems: "center", gap: 8 }}>
              {bg.image && <ImageThumb file={bg.image} />}
              {bg.image ? t("frame.imageCustom") : t("webcam.bgImageNone")}
            </span>
            <div style={{ display: "flex", gap: 8 }}>
              <button
                type="button"
                className="btn btn--sm"
                disabled={!keyOn || busy}
                aria-busy={busy}
                onClick={async () => {
                  setBusy(true);
                  const file = await chooseProjectImage(t("webcam.errImport"));
                  setBusy(false);
                  if (file) setBg({ image: file });
                }}
              >
                {busy ? t("frame.importing") : t("webcam.bgImage")}
              </button>
              {bg.image && (
                <button
                  type="button"
                  className="btn btn--sm"
                  onClick={() => {
                    const { image: _drop, ...rest } = bg;
                    void _drop;
                    set({ background: rest });
                  }}
                >
                  {t("webcam.bgImageClear")}
                </button>
              )}
            </div>
          </div>
        )}
        {(bg.kind === "blur") && <Slider label={t("webcam.bgBlur")} value={bg.blur} min={0} max={1} step={0.01} format={pct} disabled={!keyOn} onChange={(v) => setBg({ blur: v })} />}
      </Group>
    </>
  );
}

function BackgroundImageGroup() {
  const img = useEditor((s) => s.project!.style.backgroundImage);
  const [busy, setBusy] = useState(false);
  const ed = useEditor.getState;
  const setImg = (patch: Partial<NonNullable<typeof img>>) => img && ed().apply((p) => ({ ...p, style: { ...p.style, backgroundImage: { ...img, ...patch } } }), { coalesce: `bgimg.${Object.keys(patch).join(",")}` });
  return (
    <Group title={t("frame.imageGroup")}>
      <span className="field__hint" style={{ display: "flex", alignItems: "center", gap: 8 }}>
        {img && <ImageThumb file={img.file} />}
        {img ? t("frame.imageCustom") : t("frame.imageNone")}
      </span>
      <div style={{ display: "flex", gap: 8 }}>
        <button
          type="button"
          className="btn btn--sm"
          disabled={busy}
          aria-busy={busy}
          onClick={async () => {
            setBusy(true);
            const file = await chooseProjectImage(t("frame.errImport"));
            setBusy(false);
            if (file) ed().apply((p) => ({ ...p, style: { ...p.style, backgroundImage: { fit: p.style.backgroundImage?.fit ?? "cover", blur: p.style.backgroundImage?.blur ?? 0, file } } }));
          }}
        >
          {busy ? t("frame.importing") : t("frame.imageChoose")}
        </button>
        {img && (
          <button
            type="button"
            className="btn btn--sm"
            onClick={() =>
              ed().apply((p) => {
                const { backgroundImage: _drop, ...rest } = p.style;
                void _drop;
                return { ...p, style: rest };
              })
            }
          >
            {t("frame.imageRemove")}
          </button>
        )}
      </div>
      {img && (
        <>
          <Segmented<"cover" | "contain">
            label={t("frame.imageFit")}
            value={img.fit}
            onChange={(v) => setImg({ fit: v })}
            options={[
              { value: "cover", label: t("frame.fitCover") },
              { value: "contain", label: t("frame.fitContain") },
            ]}
          />
          <Slider label={t("frame.imageBlur")} value={img.blur} min={0} max={100} step={1} format={(v) => `${v} px`} onChange={(v) => setImg({ blur: v })} />
        </>
      )}
    </Group>
  );
}

// ---------------------------------------------------------------- Zoom

function ZoomPanel() {
  const style = useEditor((s) => s.project!.style.zoom);
  const selection = useEditor((s) => s.selection);
  const seg = useEditor((s) => (s.selection?.kind === "zoom" ? s.project!.zoomSegments.find((z) => z.id === s.selection!.id) : undefined));
  const ed = useEditor.getState;

  const setPreset = (preset: ZoomPreset) => {
    const pr = ZOOM_PRESETS[preset];
    ed().setStyle("zoom", { preset, scale: pr.scale, easing: pr.easing, transitionMs: pr.transitionMs }, "zoom.preset");
  };
  const [busy, setBusy] = useState(false);
  const applyAll = () => {
    ed().apply((p) => ({ ...p, zoomSegments: p.zoomSegments.map((z) => ({ ...z, scale: style.scale, easing: style.easing, transitionMs: style.transitionMs })) }));
    ed().notifyUndoable(t("zoom.applied"));
  };
  /** Runs the backend's auto-zoom (clicks, typing, focus changes); keeps manual segments. */
  const regenerate = async () => {
    const p = ed().project;
    if (!p) return;
    setBusy(true);
    try {
      const auto = await backend.generateAutoZoom(p, style.preset);
      ed().apply((cur) => {
        const manual = cur.zoomSegments.filter((z) => !z.auto);
        const free = auto.filter((a) => !manual.some((m) => a.start < m.end && m.start < a.end));
        return { ...cur, zoomSegments: [...manual, ...free].sort((a, b) => a.start - b.start) };
      });
      ed().notifyUndoable(t("zoom.generated", { count: auto.length, preset: t(`zoom.${style.preset}`) }));
    } catch (e) {
      useApp.getState().reportIssue(e, t("zoom.errAuto"));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <Group title={t("zoom.preset")}>
        <Segmented<ZoomPreset>
          label={t("zoom.presetLabel")}
          value={style.preset}
          onChange={setPreset}
          options={(Object.keys(ZOOM_PRESETS) as ZoomPreset[]).map((k) => ({ value: k, label: t(`zoom.${k}`) }))}
        />
        <Slider label={t("zoom.defaultScale")} value={style.scale} min={1.1} max={4} step={0.05} format={(v) => `${v.toFixed(2)}×`} onChange={(v) => ed().setStyle("zoom", { scale: v })} />
        <Select<Easing> label={t("zoom.easing")} value={style.easing} options={easingOptions()} onChange={(v) => ed().setStyle("zoom", { easing: v })} />
        <Slider
          label={t("zoom.transitionDuration")}
          value={style.transitionMs}
          min={100}
          max={1500}
          step={50}
          format={(v) => `${v} ms`}
          onChange={(v) => ed().setStyle("zoom", { transitionMs: v })}
        />
        <div style={{ display: "flex", gap: 8 }}>
          <button type="button" className="btn btn--sm" onClick={applyAll}>
            {t("zoom.applyAll")}
          </button>
          <button type="button" className="btn btn--sm" onClick={() => void regenerate()} disabled={busy}>
            <Wand2 size={13} aria-hidden="true" /> {busy ? t("zoom.generating") : t("zoom.autoZoom")}
          </button>
        </div>
      </Group>
      <Group
        title={seg ? t("zoom.selectedSegment") : t("zoom.segment")}
        action={
          seg && (
            <button type="button" className="iconbtn" style={{ width: 26, height: 26 }} aria-label={t("zoom.deleteSegment")} onClick={() => ed().deleteSelection()}>
              <Trash2 size={14} />
            </button>
          )
        }
      >
        {seg && selection ? (
          <>
            <div className="field__hint">
              {fmtTime(seg.start, true)} → {fmtTime(seg.end, true)} ({t("unit.seconds", { n: ((seg.end - seg.start) / 1000).toFixed(1) })}){seg.auto ? t("zoom.autoTag") : ""}
            </div>
            <Slider label={t("zoom.scale")} value={seg.scale} min={1.1} max={4} step={0.05} format={(v) => `${v.toFixed(2)}×`} onChange={(v) => ed().updateZoom(seg.id, { scale: v, auto: false })} />
            <Select<Easing> label={t("zoom.easing")} value={seg.easing} options={easingOptions()} onChange={(v) => ed().updateZoom(seg.id, { easing: v, auto: false })} />
            <Slider
              label={t("zoom.transition")}
              value={seg.transitionMs}
              min={0}
              max={1500}
              step={50}
              format={(v) => `${v} ms`}
              onChange={(v) => ed().updateZoom(seg.id, { transitionMs: v, auto: false })}
            />
            <Slider label={t("zoom.focusX")} value={seg.cx} min={0} max={1} step={0.01} format={pct} onChange={(v) => ed().updateZoom(seg.id, { cx: v, auto: false })} />
            <Slider label={t("zoom.focusY")} value={seg.cy} min={0} max={1} step={0.01} format={pct} onChange={(v) => ed().updateZoom(seg.id, { cy: v, auto: false })} />
            <Slider
              label={t("zoom.duration")}
              value={seg.end - seg.start}
              min={300}
              max={15000}
              step={100}
              format={(v) => t("unit.seconds", { n: (v / 1000).toFixed(1) })}
              onChange={(v) => {
                const p = ed().project!;
                const next = p.zoomSegments.filter((z) => z.start >= seg.end - 1 && z.id !== seg.id).sort((a, b) => a.start - b.start)[0];
                const max = Math.min(p.media.durationMs, next ? next.start : Infinity);
                ed().updateZoom(seg.id, { end: Math.min(max, seg.start + v) });
              }}
            />
            <p className="field__hint" style={{ margin: 0 }}>
              {t("zoom.tip")}
            </p>
          </>
        ) : (
          <div className="empty-note">{t("zoom.empty")}</div>
        )}
      </Group>
      <SpeedGroup />
    </>
  );
}

// -------------------------------------------------------------- Cursor

function CursorPanel() {
  const c = useEditor((s) => s.project!.style.cursor);
  const keys = useEditor((s) => s.project!.style.keys);
  const hasKeys = useEditor((s) => s.project!.keys.length > 0);
  const set = (patch: Partial<typeof c>) => useEditor.getState().setStyle("cursor", patch);
  return (
    <>
      <Group title={t("cursor.group")}>
        <ToggleRow label={t("cursor.show")} checked={c.visible} onChange={(v) => set({ visible: v })} />
        <ToggleRow label={t("cursor.smooth")} hint={t("cursor.smoothHint")} checked={c.smooth} onChange={(v) => set({ smooth: v })} />
        <Slider label={t("cursor.smoothing")} value={c.smoothing} min={0} max={1} step={0.01} format={pct} disabled={!c.smooth} onChange={(v) => set({ smoothing: v })} />
        <Slider label={t("cursor.size")} value={c.size} min={0.5} max={3} step={0.05} format={(v) => `${v.toFixed(2)}×`} onChange={(v) => set({ size: v })} />
      </Group>
      <Group title={t("cursor.effects")}>
        <ToggleRow label={t("cursor.ripple")} hint={t("cursor.rippleHint")} checked={c.clickRipple} onChange={(v) => set({ clickRipple: v })} />
        <Slider label={t("cursor.blur")} value={c.motionBlur} min={0} max={1} step={0.01} format={pct} onChange={(v) => set({ motionBlur: v })} />
      </Group>
      <Group title={t("cursor.keystrokes")}>
        <ToggleRow
          label={t("cursor.keyOverlay")}
          hint={hasKeys ? t("cursor.keyOverlayHint") : t("cursor.keyOverlayNone")}
          checked={keys.show}
          onChange={(v) => useEditor.getState().setStyle("keys", { show: v })}
        />
      </Group>
    </>
  );
}

// --------------------------------------------------------------- Frame

const FRAMES: WindowFrame[] = ["none", "minimal", "windows", "macos", "laptop", "phone"];

function FramePanel() {
  const st = useEditor((s) => s.project!.style);
  const ed = useEditor.getState;
  const setTop = (patch: Partial<typeof st>, key: string) => ed().apply((p) => ({ ...p, style: { ...p.style, ...patch } }), { coalesce: `frame.${key}` });
  return (
    <>
      <Group title={t("frame.background")}>
        <div className="swatches" role="radiogroup" aria-label={t("frame.gradient")}>
          {GRADIENTS.map((g) => (
            <button
              key={g.id}
              type="button"
              role="radio"
              aria-checked={st.background.id === g.id}
              aria-label={t(`gradient.${g.id}` as MessageKey)}
              title={t(`gradient.${g.id}` as MessageKey)}
              className="swatch"
              style={{ background: gradientCss(g) }}
              onClick={() => setTop({ background: g }, "bg")}
            />
          ))}
        </div>
        <Slider
          label={t("frame.angle")}
          value={st.background.angle}
          min={0}
          max={360}
          step={5}
          format={(v) => `${v}°`}
          onChange={(v) => setTop({ background: { ...st.background, angle: v } }, "angle")}
        />
      </Group>
      <Group title={t("frame.layout")}>
        <Slider label={t("frame.padding")} value={st.padding} min={0} max={40} step={0.5} format={(v) => `${v}%`} onChange={(v) => setTop({ padding: v }, "padding")} />
        <Slider label={t("frame.radius")} value={st.radius} min={0} max={48} step={1} format={(v) => `${v} px`} onChange={(v) => setTop({ radius: v }, "radius")} />
        <Slider label={t("frame.shadow")} value={st.shadow} min={0} max={100} step={1} format={(v) => `${v}`} onChange={(v) => setTop({ shadow: v }, "shadow")} />
      </Group>
      <Group title={t("frame.windowFrame")}>
        <Select<WindowFrame> label={t("frame.style")} value={st.frame} options={FRAMES.map((f) => ({ value: f, label: t(`frame.${f}`) }))} onChange={(v) => setTop({ frame: v }, "frame")} />
      </Group>
      <BackgroundImageGroup />
    </>
  );
}

// -------------------------------------------------------------- Webcam

const CORNERS: Corner[] = ["top-left", "top-right", "bottom-left", "bottom-right"];

function WebcamPanel() {
  const w = useEditor((s) => s.project!.style.webcam);
  const hasTrack = useEditor((s) => !!s.project!.media.webcam);
  /** The demo scene (no media URLs) shows a placeholder camera, like the exporter-less browser preview. */
  const demo = useEditor((s) => !s.project!.media.screen?.url);
  const set = (patch: Partial<typeof w>) => useEditor.getState().setStyle("webcam", patch);
  return (
    <>
      <Group title={t("webcam.group")}>
        {!hasTrack && <p className="field__hint" style={{ margin: 0 }}>{t("webcam.noTrack")}</p>}
        <ToggleRow label={t("webcam.show")} checked={w.enabled} onChange={(v) => set({ enabled: v })} />
        <div className="field">
          <span className="field__label" id="cam-shape">
            {t("webcam.shape")}
          </span>
          <div className="optgrid" role="radiogroup" aria-labelledby="cam-shape">
            {(
              [
                ["circle", t("webcam.circle"), <Circle size={18} key="c" />],
                ["rounded", t("webcam.rounded"), <RectangleHorizontal size={18} key="r" />],
                ["square", t("webcam.square"), <Square size={18} key="s" />],
              ] as [WebcamShape, string, ReactNode][]
            ).map(([v, label, icon]) => (
              <button key={v} type="button" role="radio" aria-checked={w.shape === v} className="optcard" onClick={() => set({ shape: v })}>
                {icon}
                {label}
              </button>
            ))}
          </div>
        </div>
        <Slider label={t("webcam.size")} value={w.size} min={0.05} max={0.6} step={0.01} format={pct} onChange={(v) => set({ size: v })} />
        <Select<Corner> label={t("webcam.position")} value={w.position} options={CORNERS.map((c) => ({ value: c, label: t(`corner.${c}`) }))} onChange={(v) => set({ position: v })} />
      </Group>
      {hasTrack || demo ? (
        <WebcamEffects />
      ) : (
        <p className="field__hint" data-testid="webcam-effects-need-track" style={{ margin: 0 }}>
          {t("webcam.effectsNeedTrack")}
        </p>
      )}
    </>
  );
}

// --------------------------------------------------------------- Audio

/** Gain 0..4 (backend range) with a mute switch: mute = gain 0, unmute restores the previous gain. */
function TrackGain({ label, muteLabel, value, onChange }: { label: string; muteLabel: string; value: number; onChange: (v: number) => void }) {
  const last = useRef(value > 0 ? value : 1);
  if (value > 0) last.current = value;
  return (
    <div className="field">
      <Slider label={label} value={value} min={0} max={4} step={0.05} format={(v) => (v === 0 ? t("audio.muted") : pct(v))} onChange={onChange} />
      <ToggleRow label={t("audio.mute")} checked={value === 0} ariaLabel={muteLabel} onChange={(m) => onChange(m ? 0 : last.current)} />
    </div>
  );
}

function AudioPanel() {
  const a = useEditor((s) => s.project!.style.audio);
  const set = (patch: Partial<typeof a>) => useEditor.getState().setStyle("audio", patch);
  const mic = useEditor((s) => s.project!.media.mic);
  const sys = useEditor((s) => s.project!.media.systemAudio);
  const off = (ms: number) => `${ms >= 0 ? "+" : ""}${ms} ms`;
  return (
    <>
      <Group title={t("audio.tracks")}>
        {mic ? (
          <TrackGain label={t("audio.mic", { offset: off(mic.offsetMs) })} muteLabel={t("audio.muteMic")} value={a.micGain} onChange={(v) => set({ micGain: v })} />
        ) : (
          <p className="field__hint" style={{ margin: 0 }}>{t("audio.noMic")}</p>
        )}
        {sys ? (
          <TrackGain label={t("audio.system", { offset: off(sys.offsetMs) })} muteLabel={t("audio.muteSystem")} value={a.systemGain} onChange={(v) => set({ systemGain: v })} />
        ) : (
          <p className="field__hint" style={{ margin: 0 }}>{t("audio.noSystem")}</p>
        )}
      </Group>
      <Group title={t("audio.cleanup")}>
        <ToggleRow label={t("audio.gate")} hint={t("audio.gateHint")} checked={a.noiseGate} onChange={(v) => set({ noiseGate: v })} />
        {a.noiseGate && (
          <Slider label={t("audio.gateThreshold")} value={a.noiseGateThreshold} min={-80} max={-20} step={1} format={(v) => `${v} dB`} onChange={(v) => set({ noiseGateThreshold: v })} />
        )}
        <ToggleRow label={t("audio.highPass")} hint={t("audio.highPassHint")} checked={a.highPass} onChange={(v) => set({ highPass: v })} />
        {a.highPass && <Slider label={t("audio.cutoff")} value={a.highPassHz} min={40} max={300} step={5} format={(v) => `${v} Hz`} onChange={(v) => set({ highPassHz: v })} />}
      </Group>
      <Group title={t("audio.loudness")}>
        <ToggleRow label={t("audio.normalize")} checked={a.normalize} onChange={(v) => set({ normalize: v })} />
        {a.normalize && <Slider label={t("audio.target")} value={a.targetLufs} min={-24} max={-9} step={1} format={(v) => `${v} LUFS`} onChange={(v) => set({ targetLufs: v })} />}
        <p className="field__hint" style={{ margin: 0 }}>
          {t("audio.note")}
        </p>
      </Group>
    </>
  );
}

// ------------------------------------------------------------- Captions

function CaptionsPanel() {
  const c = useEditor((s) => s.project!.style.captions);
  const cues = useEditor((s) => s.project!.captions);
  const playhead = useEditor((s) => s.playhead);
  const set = (patch: Partial<typeof c>) => useEditor.getState().setStyle("captions", patch);
  const file = useRef<HTMLInputElement>(null);

  const onFile = async (f: File | undefined) => {
    if (!f) return;
    const parsed = parseSubtitles(await f.text());
    if (!parsed.length) {
      useApp.getState().showToast(t("captions.noCues"));
      return;
    }
    useEditor.getState().setCaptions(parsed);
    useApp.getState().showToast(t("captions.imported", { count: parsed.length }));
  };

  return (
    <>
      <Group title={t("captions.group")}>
        <ToggleRow label={t("captions.show")} checked={c.enabled} onChange={(v) => set({ enabled: v })} />
        <input ref={file} type="file" accept=".srt,.vtt,text/vtt" className="sr-only" tabIndex={-1} aria-hidden="true" onChange={(e) => void onFile(e.target.files?.[0]).finally(() => (e.target.value = ""))} />
        <div style={{ display: "flex", gap: 8 }}>
          <button type="button" className="btn btn--sm" onClick={() => file.current?.click()}>
            <Upload size={13} aria-hidden="true" /> {t("captions.import")}
          </button>
          <button type="button" className="btn btn--sm" disabled={!cues.length} onClick={() => useEditor.getState().setCaptions([])}>
            {t("captions.clear")}
          </button>
        </div>
      </Group>
      <Group title={t("captions.style")}>
        <Slider label={t("captions.fontSize")} value={c.fontSize} min={20} max={80} step={1} format={(v) => `${v} px`} onChange={(v) => set({ fontSize: v })} />
        <Segmented<"bottom" | "top">
          label={t("captions.position")}
          value={c.position}
          onChange={(v) => set({ position: v })}
          options={[
            { value: "bottom", label: t("captions.bottom") },
            { value: "top", label: t("captions.top") },
          ]}
        />
        <ToggleRow label={t("captions.box")} checked={c.background} onChange={(v) => set({ background: v })} />
      </Group>
      <Group title={t("captions.cues", { count: cues.length })}>
        {cues.length ? (
          <div className="cuelist scroll">
            {cues.map((q) => (
              <button
                key={q.id}
                type="button"
                className="cue"
                aria-current={playhead >= q.start && playhead <= q.end}
                onClick={() => useEditor.getState().setPlayhead(q.start)}
                aria-label={t("captions.jump", { time: fmtTime(q.start), text: q.text })}
              >
                <span className="cue__t">{fmtTime(q.start)}</span>
                <span className="cue__text">{q.text}</span>
              </button>
            ))}
          </div>
        ) : (
          <div className="empty-note">{t("captions.empty")}</div>
        )}
      </Group>
    </>
  );
}
