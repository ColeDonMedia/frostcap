import { useEffect, useMemo, useState } from "react";
import { MediaEngine } from "../../lib/media";
import { isTypingTarget } from "../../lib/keys";
import { AnimatePresence } from "motion/react";
import { ArrowLeft, Download, Redo2, RotateCcw, Save, Undo2 } from "lucide-react";
import { describeIssue, toBackendError } from "../../backend";
import { useEditor } from "../../store/editor";
import { useApp } from "../../store/app";
import { Preview } from "./Preview";
import { Transport } from "./Transport";
import { Timeline } from "./Timeline";
import { Inspector } from "./Inspector";
import { ExportSheet } from "./ExportSheet";
import type { AspectPreset } from "../../types";
import { Segmented } from "../../components/ui";
import { speedAt } from "../../lib/speed";
import { t } from "../../i18n";

const isTyping = isTypingTarget;

export default function EditorView({ path }: { path: string }) {
  const load = useEditor((s) => s.load);
  const project = useEditor((s) => s.project);
  const loading = useEditor((s) => s.loading);
  const error = useEditor((s) => s.error);
  const errorCode = useEditor((s) => s.errorCode);
  const playing = useEditor((s) => s.playing);
  const canUndo = useEditor((s) => s.past.length > 0);
  const canRedo = useEditor((s) => s.future.length > 0);
  const dirty = useEditor((s) => s.dirty);
  const saving = useEditor((s) => s.saving);
  const [exporting, setExporting] = useState(false);
  const [aspect, setAspect] = useState<AspectPreset>("16:9");

  useEffect(() => {
    void load(path);
  }, [path, load]);

  // Real media (Tauri): one engine per loaded recording. `media` keeps its identity across edits.
  const media = project?.media;
  const engine = useMemo(() => (media ? new MediaEngine(media) : null), [media]);
  useEffect(() => () => engine?.destroy(), [engine]);
  const gains = useEditor((s) => s.project?.style.audio);
  useEffect(() => {
    if (engine && gains) engine.setGains(gains.micGain, gains.systemGain);
  }, [engine, gains]);

  // While paused, follow the playhead (scrubbing, arrows, clicks) with the video.
  useEffect(() => {
    if (!engine?.hasVideo) return;
    let raf = 0;
    let last = -1;
    return useEditor.subscribe((s) => {
      if (s.playing || s.playhead === last) return;
      last = s.playhead;
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(() => engine.seek(last));
    });
  }, [engine]);

  // Playback loop. With real video the <video> element is the clock; otherwise wall time.
  // Trimmed gaps are skipped in both cases.
  useEffect(() => {
    if (!playing) return;
    const video = engine?.hasVideo ? engine : null;
    let raf = 0;
    let last = performance.now();
    if (video) void video.play(useEditor.getState().playhead);
    const step = (now: number) => {
      const st = useEditor.getState();
      const p = st.project;
      if (!p || !st.playing) return;
      const sp = speedAt(p.speed, st.playhead);
      video?.setRate(sp);
      let t = video ? video.timeMs() : st.playhead + (now - last) * sp;
      last = now;
      const clips = p.clips;
      const inClip = clips.find((c) => t >= c.start && t < c.end);
      if (!inClip) {
        const next = clips.find((c) => c.start > t - 1);
        if (!next) {
          useEditor.setState({ playhead: clips[clips.length - 1]?.end ?? t, playing: false });
          return;
        }
        t = next.start;
        video?.seek(t);
      }
      video?.syncAudio(t);
      useEditor.setState({ playhead: t });
      raf = requestAnimationFrame(step);
    };
    raf = requestAnimationFrame(step);
    return () => {
      cancelAnimationFrame(raf);
      video?.pause();
    };
  }, [playing, engine]);

  // Editor shortcuts
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (exporting) return;
      const st = useEditor.getState();
      if (!st.project) return;
      const ctrl = e.ctrlKey || e.metaKey;
      const key = e.key.toLowerCase();
      if (ctrl && key === "z" && !e.shiftKey) {
        if (isTyping(e.target)) return;
        e.preventDefault();
        st.undo();
        return;
      }
      if (ctrl && (key === "y" || (key === "z" && e.shiftKey))) {
        if (isTyping(e.target)) return;
        e.preventDefault();
        st.redo();
        return;
      }
      if (ctrl && key === "s") {
        e.preventDefault();
        void st.save().then(() => useApp.getState().showToast(t("editor.saved")));
        return;
      }
      if (ctrl && key === "e") {
        e.preventDefault();
        setExporting(true);
        return;
      }
      if (ctrl || e.altKey || isTyping(e.target)) return;
      const role = (e.target as HTMLElement | null)?.getAttribute?.("role");
      const frame = 1000 / (st.project.media.fps || 60);
      switch (e.key) {
        case " ": {
          // Let buttons/radios keep their own Space activation.
          const tag = (e.target as HTMLElement | null)?.tagName;
          if (tag === "BUTTON" || role === "radio" || role === "switch" || role === "tab") return;
          e.preventDefault();
          st.togglePlay();
          break;
        }
        case "k":
        case "K":
          e.preventDefault();
          st.togglePlay();
          break;
        case "s":
        case "S":
          e.preventDefault();
          if (!st.splitAtPlayhead()) useApp.getState().showToast(t("editor.splitFail"));
          break;
        case "z":
        case "Z":
          e.preventDefault();
          st.addZoomAtPlayhead();
          break;
        case "Delete":
        case "Backspace":
          if (st.selection) {
            e.preventDefault();
            st.deleteSelection();
          }
          break;
        case "ArrowLeft":
        case "ArrowRight": {
          if (role === "slider" || role === "radio" || role === "tab") return;
          e.preventDefault();
          const d = (e.shiftKey ? 1000 : frame) * (e.key === "ArrowLeft" ? -1 : 1);
          st.setPlaying(false);
          st.setPlayhead(st.playhead + d);
          break;
        }
        case "Home":
          e.preventDefault();
          st.setPlayhead(st.project.clips[0]?.start ?? 0);
          break;
        case "End":
          e.preventDefault();
          st.setPlayhead(st.project.clips[st.project.clips.length - 1]?.end ?? 0);
          break;
        case "Escape":
          st.select(null);
          st.setTool(null);
          break;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [exporting]);

  if (error)
    return (
      <div className="loading">
        <div className="load-error glass" role="alert">
          <h2 className="sheet__title">{t("editor.loadError")}</h2>
          <p className="page__sub" style={{ margin: 0 }}>{describeIssue(toBackendError({ code: errorCode ?? "internal", message: error }))}</p>
          <div className="sheet__foot">
            <button
              type="button"
              className="btn"
              onClick={() => {
                const prev = useEditor.getState();
                prev.clearError();
                if (prev.project && prev.path) useApp.getState().openProject(prev.path);
                else useApp.setState({ view: "recorder", projectPath: null });
              }}
            >
              <ArrowLeft size={15} aria-hidden="true" /> {t("common.back")}
            </button>
            <button type="button" className="btn btn--primary" onClick={() => void useEditor.getState().retry()}>
              <RotateCcw size={15} aria-hidden="true" /> {t("common.retry")}
            </button>
          </div>
        </div>
      </div>
    );
  if (loading || !project)
    return (
      <div className="loading">
        <div className="spinner" role="status" aria-label={t("editor.loadingProject")} />
      </div>
    );

  return (
    <div className="editor">
      <div className="stage">
        <div className="toolbar glass" role="toolbar" aria-label={t("editor.toolbar")}>
          <div className="toolbar__title" title={project.name}>
            {project.name}
            {dirty && (
              <span className="badge" style={{ marginLeft: 8 }} aria-label={t("editor.unsaved")}>
                {t("editor.edited")}
              </span>
            )}
          </div>
          <Segmented<AspectPreset>
            label={t("editor.aspect")}
            value={aspect}
            onChange={setAspect}
            options={[
              { value: "16:9", label: "16:9" },
              { value: "9:16", label: "9:16" },
              { value: "1:1", label: "1:1" },
            ]}
          />
          <span className="toolbar__sep" aria-hidden="true" />
          <button type="button" className="iconbtn" aria-label={t("common.undo")} title={t("editor.undoTitle")} disabled={!canUndo} onClick={() => useEditor.getState().undo()}>
            <Undo2 size={17} />
          </button>
          <button type="button" className="iconbtn" aria-label={t("common.redo")} title={t("editor.redoTitle")} disabled={!canRedo} onClick={() => useEditor.getState().redo()}>
            <Redo2 size={17} />
          </button>
          <button
            type="button"
            className="iconbtn"
            aria-label={t("editor.save")}
            title={t("editor.saveTitle")}
            disabled={saving}
            onClick={() => void useEditor.getState().save().then(() => useApp.getState().showToast(t("editor.saved")))}
          >
            <Save size={17} />
          </button>
          <button type="button" className="btn btn--primary" data-testid="editor-export" onClick={() => setExporting(true)} title={t("editor.exportTitle")}>
            <Download size={15} aria-hidden="true" /> {t("editor.export")}
          </button>
        </div>
        <Preview aspect={aspect} engine={engine} />
        <Transport />
      </div>
      <Inspector />
      <Timeline />
      <AnimatePresence>{exporting && <ExportSheet aspect={aspect} onClose={() => setExporting(false)} />}</AnimatePresence>
    </div>
  );
}
