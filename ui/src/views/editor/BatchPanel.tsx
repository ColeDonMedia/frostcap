import { useState } from "react";
import { CheckCircle2, AlertTriangle, Download, FolderOpen, RotateCcw } from "lucide-react";
import { backend, toBackendError, type BatchJob, type BatchResult } from "../../backend";
import { useApp } from "../../store/app";
import { useEditor } from "../../store/editor";
import { t, type MessageKey } from "../../i18n";
import type { AspectPreset, ExportFormat } from "../../types";

const ASPECTS: AspectPreset[] = ["16:9", "9:16", "1:1"];
const FORMAT_KEY: Record<ExportFormat, MessageKey> = { mp4: "export.formatMp4", gif: "export.formatGif", av1: "export.av1" };
const MAX_JOBS = 8;

type Row = { aspect: AspectPreset; format: ExportFormat; job: BatchJob; status: "waiting" | "running" | "done" | "error"; progress: number; bytes?: number; message?: string };

let seq = 0;
/** Files this session already wrote: a re-run gets a numeric suffix instead of silently overwriting them (the UI cannot ask the disk). */
const exported = new Set<string>();

/** A pill-style checkbox (28px+ target) instead of the native 13px box. */
function Pill({ label, checked, disabled, onChange }: { label: string; checked: boolean; disabled: boolean; onChange: () => void }) {
  return (
    <button type="button" role="checkbox" aria-checked={checked} className="pill" disabled={disabled} onClick={onChange}>
      {label}
    </button>
  );
}

