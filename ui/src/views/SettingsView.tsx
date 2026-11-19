import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { chordFromEvent, validateBindings, type HotkeyBindings } from "../lib/hotkeys";
import type { WireKeyCapture } from "../contract";
import { Monitor, Moon, Sun, Sparkles, Square, Wand2 } from "lucide-react";
import { useApp, type ThemePref, type TransparencyPref } from "../store/app";
import { Kbd, Segmented, Switch } from "../components/ui";
import type { HotkeyChord } from "../types";
import { backend, isTauri, toBackendError } from "../backend";
import { BrandMark } from "../components/BrandMark";

const SHORTCUTS: [string, string[]][] = [
  ["Play / pause", ["Space"]],
  ["Split at playhead", ["S"]],
  ["Add zoom at playhead", ["Z"]],
  ["Delete selection", ["Del"]],
  ["Undo", ["Ctrl", "Z"]],
  ["Redo", ["Ctrl", "Y"]],
  ["Step one frame", ["←", "→"]],
  ["Step one second", ["Shift", "← / →"]],
  ["Jump to start / end", ["Home", "End"]],
  ["Save project", ["Ctrl", "S"]],
  ["Export", ["Ctrl", "E"]],
  ["Go to Record / Edit / Settings", ["Ctrl", "1 / 2 / 3"]],
];

/** Absolute Windows path (drive or UNC) without characters Windows forbids. */
export function folderError(p: string): string | null {
  const v = p.trim();
  if (!v) return null; // empty = backend default
  if (!/^([a-z]:\\|\\\\[^\\]+\\[^\\]+)/i.test(v)) return "Enter a full path such as D:\\Videos\\Frostcap, or use Browse.";
  if (/[<>"|?*]/.test(v.slice(2))) return 'A folder name cannot contain < > " | ? or *.';
  return null;
}

function OutputFolderField() {
  const saved = useApp((s) => s.settings.outputFolder);
  const setSetting = useApp((s) => s.setSetting);
  const reportIssue = useApp((s) => s.reportIssue);
  const [fallback, setFallback] = useState<string | null>(null);
  const [draft, setDraft] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    backend.defaultOutputDir().then(setFallback, () => setFallback(null));
  }, []);

  const shown = draft ?? (saved || fallback || "");
  const commit = () => {
    if (draft === null) return;
    const err = folderError(draft);
    setError(err);
    if (!err) {
      setSetting("outputFolder", draft.trim() === (fallback ?? "") ? "" : draft.trim());
      setDraft(null);
    }
  };

  return (
    <div className="setting">
      <label className="setting__text" htmlFor="output-folder">
        <span className="setting__label" style={{ display: "block" }}>
          Output folder
        </span>
        <span className="setting__hint" style={{ display: "block" }}>
          {saved ? "Recordings and exports are saved here." : fallback ? "Default: your Videos\\Frostcap folder." : "Default: the Frostcap folder in your Videos library."}
        </span>
      </label>
      <div style={{ display: "flex", flexDirection: "column", gap: 6, flex: "1 1 320px" }}>
        <div style={{ display: "flex", gap: 8 }}>
          <input
            id="output-folder"
            className="input"
            style={{ flex: 1, fontFamily: "var(--mono)", fontSize: 12.5 }}
            value={shown}
            placeholder="Videos\Frostcap (default)"
            aria-invalid={error ? true : undefined}
            aria-describedby={error ? "output-folder-error" : undefined}
            onChange={(e) => {
              setDraft(e.target.value);
              if (error) setError(null);
            }}
            onBlur={commit}
            onKeyDown={(e) => e.key === "Enter" && commit()}
            spellCheck={false}
          />
          <button
            type="button"
            className="btn"
            title={isTauri ? "Choose a folder" : "Demo: picks a sample folder"}
            onClick={async () => {
              try {
                const p = await backend.pickFolder("Choose the output folder");
                if (p) {
                  setSetting("outputFolder", p);
                  setDraft(null);
                  setError(null);
                }
              } catch (e) {
                reportIssue(e, "Choose folder");
              }
            }}
          >
            Browse…
          </button>
        </div>
        {error && (
          <span id="output-folder-error" className="field__error" role="alert">
            {error}
          </span>
        )}
      </div>
    </div>
  );
}

const ACTION_LABEL: Record<HotkeyChord["action"], string> = { startStop: "Start/stop recording", pauseResume: "Pause/resume recording" };
const ACTION_SHORT: Record<HotkeyChord["action"], string> = { startStop: "start/stop", pauseResume: "pause/resume" };

