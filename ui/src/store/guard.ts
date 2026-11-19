/**
 * Unsaved-changes guard: switching projects or closing the window while the
 * editor is dirty asks Save / Discard / Cancel (UnsavedDialog renders it).
 */
import { create } from "zustand";
import { useApp } from "./app";
import { useEditor } from "./editor";
import { t } from "../i18n";

export type Pending = { kind: "open"; path: string } | { kind: "close"; close: () => void };
export type Choice = "save" | "discard" | "cancel";

interface GuardState {
  pending: Pending | null;
  saving: boolean;
  resolve: (c: Choice) => Promise<void>;
}

const isDirty = () => {
  const e = useEditor.getState();
  return !!e.project && e.dirty;
};

function proceed(p: Pending) {
  if (p.kind === "open") useApp.getState().openProject(p.path);
  else p.close();
}

export const useGuard = create<GuardState>((set, get) => ({
  pending: null,
  saving: false,
  async resolve(choice) {
    const p = get().pending;
    if (!p) return;
    if (choice === "cancel") {
      set({ pending: null });
      return;
    }
    if (choice === "save") {
      set({ saving: true });
      try {
        await useEditor.getState().save();
      } catch (e) {
        set({ saving: false });
        useApp.getState().reportIssue(e, t("guard.saveFailed"));
        return; // keep the dialog open
      }
      set({ saving: false });
    }
    set({ pending: null });
    proceed(p);
  },
}));

/** Open a project, asking first if the current one has unsaved edits. */
export async function openProjectGuarded(path: string): Promise<void> {
  if (useEditor.getState().path === path || !isDirty()) {
    useApp.getState().openProject(path);
    return;
  }
  useGuard.setState({ pending: { kind: "open", path } });
}

/** Window close: returns true if closing may proceed now; otherwise asks and calls `close` after a choice. */
export function requestClose(close: () => void): boolean {
  if (!isDirty()) return true;
  useGuard.setState({ pending: { kind: "close", close } });
  return false;
}

/** Browser tab close/reload: the native "leave site?" prompt while dirty. */
export function beforeUnload(e: BeforeUnloadEvent) {
  if (!isDirty()) return;
  e.preventDefault();
  e.returnValue = "";
}