/** Several presets and formats in one go (export_batch): per-job progress and result. */
export function BatchPanel({
  formats,
  quality,
  fps,
  dims,
  disabled,
  onRunning,
}: {
  formats: ExportFormat[];
  /** Short edge in px (the "original recording" choice counts as 1080). */
  quality: number;
  fps: number;
  dims: (aspect: AspectPreset, short: number) => { width: number; height: number };
  disabled: boolean;
  onRunning: (running: boolean) => void;
}) {
  const [aspects, setAspects] = useState<Set<AspectPreset>>(new Set(["16:9", "9:16"]));
  const [fmts, setFmts] = useState<Set<ExportFormat>>(new Set(["mp4"]));
  const [rows, setRows] = useState<Row[] | null>(null);
  const [dir, setDir] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const [jobId, setJobId] = useState<string | null>(null);

  const combos = ASPECTS.filter((a) => aspects.has(a)).flatMap((a) => formats.filter((f) => fmts.has(f)).map((f) => [a, f] as const));
  const tooMany = combos.length > MAX_JOBS;
  const toggle = <T,>(set: Set<T>, v: T, put: (s: Set<T>) => void) => {
    const n = new Set(set);
    if (n.has(v)) n.delete(v);
    else n.add(v);
    put(n);
  };

  /** Runs the given rows (all of them, or only the failed ones on retry) as one export_batch. */
  const exec = async (all: Row[], only: number[]) => {
    const st = useEditor.getState();
    const id = `ui-batch-${Date.now().toString(36)}-${++seq}`;
    setJobId(id);
    setRows(all.map((r, i) => (only.includes(i) ? { ...r, status: "waiting", progress: 0, message: undefined } : r)));
    setRunning(true);
    onRunning(true);
    let un: () => void = () => undefined;
    try {
      un = await backend.onExportProgress((ev) => {
        if (ev.jobId !== id || ev.jobIndex === undefined) return;
        const target = only[ev.jobIndex];
        setRows((rs) => rs && rs.map((r, i) => (i === target && r.status !== "done" && r.status !== "error" ? { ...r, status: "running", progress: ev.progress } : r)));
      });
      await st.save(); // render what the user sees
      const res = await backend.exportBatch({ projectPath: st.path!, jobs: only.map((i) => all[i].job), jobId: id });
      for (const x of res.results) if (x.ok) exported.add(x.outPath.toLowerCase());
      setRows((rs) => rs && rs.map((r, i) => (only.includes(i) ? apply(r, res.results.find((x) => x.index === only.indexOf(i))) : r)));
    } catch (e) {
      const be = toBackendError(e);
      useApp.getState().reportIssue(be, t("export.errFailed"));
      setRows((rs) => rs && rs.map((r, i) => (only.includes(i) ? { ...r, status: "error", message: be.message } : r)));
    } finally {
      un();
      setRunning(false);
      setJobId(null);
      onRunning(false);
    }
  };

  const start = async () => {
    const st = useEditor.getState();
    const name = st.project!.name.replace(/[\\/:*?"<>|]+/g, "-").slice(0, 100);
    const base = await backend.defaultOutputDir();
    setDir(base);
    const sep = base.includes("\\") ? "\\" : "/";
    const used = new Set<string>(exported);
    const list: Row[] = combos.map(([aspect, format]) => {
      const ext = format === "gif" ? "gif" : "mp4";
      let out = `${base.replace(/[\\/]+$/, "")}${sep}${name}-${aspect.replace(":", "x")}-${format}.${ext}`;
      for (let n = 2; used.has(out.toLowerCase()); n++) out = out.replace(/(-\d+)?\.(mp4|gif)$/, `-${n}.${ext}`);
      used.add(out.toLowerCase());
      const gif = format === "gif";
      const d = dims(aspect, gif ? Math.min(quality, 720) : quality);
      return { aspect, format, job: { outPath: out, preset: { aspect, width: d.width, height: d.height, fps: gif ? Math.min(fps, 24) : fps, format } }, status: "waiting", progress: 0 };
    });
    await exec(list, list.map((_, i) => i));
  };

  const failed = rows?.map((r, i) => (r.status === "error" ? i : -1)).filter((i) => i >= 0) ?? [];
  const doneRows = rows?.filter((r) => r.status === "done") ?? [];
  const finished = !!rows && !running && rows.every((r) => r.status === "done" || r.status === "error");

  return (
    <div className="field" data-testid="batch-panel">
      <span className="field__label" id="batch-aspects">
        {t("export.batchAspects")}
      </span>
      <div className="batch-checks" role="group" aria-labelledby="batch-aspects">
        {ASPECTS.map((a) => (
          <Pill key={a} label={a} checked={aspects.has(a)} disabled={disabled || running} onChange={() => toggle(aspects, a, setAspects)} />
        ))}
      </div>
      <span className="field__label" id="batch-formats">
        {t("export.batchFormats")}
      </span>
      <div className="batch-checks" role="group" aria-labelledby="batch-formats">
        {formats.map((f) => (
          <Pill key={f} label={t(FORMAT_KEY[f])} checked={fmts.has(f)} disabled={disabled || running} onChange={() => toggle(fmts, f, setFmts)} />
        ))}
      </div>
      {combos.length === 0 && <span className="field__hint">{t("export.batchPickOne")}</span>}
      {tooMany && (
        <span className="field__error" role="alert">
          {t("export.batchTooMany")}
        </span>
      )}
      {dir && <span className="field__hint">{t("export.batchFolder", { dir })}</span>}

      {rows && (
        <ul className="batch-jobs" aria-live="polite">
          {rows.map((r, i) => (
            <li key={i} className={`batch-job batch-job--${r.status}`} data-testid="batch-job">
              <span className="batch-job__name">{t("export.jobLabel", { aspect: r.aspect, format: t(FORMAT_KEY[r.format]) })}</span>
              <span className="batch-job__state">
                {r.status === "done" ? (
                  <>
                    <CheckCircle2 size={14} aria-hidden="true" /> {t("export.jobDone", { size: `${((r.bytes ?? 0) / 1e6).toFixed(1)} MB` })}
                  </>
                ) : r.status === "error" ? (
                  <>
                    <AlertTriangle size={14} aria-hidden="true" /> {t("export.jobFailed", { message: r.message ?? "" })}
                  </>
                ) : r.status === "running" ? (
                  `${Math.round(r.progress * 100)}%`
                ) : (
                  t("export.jobWaiting")
                )}
              </span>
              {(r.status === "running" || r.status === "waiting") && (
                <div className="progress" role="progressbar" aria-label={t("export.progress")} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(r.progress * 100)}>
                  <div className="progress__bar" style={{ width: `${Math.round(r.progress * 100)}%` }} />
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
      {finished && <span className="field__hint">{t("export.batchSummary", { ok: doneRows.length, total: rows!.length })}</span>}

      <div className="sheet__foot" style={{ padding: 0 }}>
        {running && jobId ? (
          <button type="button" className="btn" onClick={() => void backend.cancelExport(jobId)}>
            {t("export.cancelExport")}
          </button>
        ) : finished ? (
          <>
            {failed.length > 0 && (
              <button type="button" className="btn" data-testid="batch-retry" onClick={() => void exec(rows!, failed)}>
                <RotateCcw size={15} aria-hidden="true" /> {t("export.retryFailed")}
              </button>
            )}
            {doneRows.length > 0 && (
              <button
                type="button"
                className="btn btn--primary"
                data-testid="batch-reveal"
                onClick={() => void backend.revealInFolder(doneRows[0].job.outPath).catch((e) => useApp.getState().reportIssue(e, t("export.errReveal")))}
              >
                <FolderOpen size={15} aria-hidden="true" /> {t("export.showFolder")}
              </button>
            )}
          </>
        ) : (
          <button type="button" className="btn btn--primary" data-testid="batch-start" disabled={disabled || combos.length === 0 || tooMany} onClick={() => void start()}>
            <Download size={15} aria-hidden="true" /> {t("export.batchStart", { count: combos.length })}
          </button>
        )}
      </div>
    </div>
  );
}

function apply(r: Row, res: BatchResult | undefined): Row {
  if (!res) return { ...r, status: "error", message: "no result" };
  return res.ok ? { ...r, status: "done", progress: 1, bytes: res.bytes } : { ...r, status: "error", message: res.error.message };
}
