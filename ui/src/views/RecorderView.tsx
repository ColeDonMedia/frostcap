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
import { MultiMonitorIcon } from "../components/MultiMonitorIcon";
import { fmtClock, fmtRelative } from "../lib/format";
import { t, tRich } from "../i18n";

type Filter = "all" | SourceKind;

const KIND_ICON: Record<SourceKind, typeof Monitor | typeof MultiMonitorIcon> = { display: Monitor, window: AppWindow, region: Crop, allDisplays: MultiMonitorIcon };

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
        reportIssue(e, t("recorder.errList"));
      });

  useEffect(() => {
    if (phase !== "idle") return;
    void refreshSources();
    backend.listRecentProjects().then(setRecents, (e) => {
      setRecents([]);
      reportIssue(e, t("recorder.errRecent"));
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
        name: t("recorder.regionName"),
        width: r.screenRect.width,
        height: r.screenRect.height,
        wire: { kind: "region", monitorId: r.monitorId, rect: r.rect },
      };
      setRegion(src);
      setSelected(src.id);
      setFilter("all");
    } catch (e) {
      reportIssue(e, t("recorder.errRegion"));
    }
  };

  const openFromDisk = async () => {
    try {
      const p = await backend.pickProject();
      if (p) void openProjectGuarded(p);
    } catch (e) {
      reportIssue(e, t("recorder.errOpen"));
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
        reportIssue(e, t("recorder.recoverFailed", { name: r.name }));
      }
    }
    setRecovering(null);
    if (ok.length === 1) showToast(t("recorder.recoveredOne", { name: ok[0].name }), "info", { label: t("recorder.open"), run: () => void openProjectGuarded(ok[0].path) });
    else if (ok.length > 1) showToast(t("recorder.recoveredMany", { count: ok.length }), "info");
    backend.listRecentProjects().then(setRecents, () => undefined);
  };
  const interrupted = (recents ?? []).filter((r) => r.interrupted);

  const visible = useMemo(() => (sources ?? []).filter((s) => filter === "all" || s.kind === filter || (filter === "display" && s.kind === "allDisplays")), [sources, filter]);
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
          <h1 className="page__title">{t("recorder.title")}</h1>
          <p className="page__sub">{t("recorder.subtitle")}</p>
        </div>
        <Segmented<Filter>
          label={t("recorder.filter")}
          value={filter}
          onChange={setFilter}
          options={[
            { value: "all", label: t("recorder.filterAll") },
            { value: "display", label: t("recorder.filterDisplays"), icon: <Monitor size={14} /> },
            { value: "window", label: t("recorder.filterWindows"), icon: <AppWindow size={14} /> },
          ]}
        />
        <button type="button" className="btn" onClick={() => void pickRegion()} disabled={phase !== "idle"}>
          <Crop size={15} aria-hidden="true" /> {t("recorder.selectRegion")}
        </button>
      </div>

      {interrupted.length > 0 && (
        <div className="banner glass" role="region" aria-label={t("recorder.interruptedRegion")}>
          <RotateCcw size={18} aria-hidden="true" />
          <div className="banner__text">
            <strong>{t("recorder.interrupted", { count: interrupted.length })}</strong>
            <span>{t("recorder.interruptedHint")}</span>
            <ul className="banner__list">
              {interrupted.map((r) => (
                <li key={r.path}>
                  <span className="banner__name">{r.name}</span>
                  <button type="button" className="btn btn--sm" disabled={recovering !== null} onClick={() => void recover([r])} aria-label={t("recorder.recoverNamed", { name: r.name })}>
                    {recovering === r.path ? t("recorder.recovering") : t("recorder.recover")}
                  </button>
                </li>
              ))}
            </ul>
          </div>
          {interrupted.length > 1 && (
            <button type="button" className="btn btn--primary btn--sm" disabled={recovering !== null} onClick={() => void recover(interrupted)}>
              {t("recorder.recoverAll")}
            </button>
          )}
        </div>
      )}

      <div className="rec-layout">
        <section aria-label={t("recorder.captureSource")}>
          {!sources ? (
            <div className="loading" style={{ minHeight: 200 }}>
              <div className="spinner" role="status" aria-label={t("recorder.loadingSources")} />
            </div>
          ) : sources.length === 0 ? (
            <div className="empty-note">
              {t("recorder.noSources")}{" "}
              <button type="button" className="btn btn--sm" onClick={() => void refreshSources()}>
                <RefreshCw size={13} aria-hidden="true" /> {t("common.retry")}
              </button>
            </div>
          ) : (
            <div
              className="sources"
              role="radiogroup"
              aria-label={t("recorder.captureSource")}
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
                      {s.kind === "allDisplays" ? (
                        <div className="srccard__alldisp" aria-hidden="true">
                          <MultiMonitorIcon size={44} strokeWidth={1.4} />
                        </div>
                      ) : s.thumbnail ? (
                        <img src={s.thumbnail} alt="" />
                      ) : (
                        <Thumb seed={s.id} kind={s.kind} />
                      )}
                    </div>
                    {sel && (
                      <m.span className="srccard__check" initial={{ scale: 0 }} animate={{ scale: 1 }} transition={softSpring}>
                        <Check size={14} strokeWidth={3} aria-hidden="true" />
                      </m.span>
                    )}
                    <div className="srccard__name">
                      <Icon size={15} aria-hidden="true" />
                      <span>{s.kind === "allDisplays" ? t("recorder.allDisplays") : s.name}</span>
                    </div>
                    <div className="srccard__meta">
                      {s.kind === "window" && s.app ? `${s.app} · ` : ""}
                      {s.width}×{s.height}
                      {s.primary ? ` · ${t("recorder.primary")}` : ""}
                    </div>
                  </m.button>
                );
              })}
            </div>
          )}
        </section>

        <aside className="panel glass" aria-label={t("recorder.optionsLabel")}>
          <h2 className="panel__title">{t("recorder.captureTitle")}</h2>
          <Opt icon={<Camera size={16} />} label={t("recorder.webcam")} hint={t("recorder.webcamHint")} k="webcam" settings={settings} set={setSetting} />
          {settings.webcam && (
            <div className="optrow optrow--sub">
              {cameras === "unsupported" ? (
                <span className="field__hint">{t("recorder.cameraUnsupported")}</span>
              ) : cameras === null ? (
                <span className="field__hint">{t("recorder.cameraSearching")}</span>
              ) : cameras.length === 0 ? (
                <span className="field__hint">{t("recorder.cameraNone")}</span>
              ) : (
                <div className="optrow__text">
                  <Select<string>
                    label={t("recorder.camera")}
                    value={settings.camera ?? cameras[0].id}
                    onChange={(v) => setSetting("camera", v)}
                    options={cameras.map((cam) => ({ value: cam.id, label: cam.name }))}
                  />
                </div>
              )}
            </div>
          )}
          <Opt icon={<Mic size={16} />} label={t("recorder.mic")} hint={t("recorder.micHint")} k="mic" settings={settings} set={setSetting} />
          <Opt icon={<Volume2 size={16} />} label={t("recorder.systemAudio")} hint={t("recorder.systemAudioHint")} k="systemAudio" settings={settings} set={setSetting} />
          <Opt icon={<MousePointer2 size={16} />} label={t("recorder.cursor")} hint={t("recorder.cursorHint")} k="cursor" settings={settings} set={setSetting} />
          <div className="optrow">
            <span className="optrow__icon" aria-hidden="true">
              <Keyboard size={16} />
            </span>
            <label className="optrow__text" htmlFor="opt-keys">
              <span className="optrow__label" style={{ display: "block" }}>
                {t("recorder.keys")}
              </span>
              <span className="optrow__hint" style={{ display: "block" }}>
                {settings.keyCapture === "all" ? t("recorder.keysAllHint") : t("recorder.keysShortcutsHint")}
              </span>
            </label>
            <Switch
              id="opt-keys"
              label={t("recorder.keys")}
              checked={settings.keyCapture !== "off"}
              onChange={(v) => {
                // Security: the recorder switch only enables Shortcuts; All keys needs Settings + confirmation.
                setSetting("keyCapture", v ? "shortcuts" : "off");
              }}
            />
          </div>
          <Opt icon={<Timer size={16} />} label={t("recorder.countdown")} hint={t("recorder.countdownHint")} k="countdown" settings={settings} set={setSetting} />
          <div className="optrow">
            <span className="optrow__icon" aria-hidden="true">
              <Gauge size={16} />
            </span>
            <div className="optrow__text">
              <Select<number>
                label={t("recorder.fps")}
                value={settings.fps}
                onChange={(v) => setSetting("fps", v)}
                options={[
                  { value: 30, label: t("recorder.fpsOption", { fps: 30 }) },
                  { value: 60, label: t("recorder.fpsOption", { fps: 60 }) },
                  { value: 120, label: t("recorder.fpsHigh", { fps: 120 }) },
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
            {busy ? t("recorder.recording") : t("recorder.start")}
          </m.button>
          <p className="field__hint" style={{ margin: 0, textAlign: "center" }}>
            {source ? (
              <>{tRich("recorder.capturing", { name: source.name, fps: settings.fps }, (s) => <strong key="name" style={{ color: "var(--text)" }}>{s}</strong>)}</>
            ) : (
              t("recorder.selectSource")
            )}
          </p>
        </aside>
      </div>

      <h2 className="h2">
        <Clock size={16} aria-hidden="true" /> {t("recorder.recent")}
      </h2>
      {recents && recents.length === 0 && <div className="empty-note">{t("recorder.noRecent")}</div>}
      <div className="recents">
        {(recents ?? []).map((r, i) => (
          <m.button
            key={r.path}
            type="button"
            className="recent"
            data-testid={`recent-${i}`}
            onClick={() => void openProjectGuarded(r.path)}
            aria-label={t("recorder.recentLabel", { name: r.name, duration: fmtClock(r.durationMs), when: fmtRelative(r.createdAt) }) + (r.interrupted ? t("recorder.recentInterruptedSuffix") : "")}
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
              {r.interrupted ? t("recorder.recentInterrupted") : ""}
            </div>
          </m.button>
        ))}
        {recents && (
          <button type="button" className="recent recent--open" onClick={() => void openFromDisk()} title={isTauri ? t("recorder.openProjectTitle") : t("recorder.openProjectDemo")}>
            <FolderOpen size={22} aria-hidden="true" />
            <span className="recent__name">{t("recorder.openProject")}</span>
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
