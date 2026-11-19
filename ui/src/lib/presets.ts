import type { Easing, GradientBackground, ProjectStyle, ZoomPreset } from "../types";

export const GRADIENTS: GradientBackground[] = [
  { id: "frost", angle: 135, stops: ["#4ed2fb", "#389af5", "#0752f1", "#1151ef"] }, // brand
  { id: "aurora", angle: 160, stops: ["#0f2027", "#2c5364", "#4ade80"] },
  { id: "dusk", angle: 135, stops: ["#ff9a8b", "#ff6a88", "#8b5cf6"] },
  { id: "glacier", angle: 180, stops: ["#e0f2fe", "#93c5fd", "#6366f1"] },
  { id: "ember", angle: 120, stops: ["#f59e0b", "#ef4444", "#7c2d12"] },
  { id: "midnight", angle: 145, stops: ["#0b1026", "#1e1b4b", "#4c1d95"] },
  { id: "mint", angle: 135, stops: ["#a7f3d0", "#34d399", "#0e7490"] },
  { id: "sky", angle: 135, stops: ["#a1c4fd", "#c2e9fb"] }, // Rust Style::default()
];

/** Mirrors `ZoomPreset::params()` in crates/frostcap-core/src/autozoom.rs. */
export const ZOOM_PRESETS: Record<ZoomPreset, { scale: number; easing: Easing; transitionMs: number }> = {
  gentle: { scale: 1.5, easing: "easeInOutCubic", transitionMs: 900 },
  balanced: { scale: 1.8, easing: "easeInOutCubic", transitionMs: 650 },
  snappy: { scale: 2.2, easing: "easeOutQuint", transitionMs: 350 },
};

/** Defaults for UI-only style fields; Rust-backed fields are overwritten from the project. */
export function defaultStyle(): ProjectStyle {
  const zp = ZOOM_PRESETS.balanced;
  return {
    background: GRADIENTS[7],
    padding: 8,
    radius: 12,
    shadow: 50,
    frame: "minimal",
    zoom: { preset: "balanced", scale: zp.scale, easing: zp.easing, transitionMs: zp.transitionMs },
    cursor: { visible: true, smooth: true, smoothing: 0.6, size: 1, clickRipple: true, motionBlur: 0.3 },
    keys: { show: false },
    webcam: {
      enabled: false,
      shape: "circle",
      size: 0.22,
      position: "bottom-right",
      margin: 28 / 1080,
      borderWidth: 0,
      borderColor: "#ffffff",
      mirror: false,
      brightness: 0,
      contrast: 1,
      saturation: 1,
      shadow: 0.4,
      chromaKey: { enabled: false, color: "#00ff00", similarity: 0.4, smoothness: 0.1, spill: 0.5 },
      background: { kind: "none", color: "#11141c", gradient: "linear-gradient(135deg,#a1c4fd,#c2e9fb)", blur: 0.5 },
    },
    audio: {
      noiseGate: true,
      noiseGateThreshold: -50,
      highPass: true,
      highPassHz: 80,
      normalize: true,
      targetLufs: -16,
      micGain: 1,
      systemGain: 1,
    },
    captions: { enabled: true, fontSize: 40, position: "bottom", background: true },
  };
}

export function gradientCss(g: GradientBackground): string {
  return g.stops.length === 1 ? g.stops[0] : `linear-gradient(${g.angle}deg, ${g.stops.join(", ")})`;
}

/** Labels come from the catalog: `easing.<name>`. */
export const EASINGS: Easing[] = ["easeInOutCubic", "easeOutQuint", "spring", "linear"];
