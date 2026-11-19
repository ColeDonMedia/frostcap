import { Circle, Clapperboard, Settings as Gear, Moon, Sun, Monitor } from "lucide-react";
import { useApp, type View, type ThemePref } from "../store/app";
import { BrandMark } from "./BrandMark";

const NAV: { view: View; label: string; icon: typeof Circle; key: string }[] = [
  { view: "recorder", label: "Record", icon: Circle, key: "1" },
  { view: "editor", label: "Edit", icon: Clapperboard, key: "2" },
  { view: "settings", label: "Settings", icon: Gear, key: "3" },
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
    <nav className="sidebar glass" aria-label="Main">
      <div className="sidebar__brand">
        <BrandMark size={30} glow label="Frostcap" />
        <span className="sidebar__wordmark" aria-hidden="true">
          Frostcap
        </span>
      </div>
      <div className="sidebar__section">Workspace</div>
      {NAV.map((n) => {
        const Icon = n.icon;
        const active = view === n.view;
        const disabled = n.view === "editor" && !projectPath;
        return (
          <button
            key={n.view}
            type="button"
            className="navbtn"
            aria-current={active ? "page" : undefined}
            aria-label={n.label}
            title={disabled ? "Record or open a project first" : `${n.label} (Ctrl+${n.key})`}
            disabled={disabled}
            onClick={() => setView(n.view)}
          >
            {active && <span className="navbtn__pill" aria-hidden="true" />}
            <Icon size={18} aria-hidden="true" fill={n.view === "recorder" && active ? "currentColor" : "none"} />
            <span className="navbtn__label">{n.label}</span>
          </button>
        );
      })}
      <div className="sidebar__spacer" />
      <div className="sidebar__footer">
        <button
          type="button"
          className="navbtn"
          aria-label={`Theme: ${theme}. Switch to ${NEXT_THEME[theme]}`}
          title={`Theme: ${theme}`}
          onClick={() => setSetting("theme", NEXT_THEME[theme])}
        >
          <ThemeIcon size={18} aria-hidden="true" />
          <span className="navbtn__label">Theme: {theme[0].toUpperCase() + theme.slice(1)}</span>
        </button>
        <div className="sidebar__meta">Frostcap 0.1 · MIT</div>
      </div>
    </nav>
  );
}
