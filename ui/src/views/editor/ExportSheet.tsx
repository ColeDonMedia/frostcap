import { useEffect, useId, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { m } from "motion/react";
import { X, Download, CheckCircle2, FolderOpen } from "lucide-react";
import { backend, toBackendError, type EncoderCaps } from "../../backend";
import { BatchPanel } from "./BatchPanel";
import { useEditor, outputDuration } from "../../store/editor";
import { useApp } from "../../store/app";
import { Segmented, Select, ToggleRow, softSpring } from "../../components/ui";
import type { AspectPreset, ExportFormat, OutputPreset } from "../../types";
import { fmtClock } from "../../lib/format";
import { trapTab } from "../../lib/focus";
import { t, type MessageKey } from "../../i18n";

const ASPECTS: { value: AspectPreset; label: string; hint: MessageKey; w: number; h: number }[] = [
  { value: "16:9", label: "16:9", hint: "export.hint169", w: 48, h: 27 },
  { value: "9:16", label: "9:16", hint: "export.hint916", w: 22, h: 39 },
  { value: "1:1", label: "1:1", hint: "export.hint11", w: 34, h: 34 },
];

/** Short edge of the output. "raw" = the original recording without edits (legacy preset). */
type Quality = 720 | 1080 | 1440 | 2160 | "raw";
const qualities = (): { value: Quality; label: string }[] => [
  { value: 1080, label: "1080p" },
  { value: 1440, label: "1440p" },
  { value: 2160, label: t("export.q4k") },
  { value: 720, label: "720p" },
  { value: "raw", label: t("export.raw") },
];

/** Backend stage names are translated when known; anything else is shown as sent. */
const KNOWN_STAGES = ["starting", "preparing", "audio", "rendering", "finalizing", "done"];
const stageLabel = (stage: string) => (KNOWN_STAGES.includes(stage) ? t(`export.stage.${stage}` as MessageKey) : stage);

export function dims(aspect: AspectPreset, short: number) {
  const long = Math.round((short * 16) / 9 / 2) * 2;
  if (aspect === "16:9") return { width: long, height: short };
  if (aspect === "9:16") return { width: short, height: long };
  return { width: short, height: short };
}

type JobState = { jobId: string; outPath: string; progress: number; stage: string; status: "running" | "done" | "error" | "cancelled"; message?: string; bytes?: number };

let jobSeq = 0;

export function ExportSheet({ aspect: initialAspect, onClose }: { aspect: AspectPreset; onClose: () => void }) {
  const path = useEditor((s) => s.path)!;
  const project = useEditor((s) => s.project)!;
  const reportIssue = useApp((s) => s.reportIssue);
  const [aspect, setAspect] = useState<AspectPreset>(initialAspect);
  const [quality, setQuality] = useState<Quality>(1080);
  const [fps, setFps] = useState(60);
  const [format, setFormat] = useState<ExportFormat>("mp4");
  const [outPath, setOutPath] = useState<string | null>(null);
  const [job, setJob] = useState<JobState | null>(null);
  const [caps, setCaps] = useState<EncoderCaps | null>(null);
  const [batch, setBatch] = useState(false);
  const [batchRunning, setBatchRunning] = useState(false);
  const dialog = useRef<HTMLDivElement>(null);
  const aspectRef = useRef<HTMLDivElement>(null);
  const titleId = useId();

  useEffect(() => {
    backend.encoderCapabilities().then(setCaps, () => setCaps(null));
  }, []);
  // A saved AV1 choice falls back to MP4 when this PC cannot encode AV1.
  useEffect(() => {
    if (caps && format === "av1" && !caps.formats.includes("av1")) setFormat("mp4");
  }, [caps, format]);
  /** Formats the encoder supports; AV1 only when the backend lists it. */
  const formats: ExportFormat[] = (caps?.formats ?? ["mp4", "gif"]).filter((f): f is ExportFormat => f === "mp4" || f === "gif" || f === "av1");

  const running = job?.status === "running" || batchRunning;
  const raw = quality === "raw";
  const duration = outputDuration(project);
  const gif = format === "gif";
  // GIF: <= 24 fps and <= 1080 short edge (backend caps further).
  useEffect(() => {
    if (gif) {
      setFps((f) => Math.min(f, 24));
      setQuality((q) => (q === "raw" || q > 1080 ? 720 : q));
    }
  }, [gif]);
  // Picked path must match the format's extension.
  useEffect(() => setOutPath((p) => (p && !p.toLowerCase().endsWith(`.${format === "gif" ? "gif" : "mp4"}`) ? null : p)), [format]);

  const d = raw ? { width: project.media.width, height: project.media.height } : dims(aspect, quality);
  const preset: OutputPreset | "raw" = raw ? "raw" : { aspect, width: d.width, height: d.height, fps, format };

  // Focus trap + Escape (ignored while exporting; use Cancel export).
  useEffect(() => {
    const el = dialog.current!;
    if (!el.contains(document.activeElement)) el.querySelector<HTMLElement>("[data-autofocus]")?.focus();
    const focusables = () =>
      Array.from(el.querySelectorAll<HTMLElement>('button:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex="0"]')).filter(
        (x) => x.offsetParent !== null || x === document.activeElement,
      );
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !running) {
        e.preventDefault();
        onClose();
        return;
      }
      if (e.key !== "Tab") return;
      const f = focusables();
      const target = trapTab(f.length, f.indexOf(document.activeElement as HTMLElement), e.shiftKey);
      if (target === null) return;
      e.preventDefault();
      (target < 0 ? el : f[target]).focus();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [running]);
  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null;
    return () => opener?.focus?.();
  }, []);
  // Controls get disabled during an export: keep focus inside the dialog.
  useEffect(() => {
    if (running && !dialog.current?.contains(document.activeElement)) dialog.current?.focus();
  }, [running]);

  const onAspectKey = (e: ReactKeyboardEvent) => {
    const i = ASPECTS.findIndex((a) => a.value === aspect);
    let n = i;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") n = (i + 1) % ASPECTS.length;
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp") n = (i - 1 + ASPECTS.length) % ASPECTS.length;
    else if (e.key === "Home") n = 0;
    else if (e.key === "End") n = ASPECTS.length - 1;
    else return;
    e.preventDefault();
    e.stopPropagation();
    setAspect(ASPECTS[n].value);
    aspectRef.current?.querySelectorAll<HTMLButtonElement>("[role=radio]")[n]?.focus();
  };

  const safeName = project.name.replace(/[\\/:*?"<>|]+/g, "-").slice(0, 120);

  /** The backend only accepts export targets chosen in its save dialog. */
  const choose = async (): Promise<string | null> => {
    try {
      const p = await backend.pickSavePath(safeName, format === "gif" ? "gif" : "mp4");
      if (p) setOutPath(p);
      return p;
    } catch (e) {
      reportIssue(e, t("export.errChoose"));
      return null;
    }
  };

  const run = async () => {
    const target = outPath ?? (await choose());
    if (!target) return;
    const jobId = `ui-${Date.now().toString(36)}-${++jobSeq}`;
    setJob({ jobId, outPath: target, progress: 0, stage: "starting", status: "running" });
    const un = await backend.onExportProgress((ev) => {
      if (ev.jobId === jobId) setJob((j) => (j && j.jobId === jobId && j.status === "running" ? { ...j, progress: ev.progress, stage: ev.stage } : j));
    });
    try {
      await useEditor.getState().save(); // render what the user sees
      const res = await backend.exportProject({ path, preset, outPath: target, jobId });
      setJob((j) => ({ jobId, outPath: res.outPath, progress: 1, stage: "done", status: j?.status === "cancelled" ? "cancelled" : "done", bytes: res.bytes }));
    } catch (e) {
      const be = toBackendError(e);
      const cancelled = /cancel/i.test(be.message);
      setJob((j) => ({ jobId, outPath: target, progress: j?.progress ?? 0, stage: "", status: cancelled ? "cancelled" : "error", message: be.message }));
      if (!cancelled) reportIssue(be, t("export.errFailed"));
    } finally {
      un();
    }
  };

  const cancel = async () => {
    if (!job) return;
    try {
      await backend.cancelExport(job.jobId);
    } catch (e) {
      reportIssue(e, t("export.errCancel"));
    }
  };

  const reveal = async () => {
    if (!job) return;
    try {
      await backend.revealInFolder(job.outPath);
    } catch (e) {
      reportIssue(e, t("export.errReveal"));
    }
  };

  const pct = Math.round((job?.progress ?? 0) * 100);

  return (
    <m.div className="scrim" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} onPointerDown={(e) => e.target === e.currentTarget && !running && onClose()}>
      <m.div
        ref={dialog}
        className="sheet glass glass--strong"
        role="dialog"
        data-testid="export-sheet"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-busy={running || undefined}
        tabIndex={-1}
        initial={{ opacity: 0, y: 24, scale: 0.97 }}
        animate={{ opacity: 1, y: 0, scale: 1 }}
        exit={{ opacity: 0, y: 24, scale: 0.97 }}
        transition={softSpring}
      >
        <div className="sheet__head">
          <div>
            <h2 className="sheet__title" id={titleId}>
              {t("export.title")}
            </h2>
            <p className="page__sub" style={{ marginTop: 2 }}>
              {project.name} · {fmtClock(duration)}
            </p>
          </div>
          <button type="button" className="iconbtn" aria-label={t("export.close")} onClick={onClose} disabled={running}>
            <X size={18} />
          </button>
        </div>

        <div className="field">
          <span className="field__label" id="aspect-l">
            {t("export.aspect")}
          </span>
          <div className="aspects" role="radiogroup" aria-labelledby="aspect-l" ref={aspectRef} onKeyDown={running || raw || batch ? undefined : onAspectKey}>
            {ASPECTS.map((a) => {
              const sel = aspect === a.value;
              return (
                <button
                  key={a.value}
                  type="button"
                  role="radio"
                  aria-checked={sel}
                  tabIndex={sel ? 0 : -1}
                  className="aspect"
                  data-autofocus={sel ? true : undefined}
                  disabled={running || raw || batch}
                  onClick={() => setAspect(a.value)}
                >
                  <span className="aspect__shape" style={{ width: a.w, height: a.h }} aria-hidden="true" />
                  {a.label}
                  <small>{t(a.hint)}</small>
                </button>
              );
            })}
          </div>
        </div>

        <div className="row2">
          <Select<Quality>
            label={t("export.resolution")}
            value={quality}
            onChange={setQuality}
            disabled={running}
            options={qualities().filter((q) => !gif || (q.value !== "raw" && q.value <= 1080))}
          />
          <Select<number>
            label={t("export.fps")}
            value={fps}
            onChange={setFps}
            disabled={running || raw}
            options={(gif ? [10, 15, 24] : [24, 30, 60]).map((f) => ({ value: f, label: t("recorder.fpsOption", { fps: f }) }))}
          />
        </div>

        <div className="field">
          <span className="field__label">{t("export.format")}</span>
          <Segmented<ExportFormat>
            label={t("export.format")}
            value={format}
            onChange={setFormat}
            disabled={running || raw || batch}
            options={formats.map((f) => ({ value: f, label: t(f === "mp4" ? "export.formatMp4" : f === "gif" ? "export.formatGif" : "export.av1") }))}
          />
          {caps && !caps.formats.includes("av1") && (
            <span className="field__hint" data-testid="av1-hint">
              {t("export.av1Hint")}
            </span>
          )}
          {batch && <span className="field__hint">{t("export.batchMode")}</span>}
          <ToggleRow label={t("export.batchToggle")} checked={batch} onChange={setBatch} />
        </div>

        {batch ? (
          <BatchPanel formats={formats} quality={raw ? 1080 : (quality as number)} fps={fps} dims={dims} disabled={raw} onRunning={setBatchRunning} />
        ) : (
          <>
        <div className="field">
          <label className="field__label" htmlFor="export-out">
            {t("export.saveTo")}
          </label>
          <div style={{ display: "flex", gap: 8 }}>
            <input
              id="export-out"
              className="input"
              readOnly
              disabled={running}
              value={outPath ?? ""}
              placeholder={t("export.placeholder")}
              style={{ flex: 1, fontFamily: "var(--mono)", fontSize: 12.5 }}
            />
            <button type="button" className="btn" onClick={() => void choose()} disabled={running}>
              {t("export.choose")}
            </button>
          </div>
          <span className="field__hint">
            {t("export.summary", { width: d.width, height: d.height, detail: raw ? t("export.originalFile", { fps: project.media.fps }) : t("export.rateAspect", { fps, aspect }) })}
          </span>
        </div>

        {/* Always rendered so the dialog does not jump when an export starts. */}
        <div className="field export-status" aria-hidden={job ? undefined : "true"} style={{ visibility: job ? "visible" : "hidden" }}>
          <div className="field__row" style={{ minHeight: 0 }} aria-live="polite">
            <span className="field__label">
              {job?.status === "done" ? (
                <span style={{ display: "inline-flex", alignItems: "center", gap: 6, color: "var(--ok)" }}>
                  <CheckCircle2 size={15} aria-hidden="true" /> {t("export.exported")}
                </span>
              ) : job?.status === "error" ? (
                <span style={{ color: "var(--danger)" }}>{t("export.failed")}</span>
              ) : job?.status === "cancelled" ? (
                t("export.cancelled")
              ) : (
                job?.stage ? t("export.exportingStage", { stage: stageLabel(job.stage) }) : t("export.exporting")
              )}
            </span>
            <span className="field__value">{pct}%</span>
          </div>
          <div className="progress" role="progressbar" aria-label={t("export.progress")} aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct}>
            <m.div className="progress__bar" animate={{ width: `${pct}%` }} transition={{ type: "spring", stiffness: 200, damping: 30 }} />
          </div>
          <span className="field__hint export-status__detail">
            {job?.status === "done" ? `${job.outPath}${job.bytes ? ` · ${(job.bytes / 1e6).toFixed(1)} MB` : ""}` : job?.status === "error" ? job.message : " "}
          </span>
        </div>

          </>
        )}

        <div className="sheet__foot">
          {job?.status === "done" && (
            <button type="button" className="btn" onClick={() => void reveal()}>
              <FolderOpen size={15} aria-hidden="true" /> {t("export.showFolder")}
            </button>
          )}
          {running && !batch ? (
            <button type="button" className="btn" onClick={() => void cancel()}>
              {t("export.cancelExport")}
            </button>
          ) : (
            <button type="button" className="btn" onClick={onClose} disabled={running}>
              {job?.status === "done" || batch ? t("common.close") : t("common.cancel")}
            </button>
          )}
          {!batch && (
          <button type="button" className="btn btn--primary" onClick={() => void run()} disabled={running}>
            <Download size={15} aria-hidden="true" />
            {running ? t("export.running") : job?.status === "done" ? t("export.again") : t("export.title")}
          </button>
          )}
        </div>
      </m.div>
    </m.div>
  );
}
