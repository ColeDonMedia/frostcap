import { m } from "motion/react";
import { Pause, Play, SkipBack, SkipForward, ChevronLeft, ChevronRight, Scissors, ZoomIn } from "lucide-react";
import { useEditor, outputDuration, sourceToOutput } from "../../store/editor";
import { fmtTime } from "../../lib/format";
import { softSpring } from "../../components/ui";

export function Transport() {
  const playing = useEditor((s) => s.playing);
  const playhead = useEditor((s) => s.playhead);
  const project = useEditor((s) => s.project)!;
  const st = useEditor.getState;
  const frame = 1000 / (project.media.fps || 60);
  const outT = sourceToOutput(project, playhead);
  const total = outputDuration(project);

  return (
    <div className="transport glass" role="toolbar" aria-label="Playback">
      <button type="button" className="iconbtn" aria-label="Jump to start" title="Home" onClick={() => st().setPlayhead(project.clips[0]?.start ?? 0)}>
        <SkipBack size={17} />
      </button>
      <button type="button" className="iconbtn" aria-label="Previous frame" title="←" onClick={() => st().setPlayhead(playhead - frame)}>
        <ChevronLeft size={18} />
      </button>
      <m.button
        type="button"
        className="transport__play"
        aria-label={playing ? "Pause" : "Play"}
        aria-pressed={playing}
        title="Space"
        onClick={() => st().togglePlay()}
        whileHover={{ scale: 1.06 }}
        whileTap={{ scale: 0.92 }}
        transition={softSpring}
      >
        {playing ? <Pause size={18} fill="currentColor" /> : <Play size={18} fill="currentColor" style={{ marginLeft: 2 }} />}
      </m.button>
      <button type="button" className="iconbtn" aria-label="Next frame" title="→" onClick={() => st().setPlayhead(playhead + frame)}>
        <ChevronRight size={18} />
      </button>
      <button
        type="button"
        className="iconbtn"
        aria-label="Jump to end"
        title="End"
        onClick={() => st().setPlayhead(project.clips[project.clips.length - 1]?.end ?? 0)}
      >
        <SkipForward size={17} />
      </button>
      <span className="transport__time" aria-label={`Position ${fmtTime(outT)} of ${fmtTime(total)}`}>
        {fmtTime(outT, true)} <small>/ {fmtTime(total, true)}</small>
      </span>
      <span className="transport__spacer" />
      <button type="button" className="btn btn--sm btn--ghost" onClick={() => st().splitAtPlayhead()} title="Split at playhead (S)">
        <Scissors size={14} aria-hidden="true" /> Split
      </button>
      <button type="button" className="btn btn--sm btn--ghost" onClick={() => st().addZoomAtPlayhead()} title="Add zoom at playhead (Z)">
        <ZoomIn size={14} aria-hidden="true" /> Zoom
      </button>
    </div>
  );
}
