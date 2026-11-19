/** Two monitors side by side (lucide has no multi-monitor glyph); same stroke style as lucide icons. */
export function MultiMonitorIcon({ size = 16, strokeWidth = 2, ...rest }: { size?: number; strokeWidth?: number; "aria-hidden"?: boolean | "true" }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={strokeWidth} strokeLinecap="round" strokeLinejoin="round" {...rest}>
      <rect x="1.5" y="5" width="12" height="9" rx="1.5" />
      <path d="M5 19h5M7.5 14v5" />
      <rect x="15.5" y="8" width="7" height="8" rx="1.5" />
      <path d="M19 16v3" />
    </svg>
  );
}
