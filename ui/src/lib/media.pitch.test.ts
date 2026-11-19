// @vitest-environment happy-dom
import { describe, expect, it } from "vitest";
import { MediaEngine } from "./media";

describe("MediaEngine.setRate", () => {
  it("changes pitch with speed like the export: preservesPitch is off on every element", () => {
    const eng = new MediaEngine({
      dir: "C:\p",
      screen: { file: "screen.mp4", url: "asset://screen" },
      webcam: { file: "webcam.mp4", url: "asset://cam" },
      mic: { file: "m.wav", url: "asset://m", offsetMs: 0, sampleRate: 48000, channels: 1, durationMs: 1000 },
      systemAudio: null,
      width: 1920,
      height: 1080,
      fps: 60,
      durationMs: 1000,
    } as never);
    eng.setRate(2);
    const els = [eng.video, eng.webcam].filter(Boolean) as HTMLMediaElement[];
    expect(els.length).toBeGreaterThan(0);
    for (const el of els) {
      expect(el.playbackRate).toBe(2);
      expect(el.preservesPitch).toBe(false);
    }
    eng.destroy();
  });
});
