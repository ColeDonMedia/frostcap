import { useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { ZoomIn, MousePointer2, Frame, Camera, AudioLines, Captions, Upload, Wand2, Trash2, Circle, Square, RectangleHorizontal } from "lucide-react";
import { useEditor } from "../../store/editor";
import { useApp } from "../../store/app";
import { Segmented, Select, Slider, ToggleRow } from "../../components/ui";
import { slideStyle } from "../../lib/indicator";
import { EASINGS, GRADIENTS, ZOOM_PRESETS, gradientCss } from "../../lib/presets";
import { backend } from "../../backend";
import { parseSubtitles } from "../../lib/captions";
import { fmtTime } from "../../lib/format";
import type { Corner, Easing, WebcamShape, WindowFrame, ZoomPreset } from "../../types";

type TabId = "zoom" | "cursor" | "frame" | "webcam" | "audio" | "captions";
const TABS: { id: TabId; label: string; icon: typeof ZoomIn }[] = [
  { id: "zoom", label: "Zoom", icon: ZoomIn },
  { id: "cursor", label: "Cursor", icon: MousePointer2 },
  { id: "frame", label: "Frame", icon: Frame },
  { id: "webcam", label: "Webcam", icon: Camera },
  { id: "audio", label: "Audio", icon: AudioLines },
  { id: "captions", label: "Captions", icon: Captions },
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
    <aside className="inspector glass" aria-label="Inspector">
      <div className="tabs" role="tablist" aria-label="Inspector sections" ref={tabsRef} onKeyDown={onKey}>
        <span className="tab__bg" aria-hidden="true" style={slideStyle(TABS.findIndex((t) => t.id === tab), TABS.length, { padPx: 8, gapPx: 2 })} />
        {TABS.map((t) => {
          const Icon = t.icon;
          const sel = t.id === tab;
          return (
            <button
              key={t.id}
              type="button"
              role="tab"
              id={`tab-${t.id}`}
              aria-selected={sel}
              aria-controls={sel ? `panel-${t.id}` : undefined}
              tabIndex={sel ? 0 : -1}
              className="tab"
              onClick={() => setTab(t.id)}
            >
              <Icon size={16} aria-hidden="true" />
              <span>{t.label}</span>
            </button>
          );
        })}
      </div>
      <div className="tabpanel scroll" role="tabpanel" id={`panel-${tab}`} aria-labelledby={`tab-${tab}`} tabIndex={0}>
        {tab === "zoom" && <ZoomPanel />}
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
    ed().notifyUndoable("Applied to all zoom segments");
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
      ed().notifyUndoable(`${auto.length} auto zoom${auto.length === 1 ? "" : "s"} generated (${ZOOM_PRESETS[style.preset].label})`);
    } catch (e) {
      useApp.getState().reportIssue(e, "Auto zoom");
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <Group title="Preset">
        <Segmented<ZoomPreset>
          label="Zoom preset"
          value={style.preset}
          onChange={setPreset}
          options={(Object.keys(ZOOM_PRESETS) as ZoomPreset[]).map((k) => ({ value: k, label: ZOOM_PRESETS[k].label }))}
        />
        <Slider label="Default scale" value={style.scale} min={1.1} max={4} step={0.05} format={(v) => `${v.toFixed(2)}×`} onChange={(v) => ed().setStyle("zoom", { scale: v })} />
        <Select<Easing> label="Easing" value={style.easing} options={EASINGS} onChange={(v) => ed().setStyle("zoom", { easing: v })} />
        <Slider
          label="Transition duration"
          value={style.transitionMs}
          min={100}
          max={1500}
          step={50}
          format={(v) => `${v} ms`}
          onChange={(v) => ed().setStyle("zoom", { transitionMs: v })}
        />
        <div style={{ display: "flex", gap: 8 }}>
          <button type="button" className="btn btn--sm" onClick={applyAll}>
            Apply to all
          </button>
          <button type="button" className="btn btn--sm" onClick={() => void regenerate()} disabled={busy}>
            <Wand2 size={13} aria-hidden="true" /> {busy ? "Generating…" : "Auto zoom"}
          </button>
        </div>
      </Group>
      <Group
        title={seg ? "Selected segment" : "Segment"}
        action={
          seg && (
            <button type="button" className="iconbtn" style={{ width: 26, height: 26 }} aria-label="Delete segment" onClick={() => ed().deleteSelection()}>
              <Trash2 size={14} />
            </button>
          )
        }
      >
        {seg && selection ? (
          <>
            <div className="field__hint">
              {fmtTime(seg.start, true)} → {fmtTime(seg.end, true)} ({((seg.end - seg.start) / 1000).toFixed(1)} s){seg.auto ? " · auto" : ""}
            </div>
            <Slider label="Scale" value={seg.scale} min={1.1} max={4} step={0.05} format={(v) => `${v.toFixed(2)}×`} onChange={(v) => ed().updateZoom(seg.id, { scale: v, auto: false })} />
            <Select<Easing> label="Easing" value={seg.easing} options={EASINGS} onChange={(v) => ed().updateZoom(seg.id, { easing: v, auto: false })} />
            <Slider
              label="Transition"
              value={seg.transitionMs}
              min={0}
              max={1500}
              step={50}
              format={(v) => `${v} ms`}
              onChange={(v) => ed().updateZoom(seg.id, { transitionMs: v, auto: false })}
            />
            <Slider label="Focus X" value={seg.cx} min={0} max={1} step={0.01} format={pct} onChange={(v) => ed().updateZoom(seg.id, { cx: v, auto: false })} />
            <Slider label="Focus Y" value={seg.cy} min={0} max={1} step={0.01} format={pct} onChange={(v) => ed().updateZoom(seg.id, { cy: v, auto: false })} />
            <Slider
              label="Duration"
              value={seg.end - seg.start}
              min={300}
              max={15000}
              step={100}
              format={(v) => `${(v / 1000).toFixed(1)} s`}
              onChange={(v) => {
                const p = ed().project!;
                const next = p.zoomSegments.filter((z) => z.start >= seg.end - 1 && z.id !== seg.id).sort((a, b) => a.start - b.start)[0];
                const max = Math.min(p.media.durationMs, next ? next.start : Infinity);
                ed().updateZoom(seg.id, { end: Math.min(max, seg.start + v) });
              }}
            />
            <p className="field__hint" style={{ margin: 0 }}>
              Tip: click the preview to place the focus point.
            </p>
          </>
        ) : (
          <div className="empty-note">Select a zoom segment on the timeline, or press Z to add one at the playhead.</div>
        )}
      </Group>
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
      <Group title="Cursor">
        <ToggleRow label="Show cursor" checked={c.visible} onChange={(v) => set({ visible: v })} />
        <ToggleRow label="Smooth cursor motion" hint="Spring-filters the recorded cursor path" checked={c.smooth} onChange={(v) => set({ smooth: v })} />
        <Slider label="Smoothing" value={c.smoothing} min={0} max={1} step={0.01} format={pct} disabled={!c.smooth} onChange={(v) => set({ smoothing: v })} />
        <Slider label="Size" value={c.size} min={0.5} max={3} step={0.05} format={(v) => `${v.toFixed(2)}×`} onChange={(v) => set({ size: v })} />
      </Group>
      <Group title="Effects">
        <ToggleRow label="Click ripple" hint="Pulse on every click" checked={c.clickRipple} onChange={(v) => set({ clickRipple: v })} />
        <Slider label="Motion blur" value={c.motionBlur} min={0} max={1} step={0.01} format={pct} onChange={(v) => set({ motionBlur: v })} />
      </Group>
      <Group title="Keystrokes">
        <ToggleRow
          label="Show key overlay"
          hint={hasKeys ? "Shows recorded key presses on screen" : "This recording has no key presses (capture was off)"}
          checked={keys.show}
          onChange={(v) => useEditor.getState().setStyle("keys", { show: v })}
        />
      </Group>
    </>
  );
}