/** View/change the global recording hotkeys (set_hotkeys; persisted by the backend). */
function HotkeysSection() {
  const [chords, setChords] = useState<HotkeyChord[] | null>(null);
  const [editing, setEditing] = useState<HotkeyChord["action"] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const captureRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    backend.hotkeys().then(setChords, () => setChords([]));
  }, []);
  useEffect(() => {
    if (editing) captureRef.current?.focus();
  }, [editing]);

  const current = (a: HotkeyChord["action"]) => chords?.find((c) => c.action === a)?.chord ?? "";

  const onCapture = async (e: ReactKeyboardEvent) => {
    if (!editing) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") {
      setEditing(null);
      setError(null);
      return;
    }
    const chord = chordFromEvent(e.nativeEvent);
    if (!chord) return; // only modifiers so far
    const next: HotkeyBindings = { startStop: current("startStop"), pauseResume: current("pauseResume"), [editing]: chord };
    const err = validateBindings(next);
    if (err) {
      setError(err);
      return;
    }
    setSaving(true);
    try {
      setChords(await backend.setHotkeys(next));
      setError(null);
      setEditing(null);
    } catch (ex) {
      setError(toBackendError(ex).message);
    } finally {
      setSaving(false);
    }
  };

  return (
    <section className="panel glass" aria-labelledby="s-hotkeys">
      <h2 className="panel__title" id="s-hotkeys">
        Global recording hotkeys
      </h2>
      {chords === null ? (
        <p className="field__hint">Loading…</p>
      ) : chords.length === 0 ? (
        <p className="field__hint">Global hotkeys are not available.</p>
      ) : (
        chords.map((c) => (
          <div className="setting hotkey" key={c.action}>
            <div className="setting__text">
              <span className="setting__label" style={{ display: "block" }}>
                {ACTION_LABEL[c.action]}
              </span>
              <span className="setting__hint" style={{ display: "block" }}>
                {c.registered ? "Works from any app." : "Not active: in use by another app. Choose a different combination."}
              </span>
            </div>
            {editing === c.action ? (
              <button ref={captureRef} type="button" className="btn hotkey__capture" onKeyDown={(e) => void onCapture(e)} disabled={saving} aria-describedby="hotkey-help">
                Press the new keys… (Esc to cancel)
              </button>
            ) : (
              <>
                <span className={`kbd${c.registered ? "" : " kbd--off"}`}>{c.chord}</span>
                <button
                  type="button"
                  className="btn btn--sm"
                  onClick={() => {
                    setError(null);
                    setEditing(c.action);
                  }}
                  aria-label={`Change ${ACTION_SHORT[c.action]} hotkey`}
                >
                  Change {ACTION_SHORT[c.action]}
                </button>
              </>
            )}
          </div>
        ))
      )}
      <p id="hotkey-help" className="field__hint" style={{ margin: 0 }}>
        Use Ctrl or Win plus a letter, digit or F1–F24. Starting from a hotkey always shows the countdown, and only while Frostcap is visible.
      </p>
      {error && (
        <p className="field__error" role="alert" style={{ margin: 0 }}>
          {error}
        </p>
      )}
    </section>
  );
}

