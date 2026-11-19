import { Minus, Square, X, Copy } from "lucide-react";
import { useEffect, useState } from "react";
import { isTauri, windowControls } from "../backend";
import { BrandMark } from "./BrandMark";

export function TitleBar({ subtitle }: { subtitle?: string }) {
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    if (!isTauri) return;
    let un: (() => void) | undefined;
    (async () => {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      const w = getCurrentWindow();
      setMaximized(await w.isMaximized());
      un = await w.onResized(async () => setMaximized(await w.isMaximized()));
    })();
    return () => un?.();
  }, []);

  return (
    <header className="titlebar" data-tauri-drag-region>
      <div className="titlebar__brand" data-tauri-drag-region>
        <BrandMark size={18} />
        Frostcap
        {subtitle && <span className="titlebar__title">— {subtitle}</span>}
      </div>
      <div className="titlebar__drag" data-tauri-drag-region onDoubleClick={() => windowControls.toggleMaximize()} />
      <div className="titlebar__controls">
        <button type="button" className="winbtn" aria-label="Minimize" onClick={() => windowControls.minimize()}>
          <Minus size={16} strokeWidth={1.5} />
        </button>
        <button
          type="button"
          className="winbtn"
          aria-label={maximized ? "Restore" : "Maximize"}
          onClick={async () => {
            await windowControls.toggleMaximize();
            setMaximized(await windowControls.isMaximized());
          }}
        >
          {maximized ? <Copy size={13} strokeWidth={1.5} /> : <Square size={13} strokeWidth={1.5} />}
        </button>
        <button type="button" className="winbtn winbtn--close" aria-label="Close" onClick={() => windowControls.close()}>
          <X size={17} strokeWidth={1.5} />
        </button>
      </div>
    </header>
  );
}
