/// <reference lib="webworker" />
/**
 * Computes waveform peaks for a WAV URL off the main thread, streaming the
 * response so multi-hour recordings never sit in memory as a whole.
 * Message in:  { id, url, bucketMs }
 * Message out: { id, bucketMs, peaks: Float32Array } | { id, error }
 */
import { PeakAccumulator, parseWavHeader, type WavFormat } from "./wav";

declare const self: DedicatedWorkerGlobalScope;

self.onmessage = async (e: MessageEvent<{ id: number; url: string; bucketMs: number }>) => {
  const { id, url, bucketMs } = e.data;
  try {
    const res = await fetch(url);
    if (!res.ok || !res.body) throw new Error(`HTTP ${res.status}`);
    const reader = res.body.getReader();
    let head = new Uint8Array(0);
    let fmt: WavFormat | null = null;
    let acc: PeakAccumulator | null = null;
    let remaining = Infinity;
    let frames = 1;
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      let chunk = value;
      if (!fmt) {
        const merged = new Uint8Array(head.length + chunk.length);
        merged.set(head);
        merged.set(chunk, head.length);
        head = merged;
        fmt = parseWavHeader(head);
        if (!fmt) continue;
        frames = Math.max(1, Math.round((fmt.sampleRate * bucketMs) / 1000));
        acc = new PeakAccumulator(fmt, frames);
        remaining = fmt.dataSize ?? Infinity;
        chunk = head.subarray(fmt.dataOffset);
      }
      const take = chunk.subarray(0, Math.min(chunk.length, remaining));
      acc!.push(take);
      remaining -= take.length;
      if (remaining <= 0) {
        await reader.cancel();
        break;
      }
    }
    if (!fmt || !acc) throw new Error("not a WAV file");
    const peaks = acc.finish();
    self.postMessage({ id, bucketMs: (frames * 1000) / fmt.sampleRate, peaks }, [peaks.buffer]);
  } catch (err) {
    self.postMessage({ id, error: String(err) });
  }
};