// --------------------------------------------------------------- Frame

const FRAMES: { value: WindowFrame; label: string }[] = [
  { value: "none", label: "None" },
  { value: "minimal", label: "Minimal" },
  { value: "windows", label: "Windows" },
  { value: "macos", label: "macOS" },
];

function FramePanel() {
  const st = useEditor((s) => s.project!.style);
  const ed = useEditor.getState;
  const setTop = (patch: Partial<typeof st>, key: string) => ed().apply((p) => ({ ...p, style: { ...p.style, ...patch } }), { coalesce: `frame.${key}` });
  return (
    <>
      <Group title="Background">
        <div className="swatches" role="radiogroup" aria-label="Background gradient">
          {GRADIENTS.map((g) => (
            <button
              key={g.id}
              type="button"
              role="radio"
              aria-checked={st.background.id === g.id}
              aria-label={g.id}
              title={g.id}
              className="swatch"
              style={{ background: gradientCss(g) }}
              onClick={() => setTop({ background: g }, "bg")}
            />
          ))}
        </div>
        <Slider
          label="Angle"
          value={st.background.angle}
          min={0}
          max={360}
          step={5}
          format={(v) => `${v}°`}
          onChange={(v) => setTop({ background: { ...st.background, angle: v } }, "angle")}
        />
      </Group>
      <Group title="Layout">
        <Slider label="Padding" value={st.padding} min={0} max={40} step={0.5} format={(v) => `${v}%`} onChange={(v) => setTop({ padding: v }, "padding")} />
        <Slider label="Corner radius" value={st.radius} min={0} max={48} step={1} format={(v) => `${v} px`} onChange={(v) => setTop({ radius: v }, "radius")} />
        <Slider label="Shadow" value={st.shadow} min={0} max={100} step={1} format={(v) => `${v}`} onChange={(v) => setTop({ shadow: v }, "shadow")} />
      </Group>
      <Group title="Window frame">
        <Select<WindowFrame> label="Frame style" value={st.frame} options={FRAMES} onChange={(v) => setTop({ frame: v }, "frame")} />
      </Group>
    </>
  );
}

