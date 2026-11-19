import { useState } from "react";

/** Served from ui/public/logo.png, an unmodified copy of assets/brand/logo.png. */
export const LOGO_URL = `${import.meta.env.BASE_URL}logo.png`;

/**
 * Frostcap logo. Renders the PNG over a brand-gradient squircle; if the PNG
 * fails to load, a simplified Snow White mountain is drawn instead so the mark
 * never disappears. Decorative by default (pass `label` to expose it).
 */
export function BrandMark({ size = 24, glow = false, label }: { size?: number; glow?: boolean; label?: string }) {
  const [failed, setFailed] = useState(false);
  return (
    <span
      className={`brandmark${glow ? " brandmark--glow" : ""}${failed ? " brandmark--fallback" : ""}`}
      style={{ ["--mark" as string]: `${size}px` }}
      role={label ? "img" : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
    >
      {failed ? (
        <svg viewBox="0 0 24 24" width="100%" height="100%" aria-hidden="true">
          <path d="M5 17.5 11 7.6c.5-.8 1.6-.8 2.1 0l2 3.3 1-1.5c.4-.6 1.3-.6 1.7 0l2.4 4.4c.4.8-.4 1.6-1.2 1.2l-3.3-2-3.3-2.1L7 17.6c-.9.5-2.4.6-2-.1Z" fill="#fcfdfc" opacity="0.95" />
        </svg>
      ) : (
        <img src={LOGO_URL} alt="" draggable={false} onError={() => setFailed(true)} />
      )}
    </span>
  );
}
