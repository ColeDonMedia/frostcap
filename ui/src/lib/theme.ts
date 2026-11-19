import { useEffect, useState } from "react";
import { useApp } from "../store/app";
import { isTauri } from "../backend";

function useMedia(query: string): boolean {
  const [m, setM] = useState(() => typeof matchMedia !== "undefined" && matchMedia(query).matches);
  useEffect(() => {
    const mq = matchMedia(query);
    const on = () => setM(mq.matches);
    on();
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, [query]);
  return m;
}

/** Resolves theme + surface mode and writes them to <html data-*>. */
export function useApplyTheme() {
  const theme = useApp((s) => s.settings.theme);
  const transparency = useApp((s) => s.settings.transparency);
  const prefersDark = useMedia("(prefers-color-scheme: dark)");
  const reducedTransparency = useMedia("(prefers-reduced-transparency: reduce)");
  const reducedMotion = useMedia("(prefers-reduced-motion: reduce)");

  const resolvedTheme = theme === "system" ? (prefersDark ? "dark" : "light") : theme;
  const surface =
    transparency === "solid" || (transparency === "auto" && (reducedTransparency || reducedMotion)) ? "solid" : "glass";

  useEffect(() => {
    const el = document.documentElement;
    el.dataset.theme = resolvedTheme;
    el.dataset.surface = surface;
    el.dataset.host = isTauri ? "tauri" : "browser";
    el.style.colorScheme = resolvedTheme;
  }, [resolvedTheme, surface]);

  return { theme: resolvedTheme, surface, reducedMotion };
}
