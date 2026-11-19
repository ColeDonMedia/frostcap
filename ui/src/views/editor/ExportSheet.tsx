import { useEffect, useId, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { m } from "motion/react";
import { X, Download, CheckCircle2, FolderOpen } from "lucide-react";
import { backend, toBackendError } from "../../backend";
import { useEditor, outputDuration } from "../../store/editor";
import { useApp } from "../../store/app";
import { Segmented, Select, softSpring } from "../../components/ui";
import type { AspectPreset, ExportFormat, OutputPreset } from "../../types";
import { fmtClock } from "../../lib/format";
import { trapTab } from "../../lib/focus";

const ASPECTS: { value: AspectPreset; label: string; hint: string; w: number; h: number }[] = [
  { value: "16:9", label: "16:9", hint: "YouTube, docs", w: 48, h: 27 },
  { value: "9:16", label: "9:16", hint: "Shorts, Reels", w: 22, h: 39 },
  { value: "1:1", label: "1:1", hint: "Social feed", w: 34, h: 34 },
];

/** Short edge of the output. "raw" = the original recording without edits (legacy preset). */
type Quality = 720 | 1080 | 1440 | 2160 | "raw";
const QUALITIES: { value: Quality; label: string }[] = [
  { value: 1080, label: "1080p" },
  { value: 1440, label: "1440p" },
  { value: 2160, label: "4K" },
  { value: 720, label: "720p" },
  { value: "raw", label: "Original recording (no edits)" },
];

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
  const dialog = useRef<HTMLDivElement>(null);
  const aspectRef = useRef<HTMLDivElement>(null);
  const titleId = useId();

  const running = job?.status === "running";
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
  useEffect(() => setOutPath((p) => (p && !p.toLowerCase().endsWith(`.${format}`) ? null : p)), [format]);

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
      const p = await backend.pickSavePath(safeName, format);
      if (p) setOutPath(p);
      return p;
    } catch (e) {
      reportIssue(e, "Choose location");
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
      if (!cancelled) reportIssue(be, "Export failed");
    } finally {
      un();
    }
  };

  const cancel = async () => {
    if (!job) return;
    try {
      await backend.cancelExport(job.jobId);
    } catch (e) {
      reportIssue(e, "Cancel");
    }
  };

  const reveal = async () => {
    if (!job) return;
    try {
      await backend.revealInFolder(job.outPath);
    } catch (e) {
      reportIssue(e, "Show in folder");
    }
  };

  const pct = Math.round((job?.progress ?? 0) * 100);

  return (
    <m.div className="scrim" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} onPointerDown={(e) => e.target === e.currentTarget && !running && onClose()}>
      <m.div
        ref={dialog}
        className="sheet glass glass--strong"
        role="dialog"
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
              Export
            </h2>
            <p className="page__sub" style={{ marginTop: 2 }}>
              {project.name} · {fmtClock(duration)}
            </p>
          </div>
          <button type="button" className="iconbtn" aria-label="Close export" onClick={onClose} disabled={running}>
            <X size={18} />
          </button>
        </div>

        <div className="field">
          <span className="field__label" id="aspect-l">
            Aspect ratio
          </span>
          <div className="aspects" role="radiogroup" aria-labelledby="aspect-l" ref={aspectRef} onKeyDown={running || raw ? undefined : onAspectKey}>
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
                  disabled={running || raw}
                  onClick={() => setAspect(a.value)}
                >
                  <span className="aspect__shape" style={{ width: a.w, height: a.h }} aria-hidden="true" />
                  {a.label}
                  <small>{a.hint}</small>
                </button>
              );
            })}
          </div>
        </div>

        <div className="row2">
          <Select<Quality>
            label="Resolution"
            value={quality}
            onChange={setQuality}
            disabled={running}
            options={QUALITIES.filter((q) => !gif || (q.value !== "raw" && q.value <= 1080))}
          />
          <Select<number>
            label="Frame rate"
            value={fps}
            onChange={setFps}
            disabled={running || raw}
            options={(gif ? [10, 15, 24] : [24, 30, 60]).map((f) => ({ value: f, label: `${f} fps` }))}
          />
        </div>

        <div className="field">
          <span className="field__label">Format</span>
          <Segmented<ExportFormat>
            label="Format"
            value={format}
            onChange={setFormat}
            disabled={running || raw}
            options={[
              { value: "mp4", label: "MP4 (H.264)" },
              { value: "gif", label: "GIF" },
            ]}
          />
        </div>

        <div className="field">
          <label className="field__label" htmlFor="export-out">
            Save to
          </label>
          <div style={{ display: "flex", gap: 8 }}>
            <input
              id="export-out"
              className="input"
              readOnly
              disabled={running}
              value={outPath ?? ""}
              placeholder="Choose a location when you export"
              style={{ flex: 1, fontFamily: "var(--mono)", fontSize: 12.5 }}
            />
            <button type="button" className="btn" onClick={() => void choose()} disabled={running}>
              Choose…
            </button>
          </div>
          <span className="field__hint">
            {d.width}×{d.height} · {raw ? `${project.media.fps} fps · original file` : `${fps} fps · ${aspect}`}
          </span>
        </div>

        {/* Always rendered so the dialog does not jump when an export starts. */}
        <div className="field export-status" aria-hidden={job ? undefined : "true"} style={{ visibility: job ? "visible" : "hidden" }}>
          <div className="field__row" style={{ minHeight: 0 }} aria-live="polite">
            <span className="field__label">
              {job?.status === "done" ? (
                <span style={{ display: "inline-flex", alignItems: "center", gap: 6, color: "var(--ok)" }}>
                  <CheckCircle2 size={15} aria-hidden="true" /> Exported
                </span>
              ) : job?.status === "error" ? (
                <span style={{ color: "var(--danger)" }}>Export failed</span>
              ) : job?.status === "cancelled" ? (
                "Cancelled"
              ) : (
                `Exporting${job?.stage ? ` · ${job.stage}` : ""}`
              )}
            </span>
            <span className="field__value">{pct}%</span>
          </div>
          <div className="progress" role="progressbar" aria-label="Export progress" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct}>
            <m.div className="progress__bar" animate={{ width: `${pct}%` }} transition={{ type: "spring", stiffness: 200, damping: 30 }} />
          </div>
          <span className="field__hint export-status__detail">
            {job?.status === "done" ? `${job.outPath}${job.bytes ? ` · ${(job.bytes / 1e6).toFixed(1)} MB` : ""}` : job?.status === "error" ? job.message : " "}
          </span>
        </div>

        <div className="sheet__foot">
          {job?.status === "done" && (
            <button type="button" className="btn" onClick={() => void reveal()}>
              <FolderOpen size={15} aria-hidden="true" /> Show in folder
            </button>
          )}
          {running ? (
            <button type="button" className="btn" onClick={() => void cancel()}>
              Cancel export
            </button>
          ) : (
            <button type="button" className="btn" onClick={onClose}>
              {job?.status === "done" ? "Close" : "Cancel"}
            </button>
          )}
          <button type="button" className="btn btn--primary" onClick={() => void run()} disabled={running}>
            <Download size={15} aria-hidden="true" />
            {running ? "Exporting…" : job?.status === "done" ? "Export again" : "Export"}
          </button>
        </div>
      </m.div>
    </m.div>
  );
}
