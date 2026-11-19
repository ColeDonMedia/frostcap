import { lazy, Suspense, useEffect } from "react";
import { AnimatePresence, LazyMotion, MotionConfig, domAnimation, m } from "motion/react";
import { TitleBar } from "./components/TitleBar";
import { Sidebar } from "./components/Sidebar";
import { Mesh } from "./components/Mesh";
import { RecorderBar, Countdown } from "./components/RecorderBar";
import RecorderView from "./views/RecorderView";
import { useApp, type View } from "./store/app";
import { useApplyTheme } from "./lib/theme";
import { useRecording } from "./store/recording";
import { navShortcut } from "./lib/keys";
import { beforeUnload, requestClose } from "./store/guard";
import { UnsavedDialog } from "./components/UnsavedDialog";
import { Toasts } from "./components/Toasts";
import { isTauri } from "./backend";

// Code-split: editor (canvas renderer, timeline, inspector, export) and settings.
const EditorView = lazy(() => import("./views/editor/EditorView"));
const SettingsView = lazy(() => import("./views/SettingsView"));

const TITLES: Record<View, string> = { recorder: "Record", editor: "Editor", settings: "Settings" };

export default function App() {
  const { surface } = useApplyTheme();
  const view = useApp((s) => s.view);
  const setView = useApp((s) => s.setView);
  const projectPath = useApp((s) => s.projectPath);

  // Global navigation shortcuts: Ctrl+1/2/3.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const v = navShortcut(e);
      if (!v || (v === "editor" && !useApp.getState().projectPath)) return;
      e.preventDefault();
      setView(v);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [setView]);

  useEffect(() => {
    void useRecording.getState().syncWithBackend();
    let un: (() => void) | undefined;
    let alive = true;
    void useRecording.getState().listenHotkeys().then((u) => (alive ? (un = u) : u()));
    return () => {
      alive = false;
      un?.();
    };
  }, []);

  // Unsaved edits: browser tab close/reload and the Tauri window close both ask first.
  useEffect(() => {
    window.addEventListener("beforeunload", beforeUnload);
    let un: (() => void) | undefined;
    let allowClose = false;
    if (isTauri) {
      void import("@tauri-apps/api/window").then(async ({ getCurrentWindow }) => {
        const w = getCurrentWindow();
        un = await w.onCloseRequested((e) => {
          if (allowClose) return;
          const ok = requestClose(() => {
            allowClose = true;
            void w.close();
          });
          if (!ok) e.preventDefault();
        });
      });
    }
    return () => {
      window.removeEventListener("beforeunload", beforeUnload);
      un?.();
    };
  }, []);

  useEffect(() => {
    document.title = `Frostcap — ${TITLES[view]}`;
  }, [view]);

  return (
    <LazyMotion features={domAnimation} strict>
      <MotionConfig reducedMotion={surface === "solid" ? "always" : "user"}>
        <Mesh />
        <a className="skip-link" href="#main">
          Skip to main content
        </a>
        <div className="app">
          <TitleBar subtitle={TITLES[view]} />
          <div className="shell">
            <Sidebar />
            <main className="main" id="main" tabIndex={-1}>
              <AnimatePresence mode="wait" initial={false}>
                <m.div
                  key={view}
                  style={{ flex: 1, minHeight: 0, display: "flex", flexDirection: "column" }}
                  initial={{ opacity: 0, y: 8 }}
                  animate={{ opacity: 1, y: 0 }}
                  exit={{ opacity: 0, y: -6 }}
                  transition={{ duration: 0.18 }}
                >
                  <Suspense
                    fallback={
                      <div className="loading">
                        <div className="spinner" role="status" aria-label="Loading" />
                      </div>
                    }
                  >
                    {view === "recorder" && <RecorderView />}
                    {view === "editor" && projectPath && <EditorView path={projectPath} />}
                    {view === "settings" && <SettingsView />}
                  </Suspense>
                </m.div>
              </AnimatePresence>
            </main>
          </div>
        </div>
        <RecorderBar />
        <Countdown />
        <UnsavedDialog />
        <Toasts />
      </MotionConfig>
    </LazyMotion>
  );
}
