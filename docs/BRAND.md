# Frostcap brand

The name is written **Frostcap** (capital F, one word) everywhere in the UI.

## Palette

| Name | Hex | Role | CSS token |
|---|---|---|---|
| Frost Cyan | `#4ED2FB` | Light upper-left of the gradient; focus/selection lines and slider fill in dark theme | `--frost-cyan` |
| Glacier Blue | `#389AF5` | Middle of the gradient; secondary line colour (dark) | `--glacier-blue` |
| Arctic Blue | `#0752F1` | Right side of the gradient; focus/selection colour in light theme | `--arctic-blue` |
| Deep Frost Blue | `#1151EF` | Bottom of the gradient | `--deep-frost-blue` |
| Snow White | `#FCFDFC` | Mountain peaks; text and icons on brand colour | `--snow-white` |

### Derived values

These come from the palette and exist only to meet contrast requirements:

| Name | Value | Derived from | Why |
|---|---|---|---|
| Glacier Deep | `#1A6AD8` | Glacier Blue, darkened | Snow White on it is 5.03:1. Glacier Blue itself is only 2.90:1, so it cannot sit under text. |
| Accent glow | `rgba(7, 82, 241, 0.38)` | Arctic Blue | Button and mark glow |
| Accent sheen | `inset 0 1px 0 rgba(78, 210, 251, 0.55)` | Frost Cyan | Top highlight on primary buttons (decoration only) |
| Selection | `rgba(56, 154, 245, 0.4)` dark / `0.3` light | Glacier Blue | `::selection` |
| Light-theme clip fill | `#1A6AD8 → #165FCC`, border `#0F4FB3` | Glacier Deep | Video clips on the timeline, white labels |
| Light-theme zoom border | `#0A3FB8` | Arctic Blue, darker | Zoom-segment outline |
| Mesh tints (light) | `#8FE3FD`, `#6F9CF7` | Frost Cyan / Arctic Blue, lightened | Background blobs |

## Gradients

- `--brand-grad`: 135°, Frost Cyan 0% → Glacier Blue 38% → Arctic Blue 72% → Deep Frost Blue 100%. Use it only for decoration: logo fallback, preview background preset "frost", the faux-screen chart. **Never put text on it**, because Snow White on Frost Cyan is 1.73:1.
- `--accent-grad`: 135°, Glacier Deep → Arctic Blue → Deep Frost Blue. Every stop is at least 5.03:1 with Snow White. Use it for anything carrying text or icons: primary buttons, play button, switches, the source-card check badge.
- Gradient-as-text is not used. If it is ever needed, use `--accent-grad` on light backgrounds (≥ 5:1 on white) and a Frost Cyan → Glacier Blue gradient on dark backgrounds (≥ 5.85:1 on `#161A2A`). Never use the cyan end on light backgrounds.

## Themed accent tokens (`ui/src/styles.css`)

| Token | Dark | Light |
|---|---|---|
| `--focus` (focus rings) | Frost Cyan (10.9:1 on `#0C0F1A`) | Arctic Blue (6.0:1 on white) |
| `--accent-line` (selected outlines, markers, ripples) | Frost Cyan | Arctic Blue |
| `--accent-line-2` | Glacier Blue | Glacier Deep |
| `--accent-text` | Frost Cyan | Arctic Blue |
| `--accent-fill` (slider and progress fill, aspect shapes) | Frost Cyan → Glacier Blue | Glacier Deep → Arctic Blue (≥ 3:1 non-text on white) |
| `--on-accent` (text/icons on `--accent-grad`) | Snow White | Snow White |
| `--mesh-1..4` (background blobs) | Arctic, Deep Frost, Glacier, Frost Cyan @ 0.62 opacity | Glacier, Frost Cyan, two light tints @ 0.55 opacity |

Timeline: zoom segments use Arctic → Deep Frost with a Frost Cyan top highlight and Snow White labels (5.9:1). Video clips are a translucent Glacier tint in dark mode and Glacier Deep in light mode, both with white labels. Right-click markers stay amber and danger stays red, since those colours carry meaning and are not brand colours.

Contrast figures are WCAG 2.x relative-luminance ratios against the solid colours listed. On glass, the actual backdrop varies; in dark mode the glass tints are dark enough that the numbers hold.

## Title bar glass

The title bar is the strongest glass surface in the app: it sits directly on the Mica window in Tauri, because `html` is translucent there.

- **Blur:** `blur(44px) saturate(200%) brightness(1.04)`. Panels use 28px / 170%.
- **Tint:** a vertical gradient, `--tb-tint-top` → `--tb-tint-bottom`.
  - Dark: `rgba(34,42,68,.66)` → `rgba(16,20,36,.50)`.
  - Light: `rgba(250,252,255,.82)` → `rgba(214,228,252,.55)`.
- **Top highlight:** `--tb-highlight`.
- **Bottom edge:** a 1px brand hairline, `--tb-edge`, fading in and out across the width. Dark goes Frost Cyan → Glacier → Arctic; light goes Glacier → Arctic → Deep Frost.
- **Drop shadow:** a soft one, `--tb-shadow`.
- **Caption buttons:**
  - Hover is a glass gradient with an inner highlight: `--winbtn-hover` / `--winbtn-hover-edge`. Dark uses white tints; light uses Arctic Blue tints.
  - Press is a darker inset (`--winbtn-press` / `--winbtn-press-edge`), and the glyph scales to 0.9.
  - Close on hover is Windows red, `#CC3324 → #C42B1C`. White glyph contrast is 5.16 to 5.66:1. Pressed is `#B02718 → #C13A2B`.
- **Solid mode** (setting, or OS reduced transparency/motion): opaque `--glass-solid-strong`, no backdrop filter, flat hover/press. The brand hairline and the red close stay.

## Logo

- **Mark:** blue-gradient rounded square (squircle) holding a Snow White frosted-glass mountain.
- **Master:** `assets/brand/logo.png` (1024², source `logo-source.webp`).
- **UI copy:** `ui/public/logo.png` is an unmodified copy of the master. It is used for the favicon and every in-app mark. Re-copy it when the master changes.
- **Master status (checked 2026-11-17 19:38):** 1024², transparent corners taken from the source's own alpha channel. The squircle spans about x 25–999, y 30–994. Outside it, only about 10.5k pixels are non-zero, all at alpha ≤ 3/255, which is invisible.
- **Where it appears:** title bar (18 px), sidebar header (30 px, with the "Frostcap" wordmark, which is hidden when the sidebar collapses), Settings → About (64 px), and the favicon (`<link rel="icon" type="image/png">`).
- **Component:** `BrandMark` (`ui/src/components/BrandMark.tsx`). If the PNG fails to load, it draws a Snow White mountain on `--brand-grad` so the mark never disappears.

### Usage rules

- **Clear space:** keep at least **25% of the mark's width** empty on every side; no text, icons or edges inside it. In the title bar (18 px mark) that is about 4.5 px; the 8 px gap to the wordmark meets it.
- **Minimum size:** 16 px on screen (favicon). Below 24 px, don't add the glow.
- **Wordmark lockup:** mark to the left, "Frostcap" in the system font (Segoe UI Variable), semibold, vertically centred, with a gap of 25–35% of the mark size.
- **Backgrounds:** works on dark, light and glass surfaces. On busy imagery, put it on a solid surface. Don't place it on `--brand-grad`, where the squircle would vanish.
- **Don't:** recolour, rotate, stretch, outline, add drop shadows other than `--accent-glow`, crop the squircle, or place the mountain without its squircle.
