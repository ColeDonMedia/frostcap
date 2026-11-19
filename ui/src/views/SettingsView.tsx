import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { chordFromEvent, validateBindings, type HotkeyBindings } from "../lib/hotkeys";
import type { WireKeyCapture } from "../contract";
import { Monitor, Moon, Sun, Sparkles, Square, Wand2 } from "lucide-react";
import { useApp, type Language, type ThemePref, type TransparencyPref } from "../store/app";
import { Kbd, Segmented, Select, Switch } from "../components/ui";
import type { HotkeyChord } from "../types";
import { backend, isTauri, toBackendError, type CaptureSettings } from "../backend";
import { BrandMark } from "../components/BrandMark";
import { LOCALES, systemLocale, t, tRich, type MessageKey } from "../i18n";

// Key names (Space, Ctrl, ...) stay in English on purpose: they are the labels printed on the keyboard shortcuts documentation.
const SHORTCUTS: [MessageKey, string[]][] = [
  ["sc.playPause", ["Space"]],
  ["sc.split", ["S"]],
  ["sc.addZoom", ["Z"]],
  ["sc.delete", ["Del"]],
  ["sc.undo", ["Ctrl", "Z"]],
  ["sc.redo", ["Ctrl", "Y"]],
  ["sc.frame", ["←", "→"]],
  ["sc.second", ["Shift", "← / →"]],
  ["sc.startEnd", ["Home", "End"]],
  ["sc.save", ["Ctrl", "S"]],
  ["sc.export", ["Ctrl", "E"]],
  ["sc.views", ["Ctrl", "1 / 2 / 3"]],
];

