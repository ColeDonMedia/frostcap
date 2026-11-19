import { Circle, Clapperboard, Settings as Gear, Moon, Sun, Monitor } from "lucide-react";
import { useApp, type View, type ThemePref } from "../store/app";
import { BrandMark } from "./BrandMark";
import { t, type MessageKey } from "../i18n";

const NAV: { view: View; label: MessageKey; icon: typeof Circle; key: string }[] = [
  { view: "recorder", label: "nav.record", icon: Circle, key: "1" },
  { view: "editor", label: "nav.edit", icon: Clapperboard, key: "2" },
  { view: "settings", label: "nav.settings", icon: Gear, key: "3" },
];

const NEXT_THEME: Record<ThemePref, ThemePref> = { system: "dark", dark: "light", light: "system" };

export function Sidebar() {
  const view = useApp((s) => s.view);
  const setView = useApp((s) => s.setView);
  const projectPath = useApp((s) => s.projectPath);
  const theme = useApp((s) => s.settings.theme);
  const setSetting = useApp((s) => s.setSetting);
  const ThemeIcon = theme === "dark" ? Moon : theme === "light" ? Sun : Monitor;

  return (
    <nav className="sidebar glass" aria-label={t("nav.main")}>
      <div className="sidebar__brand">
        <BrandMark size={30} glow label="Frostcap" />
        <span className="sidebar__wordmark" aria-hidden="true">
          Frostcap
        </span>
      </div>
      <div className="sidebar__section">{t("nav.workspace")}</div>
      {NAV.map((n) => {
        const Icon = n.icon;
        const active = view === n.view;
        const disabled = n.view === "editor" && !projectPath;
        const label = t(n.label);
        return (
          <button
            key={n.view}
            type="button"
            className="navbtn"
            data-testid={`nav-${n.view}`}
            aria-current={active ? "page" : undefined}
            aria-label={label}
            title={disabled ? t("nav.needProject") : t("nav.withKey", { label, key: n.key })}
            disabled={disabled}
            onClick={() => setView(n.view)}
          >
            {active && <span className="navbtn__pill" aria-hidden="true" />}
            <Icon size={18} aria-hidden="true" fill={n.view === "recorder" && active ? "currentColor" : "none"} />
            <span className="navbtn__label">{label}</span>
          </button>
        );
      })}
      <div className="sidebar__spacer" />
      <div className="sidebar__footer">
        <button
          type="button"
          className="navbtn"
          aria-label={t("nav.themeSwitch", { theme: t(`theme.${theme}`), next: t(`theme.${NEXT_THEME[theme]}`) })}
          title={t("nav.theme", { theme: t(`theme.${theme}`) })}
          onClick={() => setSetting("theme", NEXT_THEME[theme])}
        >
          <ThemeIcon size={18} aria-hidden="true" />
          <span className="navbtn__label">{t("nav.theme", { theme: t(`theme.${theme}`) })}</span>
        </button>
        <div className="sidebar__meta">Frostcap 0.1 · MIT</div>
      </div>
    </nav>
  );
}