export default function SettingsView() {
  const settings = useApp((s) => s.settings);
  const setSetting = useApp((s) => s.setSetting);
  const [confirmAll, setConfirmAll] = useState(false);

  return (
    <div className="page scroll">
      <div className="page__inner">
      <div className="page__head">
        <div>
          <h1 className="page__title">Settings</h1>
          <p className="page__sub">Appearance, output and keyboard shortcuts.</p>
        </div>
      </div>
      <div className="settings">
        <section className="panel glass" aria-labelledby="s-appearance">
          <h2 className="panel__title" id="s-appearance">
            Appearance
          </h2>
          <div className="setting">
            <div className="setting__text">
              <div className="setting__label">Theme</div>
              <div className="setting__hint">System follows Windows light/dark mode.</div>
            </div>
            <Segmented<ThemePref>
              label="Theme"
              value={settings.theme}
              onChange={(v) => setSetting("theme", v)}
              options={[
                { value: "system", label: "System", icon: <Monitor size={14} /> },
                { value: "dark", label: "Dark", icon: <Moon size={14} /> },
                { value: "light", label: "Light", icon: <Sun size={14} /> },
              ]}
            />
          </div>
          <div className="setting">
            <div className="setting__text">
              <div className="setting__label">Transparency</div>
              <div className="setting__hint">
                Auto uses solid surfaces when Windows asks for reduced transparency or motion. Solid also stops the background animation.
              </div>
            </div>
            <Segmented<TransparencyPref>
              label="Transparency"
              value={settings.transparency}
              onChange={(v) => setSetting("transparency", v)}
              options={[
                { value: "auto", label: "Auto", icon: <Wand2 size={14} /> },
                { value: "glass", label: "Glass", icon: <Sparkles size={14} /> },
                { value: "solid", label: "Solid", icon: <Square size={14} /> },
              ]}
            />
          </div>
        </section>

        <section className="panel glass" aria-labelledby="s-output">
          <h2 className="panel__title" id="s-output">
            Output
          </h2>
          <OutputFolderField />
        </section>

        <section className="panel glass" aria-labelledby="s-privacy">
          <h2 className="panel__title" id="s-privacy">
            Privacy
          </h2>
          <div className="setting">
            <div className="setting__text">
              <span className="setting__label" style={{ display: "block" }}>
                Key capture
              </span>
              <span className="setting__hint" style={{ display: "block" }}>
                Records key presses (never key releases, never typed text as a string) for the key overlay. “Shortcuts only” keeps keys pressed with Ctrl, Alt
                or Win, plus F-keys. “All keys” would also record passwords you type while recording, as individual presses in project.json. Off by default;
                the recorder bar shows <strong>Keys on</strong> while it is active.
              </span>
            </div>
            <Segmented<WireKeyCapture>
              label="Key capture"
              value={settings.keyCapture}
              onChange={(v) => (v === "all" ? setConfirmAll(true) : setSetting("keyCapture", v))}
              options={[
                { value: "off", label: "Off" },
                { value: "shortcuts", label: "Shortcuts only" },
                { value: "all", label: "All keys" },
              ]}
            />
          </div>
          {confirmAll && (
            <div className="confirm glass" role="alertdialog" aria-labelledby="all-keys-title" aria-describedby="all-keys-desc">
              <strong id="all-keys-title">Record every key you press?</strong>
              <p id="all-keys-desc" className="setting__hint" style={{ margin: 0 }}>
                “All keys” stores every key press while recording, which includes passwords, messages and anything else you type. Anyone with the
                project file can reconstruct it. Use “Shortcuts only” unless you really need it, and scrub keys before sharing a project.
              </p>
              <div className="sheet__foot">
                <button type="button" className="btn" autoFocus onClick={() => setConfirmAll(false)}>
                  Cancel
                </button>
                <button
                  type="button"
                  className="btn btn--danger"
                  onClick={() => {
                    setSetting("keyCapture", "all");
                    setConfirmAll(false);
                  }}
                >
                  Record all keys
                </button>
              </div>
            </div>
          )}
          <div className="setting">
            <label className="setting__text" htmlFor="set-titles">
              <span className="setting__label" style={{ display: "block" }}>
                Record window titles
              </span>
              <span className="setting__hint" style={{ display: "block" }}>
                Window titles can contain document names, email subjects or URLs. Off by default: the recorder only stores an anonymous window id
                (enough for auto-zoom to notice window switches).
              </span>
            </label>
            <Switch id="set-titles" label="Record window titles" checked={settings.captureTitles} onChange={(v) => setSetting("captureTitles", v)} />
          </div>
          <p className="field__hint" style={{ margin: 0 }}>
            Frostcap works offline: recordings, projects and settings stay on this computer. No analytics, no network requests.
          </p>
        </section>

        <section className="panel glass" aria-labelledby="s-about">
          <h2 className="panel__title" id="s-about">
            About
          </h2>
          <div className="about">
            <BrandMark size={64} glow />
            <div>
              <p className="about__name">Frostcap</p>
              <p className="about__meta">Version 0.1.0 · Free and open source (MIT)</p>
              <p className="about__meta">Screen recording with cinematic zooms, smooth cursors and frosted framing.</p>
            </div>
          </div>
        </section>

        <HotkeysSection />

        <section className="panel glass" aria-labelledby="s-keys">
          <h2 className="panel__title" id="s-keys">
            Keyboard shortcuts
          </h2>
          <dl className="shortcuts">
            {SHORTCUTS.map(([label, keys]) => (
              <div key={label} style={{ display: "contents" }}>
                <dt>{label}</dt>
                <dd>
                  {keys.map((k) => (
                    <Kbd key={k}>{k}</Kbd>
                  ))}
                </dd>
              </div>
            ))}
          </dl>
          <p className="field__hint" style={{ margin: 0 }}>
            "Editor shortcuts are fixed in this version."
          </p>
        </section>
      </div>
      </div>
    </div>
  );
}