/** Absolute Windows path (drive or UNC) without characters Windows forbids. */
export function folderError(p: string): string | null {
  const v = p.trim();
  if (!v) return null; // empty = backend default
  if (!/^([a-z]:\\|\\\\[^\\]+\\[^\\]+)/i.test(v)) return t("settings.errFull");
  if (/[<>"|?*]/.test(v.slice(2))) return t("settings.errChars");
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
          {t("settings.outputFolder")}
        </span>
        <span className="setting__hint" style={{ display: "block" }}>
          {saved ? t("settings.outputSaved") : fallback ? t("settings.outputDefault") : t("settings.outputDefaultGeneric")}
        </span>
      </label>
      <div style={{ display: "flex", flexDirection: "column", gap: 6, flex: "1 1 320px" }}>
        <div style={{ display: "flex", gap: 8 }}>
          <input
            id="output-folder"
            className="input"
            style={{ flex: 1, fontFamily: "var(--mono)", fontSize: 12.5 }}
            value={shown}
            placeholder={t("settings.outputPlaceholder")}
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
            title={isTauri ? t("settings.choose") : t("settings.chooseDemo")}
            onClick={async () => {
              try {
                const p = await backend.pickFolder(t("settings.chooseTitle"));
                if (p) {
                  setSetting("outputFolder", p);
                  setDraft(null);
                  setError(null);
                }
              } catch (e) {
                reportIssue(e, t("settings.errChoose"));
              }
            }}
          >
            {t("settings.browse")}
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

const ACTION_LABEL: Record<HotkeyChord["action"], MessageKey> = { startStop: "settings.hkStartStop", pauseResume: "settings.hkPauseResume" };
const ACTION_CHANGE: Record<HotkeyChord["action"], MessageKey> = { startStop: "settings.hkChangeStartStop", pauseResume: "settings.hkChangePauseResume" };
const ACTION_CHANGE_LABEL: Record<HotkeyChord["action"], MessageKey> = { startStop: "settings.hkChangeLabelStartStop", pauseResume: "settings.hkChangeLabelPauseResume" };

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
        {t("settings.hotkeys")}
      </h2>
      {chords === null ? (
        <p className="field__hint">{t("common.loading")}</p>
      ) : chords.length === 0 ? (
        <p className="field__hint">{t("settings.hotkeysUnavailable")}</p>
      ) : (
        chords.map((c) => (
          <div className="setting hotkey" key={c.action}>
            <div className="setting__text">
              <span className="setting__label" style={{ display: "block" }}>
                {t(ACTION_LABEL[c.action])}
              </span>
              <span className="setting__hint" style={{ display: "block" }}>
                {c.registered ? t("settings.hkActive") : t("settings.hkInactive")}
              </span>
            </div>
            {editing === c.action ? (
              <button ref={captureRef} type="button" className="btn hotkey__capture" onKeyDown={(e) => void onCapture(e)} disabled={saving} aria-describedby="hotkey-help">
                {t("settings.hkCapture")}
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
                  aria-label={t(ACTION_CHANGE_LABEL[c.action])}
                >
                  {t(ACTION_CHANGE[c.action])}
                </button>
              </>
            )}
          </div>
        ))
      )}
      <p id="hotkey-help" className="field__hint" style={{ margin: 0 }}>
        {t("settings.hkHelp")}
      </p>
      {error && (
        <p className="field__error" role="alert" style={{ margin: 0 }}>
          {error}
        </p>
      )}
    </section>
  );
}

/** Backend-persisted capture options. A failed save reverts the switch and shows an error toast. */
function CaptureSection() {
  const reportIssue = useApp((s) => s.reportIssue);
  const [cs, setCs] = useState<CaptureSettings | null>(null);

  useEffect(() => {
    backend.captureSettings().then(setCs, () => setCs(null));
  }, []);

  const change = async (key: keyof CaptureSettings, enabled: boolean) => {
    const before = cs;
    setCs((c) => (c ? { ...c, [key]: enabled } : c));
    try {
      setCs(await (key === "captureBorder" ? backend.setCaptureBorder(enabled) : backend.setExcludeOwnWindows(enabled)));
    } catch (e) {
      setCs(before);
      // The user declined the native confirmation: revert quietly.
      if (toBackendError(e).code !== "cancelled") reportIssue(e, t("settings.errCapture"));
    }
  };

  return (
    <section className="panel glass" aria-labelledby="s-capture">
      <h2 className="panel__title" id="s-capture">
        {t("settings.capture")}
      </h2>
      {cs === null ? (
        <p className="field__hint">{t("settings.captureUnavailable")}</p>
      ) : (
        <>
          <div className="setting">
            <label className="setting__text" htmlFor="set-capture-border">
              <span className="setting__label" style={{ display: "block" }}>
                {t("settings.captureBorder")}
              </span>
              <span className="setting__hint" style={{ display: "block" }}>
                {t("settings.captureBorderHint")}
              </span>
            </label>
            <Switch id="set-capture-border" label={t("settings.captureBorder")} checked={cs.captureBorder} onChange={(v) => void change("captureBorder", v)} />
          </div>
          <div className="setting">
            <label className="setting__text" htmlFor="set-exclude-own">
              <span className="setting__label" style={{ display: "block" }}>
                {t("settings.excludeOwn")}
              </span>
              <span className="setting__hint" style={{ display: "block" }}>
                {t("settings.excludeOwnHint")}
              </span>
            </label>
            <Switch id="set-exclude-own" label={t("settings.excludeOwn")} checked={cs.excludeOwnWindows} onChange={(v) => void change("excludeOwnWindows", v)} />
          </div>
        </>
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
          <h1 className="page__title">{t("settings.title")}</h1>
          <p className="page__sub">{t("settings.subtitle")}</p>
        </div>
      </div>
      <div className="settings">
        <section className="panel glass" aria-labelledby="s-appearance">
          <h2 className="panel__title" id="s-appearance">
            {t("settings.appearance")}
          </h2>
          <div className="setting">
            <div className="setting__text">
              <div className="setting__label">{t("settings.theme")}</div>
              <div className="setting__hint">{t("settings.themeHint")}</div>
            </div>
            <Segmented<ThemePref>
              label={t("settings.theme")}
              value={settings.theme}
              onChange={(v) => setSetting("theme", v)}
              options={[
                { value: "system", label: t("theme.system"), icon: <Monitor size={14} /> },
                { value: "dark", label: t("theme.dark"), icon: <Moon size={14} /> },
                { value: "light", label: t("theme.light"), icon: <Sun size={14} /> },
              ]}
            />
          </div>
          <div className="setting">
            <div className="setting__text">
              <div className="setting__label">{t("settings.transparency")}</div>
              <div className="setting__hint">{t("settings.transparencyHint")}</div>
            </div>
            <Segmented<TransparencyPref>
              label={t("settings.transparency")}
              value={settings.transparency}
              onChange={(v) => setSetting("transparency", v)}
              options={[
                { value: "auto", label: t("settings.trAuto"), icon: <Wand2 size={14} /> },
                { value: "glass", label: t("settings.trGlass"), icon: <Sparkles size={14} /> },
                { value: "solid", label: t("settings.trSolid"), icon: <Square size={14} /> },
              ]}
            />
          </div>
          <div className="setting">
            <div className="setting__text">
              <div className="setting__label">{t("settings.language")}</div>
              <div className="setting__hint">{t("settings.languageHint")}</div>
            </div>
            <div style={{ minWidth: 220 }}>
              <Select<Language>
                label={t("settings.language")}
                hideLabel
                value={settings.language}
                onChange={(v) => setSetting("language", v)}
                options={[{ value: "system", label: t("settings.languageSystem", { name: LOCALES.find((l) => l.id === systemLocale())?.name ?? "English" }) }, ...LOCALES.map((l) => ({ value: l.id, label: l.name }))]}
              />
            </div>
          </div>
        </section>

        <section className="panel glass" aria-labelledby="s-output">
          <h2 className="panel__title" id="s-output">
            {t("settings.output")}
          </h2>
          <OutputFolderField />
        </section>

        <CaptureSection />

        <section className="panel glass" aria-labelledby="s-privacy">
          <h2 className="panel__title" id="s-privacy">
            {t("settings.privacy")}
          </h2>
          <div className="setting">
            <div className="setting__text">
              <span className="setting__label" style={{ display: "block" }}>
                {t("settings.keyCapture")}
              </span>
              <span className="setting__hint" style={{ display: "block" }}>
                {tRich("settings.keyCaptureHint", undefined, (s) => (
                  <strong key="on">{s}</strong>
                ))}
              </span>
            </div>
            <Segmented<WireKeyCapture>
              label={t("settings.keyCapture")}
              value={settings.keyCapture}
              onChange={(v) => (v === "all" ? setConfirmAll(true) : setSetting("keyCapture", v))}
              options={[
                { value: "off", label: t("settings.keysOff") },
                { value: "shortcuts", label: t("settings.keysShortcuts") },
                { value: "all", label: t("settings.keysAll") },
              ]}
            />
          </div>
          {confirmAll && (
            <div className="confirm glass" role="alertdialog" aria-labelledby="all-keys-title" aria-describedby="all-keys-desc">
              <strong id="all-keys-title">{t("settings.confirmTitle")}</strong>
              <p id="all-keys-desc" className="setting__hint" style={{ margin: 0 }}>
                {t("settings.confirmBody")}
              </p>
              <div className="sheet__foot">
                <button type="button" className="btn" autoFocus onClick={() => setConfirmAll(false)}>
                  {t("common.cancel")}
                </button>
                <button
                  type="button"
                  className="btn btn--danger"
                  onClick={() => {
                    setSetting("keyCapture", "all");
                    setConfirmAll(false);
                  }}
                >
                  {t("settings.confirmYes")}
                </button>
              </div>
            </div>
          )}
          <div className="setting">
            <label className="setting__text" htmlFor="set-titles">
              <span className="setting__label" style={{ display: "block" }}>
                {t("settings.titles")}
              </span>
              <span className="setting__hint" style={{ display: "block" }}>
                {t("settings.titlesHint")}
              </span>
            </label>
            <Switch id="set-titles" label={t("settings.titles")} checked={settings.captureTitles} onChange={(v) => setSetting("captureTitles", v)} />
          </div>
          <p className="field__hint" style={{ margin: 0 }}>
            {t("settings.offline")}
          </p>
        </section>

        <section className="panel glass" aria-labelledby="s-about">
          <h2 className="panel__title" id="s-about">
            {t("settings.about")}
          </h2>
          <div className="about">
            <BrandMark size={64} glow />
            <div>
              <p className="about__name">Frostcap</p>
              <p className="about__meta">{t("settings.aboutMeta")}</p>
              <p className="about__meta">{t("settings.aboutTagline")}</p>
            </div>
          </div>
        </section>

        <HotkeysSection />

        <section className="panel glass" aria-labelledby="s-keys">
          <h2 className="panel__title" id="s-keys">
            {t("settings.shortcuts")}
          </h2>
          <dl className="shortcuts">
            {SHORTCUTS.map(([label, keys]) => (
              <div key={label} style={{ display: "contents" }}>
                <dt>{t(label)}</dt>
                <dd>
                  {keys.map((k) => (
                    <Kbd key={k}>{k}</Kbd>
                  ))}
                </dd>
              </div>
            ))}
          </dl>
          <p className="field__hint" style={{ margin: 0 }}>
            {t("settings.shortcutsNote")}
          </p>
        </section>
      </div>
      </div>
    </div>
  );
}