// -------------------------------------------------------------- Webcam

const CORNERS: { value: Corner; label: string }[] = [
  { value: "top-left", label: "Top left" },
  { value: "top-right", label: "Top right" },
  { value: "bottom-left", label: "Bottom left" },
  { value: "bottom-right", label: "Bottom right" },
];

function WebcamPanel() {
  const w = useEditor((s) => s.project!.style.webcam);
  const hasTrack = useEditor((s) => !!s.project!.media.webcam);
  const set = (patch: Partial<typeof w>) => useEditor.getState().setStyle("webcam", patch);
  return (
    <>
      <Group title="Webcam overlay">
        {!hasTrack && <p className="field__hint" style={{ margin: 0 }}>No webcam track in this recording — the preview shows a placeholder.</p>}
        <ToggleRow label="Show webcam" checked={w.enabled} onChange={(v) => set({ enabled: v })} />
        <div className="field">
          <span className="field__label" id="cam-shape">
            Shape
          </span>
          <div className="optgrid" role="radiogroup" aria-labelledby="cam-shape">
            {(
              [
                ["circle", "Circle", <Circle size={18} key="c" />],
                ["rounded", "Rounded", <RectangleHorizontal size={18} key="r" />],
                ["square", "Square", <Square size={18} key="s" />],
              ] as [WebcamShape, string, ReactNode][]
            ).map(([v, label, icon]) => (
              <button key={v} type="button" role="radio" aria-checked={w.shape === v} className="optcard" onClick={() => set({ shape: v })}>
                {icon}
                {label}
              </button>
            ))}
          </div>
        </div>
        <Slider label="Size" value={w.size} min={0.05} max={0.6} step={0.01} format={pct} onChange={(v) => set({ size: v })} />
        <Select<Corner> label="Position" value={w.position} options={CORNERS} onChange={(v) => set({ position: v })} />
      </Group>
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
      <Slider label={label} value={value} min={0} max={4} step={0.05} format={(v) => (v === 0 ? "muted" : pct(v))} onChange={onChange} />
      <ToggleRow label="Mute" checked={value === 0} ariaLabel={muteLabel} onChange={(m) => onChange(m ? 0 : last.current)} />
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
      <Group title="Tracks">
        {mic ? (
          <TrackGain label={`Microphone (offset ${off(mic.offsetMs)})`} muteLabel="Mute microphone" value={a.micGain} onChange={(v) => set({ micGain: v })} />
        ) : (
          <p className="field__hint" style={{ margin: 0 }}>No microphone track.</p>
        )}
        {sys ? (
          <TrackGain label={`System audio (offset ${off(sys.offsetMs)})`} muteLabel="Mute system audio" value={a.systemGain} onChange={(v) => set({ systemGain: v })} />
        ) : (
          <p className="field__hint" style={{ margin: 0 }}>No system-audio track.</p>
        )}
      </Group>
      <Group title="Clean-up">
        <ToggleRow label="Noise gate" hint="Silence background hiss between words" checked={a.noiseGate} onChange={(v) => set({ noiseGate: v })} />
        {a.noiseGate && (
          <Slider label="Gate threshold" value={a.noiseGateThreshold} min={-80} max={-20} step={1} format={(v) => `${v} dB`} onChange={(v) => set({ noiseGateThreshold: v })} />
        )}
        <ToggleRow label="High-pass filter" hint="Remove rumble and desk thumps" checked={a.highPass} onChange={(v) => set({ highPass: v })} />
        {a.highPass && <Slider label="Cutoff" value={a.highPassHz} min={40} max={300} step={5} format={(v) => `${v} Hz`} onChange={(v) => set({ highPassHz: v })} />}
      </Group>
      <Group title="Loudness">
        <ToggleRow label="Normalize" checked={a.normalize} onChange={(v) => set({ normalize: v })} />
        {a.normalize && <Slider label="Target" value={a.targetLufs} min={-24} max={-9} step={1} format={(v) => `${v} LUFS`} onChange={(v) => set({ targetLufs: v })} />}
        <p className="field__hint" style={{ margin: 0 }}>
          Clean-up and loudness are applied on export (backend). The preview plays the raw tracks with the gains above.
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
      useApp.getState().showToast("No cues found in that file");
      return;
    }
    useEditor.getState().setCaptions(parsed);
    useApp.getState().showToast(`Imported ${parsed.length} caption${parsed.length > 1 ? "s" : ""}`);
  };

  return (
    <>
      <Group title="Captions">
        <ToggleRow label="Show captions" checked={c.enabled} onChange={(v) => set({ enabled: v })} />
        <input ref={file} type="file" accept=".srt,.vtt,text/vtt" className="sr-only" tabIndex={-1} aria-hidden="true" onChange={(e) => void onFile(e.target.files?.[0]).finally(() => (e.target.value = ""))} />
        <div style={{ display: "flex", gap: 8 }}>
          <button type="button" className="btn btn--sm" onClick={() => file.current?.click()}>
            <Upload size={13} aria-hidden="true" /> Import SRT / VTT
          </button>
          <button type="button" className="btn btn--sm" disabled={!cues.length} onClick={() => useEditor.getState().setCaptions([])}>
            Clear
          </button>
        </div>
      </Group>
      <Group title="Style">
        <Slider label="Font size" value={c.fontSize} min={20} max={80} step={1} format={(v) => `${v} px`} onChange={(v) => set({ fontSize: v })} />
        <Segmented<"bottom" | "top">
          label="Caption position"
          value={c.position}
          onChange={(v) => set({ position: v })}
          options={[
            { value: "bottom", label: "Bottom" },
            { value: "top", label: "Top" },
          ]}
        />
        <ToggleRow label="Background box" checked={c.background} onChange={(v) => set({ background: v })} />
      </Group>
      <Group title={`Cues (${cues.length})`}>
        {cues.length ? (
          <div className="cuelist scroll">
            {cues.map((q) => (
              <button
                key={q.id}
                type="button"
                className="cue"
                aria-current={playhead >= q.start && playhead <= q.end}
                onClick={() => useEditor.getState().setPlayhead(q.start)}
                aria-label={`Jump to caption at ${fmtTime(q.start)}: ${q.text}`}
              >
                <span className="cue__t">{fmtTime(q.start)}</span>
                <span className="cue__text">{q.text}</span>
              </button>
            ))}
          </div>
        ) : (
          <div className="empty-note">No captions. Import an .srt or .vtt file.</div>
        )}
      </Group>
    </>
  );
}
