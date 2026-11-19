import { m } from "motion/react";
import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import {
  Monitor,
  AppWindow,
  Keyboard,
  Crop,
  RotateCcw,
  RefreshCw,
  Check,
  Camera,
  Mic,
  Volume2,
  MousePointer2,
  Timer,
  Circle,
  Gauge,
  FolderOpen,
  Clock,
} from "lucide-react";
import { backend, isTauri } from "../backend";
import type { CameraInfo, CaptureSource, RecentProject, SourceKind } from "../types";
import { useApp, type Settings } from "../store/app";
import { useRecording } from "../store/recording";
import { openProjectGuarded } from "../store/guard";
import { Segmented, Select, Switch, softSpring } from "../components/ui";
import { Thumb } from "../components/Thumb";
import { fmtClock, fmtRelative } from "../lib/format";

type Filter = "all" | SourceKind;

const KIND_ICON: Record<SourceKind, typeof Monitor> = { display: Monitor, window: AppWindow, region: Crop };

export default function RecorderView() {
  const [listed, setSources] = useState<CaptureSource[] | null>(null);
  /** Region picked with the native overlay (pick_region); shown as an extra source card. */
  const [region, setRegion] = useState<CaptureSource | null>(null);
  const sources = useMemo(() => (listed ? (region ? [region, ...listed] : listed) : null), [listed, region]);
  const [recents, setRecents] = useState<RecentProject[] | null>(null);
  const [filter, setFilter] = useState<Filter>("all");
  const [selected, setSelected] = useState<string | null>(null);
  const settings = useApp((s) => s.settings);
  const setSetting = useApp((s) => s.setSetting);
  const phase = useRecording((s) => s.phase);
  const start = useRecording((s) => s.start);
  const reportIssue = useApp((s) => s.reportIssue);
  const showToast = useApp((s) => s.showToast);
  const gridRef = useRef<HTMLDivElement>(null);
  const [cameras, setCameras] = useState<CameraInfo[] | null | "unsupported">(null);
  const [recovering, setRecovering] = useState<string | null>(null);

  const refreshSources = () =>
    backend
      .listSources()
      .then((s) => {
        setSources(s);
        setSelected((cur) => (cur && s.some((x) => x.id === cur) ? cur : ((s.find((x) => x.primary) ?? s[0])?.id ?? null)));
      })
      .catch((e) => {
        setSources([]);
        reportIssue(e, "Could not list sources");
      });

  useEffect(() => {
    if (phase !== "idle") return;
    void refreshSources();
    backend.listRecentProjects().then(setRecents, (e) => {
      setRecents([]);
      reportIssue(e, "Could not load recent projects");
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [phase === "idle"]);

  useEffect(() => {
    if (!settings.webcam || cameras !== null) return;
    backend.listCameras().then(setCameras, () => setCameras("unsupported"));
  }, [settings.webcam, cameras]);

  const pickRegion = async () => {
    try {
      const r = await backend.pickRegion();
      if (!r) return;
      const src: CaptureSource = {
        id: "region:picked",
        kind: "region",
        name: "Region",
        width: r.screenRect.width,
        height: r.screenRect.height,
        wire: { kind: "region", monitorId: r.monitorId, rect: r.rect },
      };
      setRegion(src);
      setSelected(src.id);
      setFilter("all");
    } catch (e) {
      reportIssue(e, "Select region");
    }
  };

  const openFromDisk = async () => {
    try {
      const p = await backend.pickProject();
      if (p) void openProjectGuarded(p);
    } catch (e) {
      reportIssue(e, "Open project");
    }
  };

  /** Recovers each project in order; returns the paths that succeeded. */
  const recover = async (list: RecentProject[]) => {
    const ok: RecentProject[] = [];
    for (const r of list) {
      setRecovering(r.path);
      try {
        await backend.recoverProject(r.path);
        ok.push(r);
      } catch (e) {
        reportIssue(e, `Recovery of "${r.name}" failed`);
      }
    }
    setRecovering(null);
    if (ok.length === 1) showToast(`Recovered "${ok[0].name}"`, "info", { label: "Open", run: () => void openProjectGuarded(ok[0].path) });
    else if (ok.length > 1) showToast(`Recovered ${ok.length} recordings`, "info");
    backend.listRecentProjects().then(setRecents, () => undefined);
  };
  const interrupted = (recents ?? []).filter((r) => r.interrupted);

  const visible = useMemo(() => (sources ?? []).filter((s) => filter === "all" || s.kind === filter), [sources, filter]);
  const source = sources?.find((s) => s.id === selected) ?? null;
  const setSource = useRecording((s) => s.setSource);
  useEffect(() => setSource(source), [source, setSource]);

  const onGridKey = (e: KeyboardEvent) => {
    const keys = ["ArrowRight", "ArrowLeft", "ArrowDown", "ArrowUp", "Home", "End"];
    if (!keys.includes(e.key) || !visible.length) return;
    e.preventDefault();
    const i = Math.max(0, visible.findIndex((s) => s.id === selected));
    const cols = Math.max(1, Math.round((gridRef.current?.clientWidth ?? 1) / 234));
    let n = i;
    if (e.key === "ArrowRight") n = i + 1;
    if (e.key === "ArrowLeft") n = i - 1;
    if (e.key === "ArrowDown") n = i + cols;
    if (e.key === "ArrowUp") n = i - cols;
    if (e.key === "Home") n = 0;
    if (e.key === "End") n = visible.length - 1;
    n = Math.min(visible.length - 1, Math.max(0, n));
    setSelected(visible[n].id);
    gridRef.current?.querySelectorAll<HTMLElement>("[role=radio]")[n]?.focus();
  };

  const busy = phase !== "idle";

  return (
    <div className="page scroll">
      <div className="page__inner">
      <div className="page__head">
        <div style={{ flex: 1 }}>
          <h1 className="page__title">New recording</h1>
          <p className="page__sub">Pick what to capture. Zooms, cursor polish and framing are applied afterwards.</p>
        </div>
        <Segmented<Filter>
          label="Filter sources"
          value={filter}
          onChange={setFilter}
          options={[
            { value: "all", label: "All" },
            { value: "display", label: "Displays", icon: <Monitor size={14} /> },
            { value: "window", label: "Windows", icon: <AppWindow size={14} /> },
          ]}
        />
        <button type="button" className="btn" onClick={() => void pickRegion()} disabled={phase !== "idle"}>
          <Crop size={15} aria-hidden="true" /> Select region…
        </button>
      </div>

      {interrupted.length > 0 && (
        <div className="banner glass" role="region" aria-label="Interrupted recordings">
          <RotateCcw size={18} aria-hidden="true" />
          <div className="banner__text">
            <strong>{interrupted.length === 1 ? "A recording was interrupted" : `${interrupted.length} recordings were interrupted`}</strong>
            <span>Frostcap can rebuild these projects from what was written to disk.</span>
            <ul className="banner__list">
              {interrupted.map((r) => (
                <li key={r.path}>
                  <span className="banner__name">{r.name}</span>
                  <button type="button" className="btn btn--sm" disabled={recovering !== null} onClick={() => void recover([r])} aria-label={`Recover ${r.name}`}>
                    {recovering === r.path ? "Recovering…" : "Recover"}
                  </button>
                </li>
              ))}
            </ul>
          </div>
          {interrupted.length > 1 && (
            <button type="button" className="btn btn--primary btn--sm" disabled={recovering !== null} onClick={() => void recover(interrupted)}>
              Recover all
            </button>
          )}
        </div>
      )}

      <div className="rec-layout">
        <section aria-label="Capture source">
          {!sources ? (
            <div className="loading" style={{ minHeight: 200 }}>
              <div className="spinner" role="status" aria-label="Loading sources" />
            </div>
          ) : sources.length === 0 ? (
            <div className="empty-note">
              No capturable screens or windows were found.{" "}
              <button type="button" className="btn btn--sm" onClick={() => void refreshSources()}>
                <RefreshCw size={13} aria-hidden="true" /> Retry
              </button>
            </div>
          ) : (
            <div
              className="sources"
              role="radiogroup"
              aria-label="Capture source"
              ref={gridRef}
              onKeyDown={onGridKey}
            >
              {visible.map((s, i) => {
                const sel = s.id === selected;
                const Icon = KIND_ICON[s.kind];
                return (
                  <m.button
                    key={s.id}
                    type="button"
                    role="radio"
                    aria-checked={sel}
                    tabIndex={sel || (!visible.some((v) => v.id === selected) && i === 0) ? 0 : -1}
                    className="srccard"
                    onClick={() => setSelected(s.id)}
                    onDoubleClick={() => !busy && start(s)}
                    initial={{ opacity: 0, y: 8 }}
                    animate={{ opacity: 1, y: 0 }}
                    transition={{ ...softSpring, delay: i * 0.03 }}
                    whileHover={{ y: -3 }}
                    whileTap={{ scale: 0.98 }}
                  >
                    <div className="srccard__thumb">
                      {s.thumbnail ? <img src={s.thumbnail} alt="" /> : <Thumb seed={s.id} kind={s.kind} />}
                    </div>
                    {sel && (
                      <m.span className="srccard__check" initial={{ scale: 0 }} animate={{ scale: 1 }} transition={softSpring}>
                        <Check size={14} strokeWidth={3} aria-hidden="true" />
                      </m.span>
                    )}
                    <div className="srccard__name">
                      <Icon size={15} aria-hidden="true" />
                      <span>{s.name}</span>
                    </div>
                    <div className="srccard__meta">
                      {s.kind === "window" && s.app ? `${s.app} · ` : ""}
                      {s.width}×{s.height}
                      {s.primary ? " · Primary" : ""}
                    </div>
                  </m.button>
                );
              })}
            </div>
          )}
        </section>

        <aside className="panel glass" aria-label="Recording options">
          <h2 className="panel__title">Capture</h2>
          <Opt icon={<Camera size={16} />} label="Webcam" hint="Overlay bubble, editable later" k="webcam" settings={settings} set={setSetting} />
          {settings.webcam && (
            <div className="optrow optrow--sub">
              {cameras === "unsupported" ? (
                <span className="field__hint">Camera selection needs a newer backend.</span>
              ) : cameras === null ? (
                <span className="field__hint">Looking for cameras…</span>
              ) : cameras.length === 0 ? (
                <span className="field__hint">No camera found.</span>
              ) : (
                <div className="optrow__text">
                  <Select<string>
                    label="Camera"
                    value={settings.camera ?? cameras[0].id}
                    onChange={(v) => setSetting("camera", v)}
                    options={cameras.map((cam) => ({ value: cam.id, label: cam.name }))}
                  />
                </div>
              )}
            </div>
          )}
          <Opt icon={<Mic size={16} />} label="Microphone" hint="Default input device" k="mic" settings={settings} set={setSetting} />
          <Opt icon={<Volume2 size={16} />} label="System audio" hint="What you hear" k="systemAudio" settings={settings} set={setSetting} />
          <Opt icon={<MousePointer2 size={16} />} label="Smooth cursor" hint="Redrawn from the cursor track, not baked into the video" k="cursor" settings={settings} set={setSetting} />
          <div className="optrow">
            <span className="optrow__icon" aria-hidden="true">
              <Keyboard size={16} />
            </span>
            <label className="optrow__text" htmlFor="opt-keys">
              <span className="optrow__label" style={{ display: "block" }}>
                Capture keystrokes
              </span>
              <span className="optrow__hint" style={{ display: "block" }}>
                {settings.keyCapture === "all" ? "All keys" : "Shortcuts only"} · see Settings › Privacy
              </span>
            </label>
            <Switch
              id="opt-keys"
              label="Capture keystrokes"
              checked={settings.keyCapture !== "off"}
              onChange={(v) => {
                // Security: the recorder switch only enables Shortcuts; All keys needs Settings + confirmation.
                setSetting("keyCapture", v ? "shortcuts" : "off");
              }}
            />
          </div>
          <Opt icon={<Timer size={16} />} label="Countdown" hint="3 seconds before start" k="countdown" settings={settings} set={setSetting} />
          <div className="optrow">
            <span className="optrow__icon" aria-hidden="true">
              <Gauge size={16} />
            </span>
            <div className="optrow__text">
              <Select<number>
                label="Frame rate"
                value={settings.fps}
                onChange={(v) => setSetting("fps", v)}
                options={[
                  { value: 30, label: "30 fps" },
                  { value: 60, label: "60 fps" },
                  { value: 120, label: "120 fps (high-refresh displays)" },
                ]}
              />
            </div>
          </div>
          <m.button
            type="button"
            className="btn btn--primary btn--lg"
            disabled={!source || busy}
            onClick={() => source && start(source)}
            whileHover={{ scale: 1.02 }}
            whileTap={{ scale: 0.97 }}
            transition={softSpring}
            style={{ marginTop: 6 }}
          >
            <Circle size={16} fill="currentColor" aria-hidden="true" />
            {busy ? "Recording…" : "Start recording"}
          </m.button>
          <p className="field__hint" style={{ margin: 0, textAlign: "center" }}>
            {source ? (
              <>
                Capturing <strong style={{ color: "var(--text)" }}>{source.name}</strong> at {settings.fps} fps
              </>
            ) : (
              "Select a source"
            )}
          </p>
        </aside>
      </div>

      <h2 className="h2">
        <Clock size={16} aria-hidden="true" /> Recent projects
      </h2>
      {recents && recents.length === 0 && <div className="empty-note">No projects yet. Your recordings will show up here.</div>}
      <div className="recents">
        {(recents ?? []).map((r, i) => (
          <m.button
            key={r.path}
            type="button"
            className="recent"
            onClick={() => void openProjectGuarded(r.path)}
            aria-label={`Open ${r.name}, ${fmtClock(r.durationMs)}, recorded ${fmtRelative(r.createdAt)}${r.interrupted ? ", interrupted" : ""}`}
            initial={{ opacity: 0, y: 8 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ ...softSpring, delay: 0.1 + i * 0.04 }}
            whileHover={{ y: -3 }}
            whileTap={{ scale: 0.98 }}
          >
            <div className="recent__thumb">
              {r.thumbnail ? <img src={r.thumbnail} alt="" style={{ width: "100%", height: "100%", objectFit: "cover" }} /> : <Thumb seed={r.path} kind="window" />}
              <span className="recent__dur">{fmtClock(r.durationMs)}</span>
            </div>
            <div className="recent__name">{r.name}</div>
            <div className="recent__meta">
              {fmtRelative(r.createdAt)}
              {r.interrupted ? " · interrupted" : ""}
            </div>
          </m.button>
        ))}
        {recents && (
          <button type="button" className="recent recent--open" onClick={() => void openFromDisk()} title={isTauri ? "Open a .frostcap project folder" : "Demo: opens a sample project"}>
            <FolderOpen size={22} aria-hidden="true" />
            <span className="recent__name">Open project…</span>
          </button>
        )}
      </div>
      </div>
    </div>
  );
}

type BoolKey = { [K in keyof Settings]: Settings[K] extends boolean ? K : never }[keyof Settings];

function Opt({
  icon,
  label,
  hint,
  k,
  settings,
  set,
}: {
  icon: ReactNode;
  label: string;
  hint: string;
  k: BoolKey;
  settings: Settings;
  set: <K extends keyof Settings>(k: K, v: Settings[K]) => void;
}) {
  const id = `opt-${k}`;
  return (
    <div className="optrow">
      <span className="optrow__icon" aria-hidden="true">
        {icon}
      </span>
      <label className="optrow__text" htmlFor={id}>
        <span className="optrow__label" style={{ display: "block" }}>{label}</span>
        <span className="optrow__hint" style={{ display: "block" }}>{hint}</span>
      </label>
      <Switch id={id} label={label} checked={settings[k]} onChange={(v) => set(k, v)} />
    </div>
  );
}
