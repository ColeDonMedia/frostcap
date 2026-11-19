import { describe, expect, it } from "vitest";
import { PeakAccumulator, parseWavHeader, peaksFromWav } from "./wav";

function wav(opts: { format: 1 | 3 | 0xfffe; sub?: 1 | 3; bits: number; channels: number; rate: number; samples: number[][]; dataSize?: number }): Uint8Array {
  const { bits, channels, rate } = opts;
  const bps = bits / 8;
  const frames = opts.samples[0].length;
  const ext = opts.format === 0xfffe;
  const fmtLen = ext ? 40 : 16;
  const dataLen = frames * channels * bps;
  const buf = new ArrayBuffer(12 + 8 + fmtLen + 8 + dataLen);
  const dv = new DataView(buf);
  const str = (o: number, s: string) => [...s].forEach((ch, i) => dv.setUint8(o + i, ch.charCodeAt(0)));
  str(0, "RIFF");
  dv.setUint32(4, buf.byteLength - 8, true);
  str(8, "WAVE");
  str(12, "fmt ");
  dv.setUint32(16, fmtLen, true);
  dv.setUint16(20, opts.format, true);
  dv.setUint16(22, channels, true);
  dv.setUint32(24, rate, true);
  dv.setUint32(28, rate * channels * bps, true);
  dv.setUint16(32, channels * bps, true);
  dv.setUint16(34, bits, true);
  if (ext) {
    dv.setUint16(36, 22, true);
    dv.setUint16(38, bits, true);
    dv.setUint32(40, 0, true);
    dv.setUint16(44, opts.sub ?? 1, true);
  }
  const d = 20 + fmtLen;
  str(d, "data");
  dv.setUint32(d + 4, opts.dataSize ?? dataLen, true);
  let o = d + 8;
  for (let f = 0; f < frames; f++)
    for (let c = 0; c < channels; c++, o += bps) {
      const v = opts.samples[c][f];
      const isFloat = opts.format === 3 || (ext && opts.sub === 3);
      if (isFloat) dv.setFloat32(o, v, true);
      else if (bits === 16) dv.setInt16(o, Math.round(v * 32767), true);
      else if (bits === 24) {
        const x = Math.round(v * 8388607);
        dv.setUint8(o, x & 255);
        dv.setUint8(o + 1, (x >> 8) & 255);
        dv.setInt8(o + 2, x >> 16);
      }
    }
  return new Uint8Array(buf);
}

// 100 Hz sample rate, 10 ms buckets -> 1 frame per bucket; easy to reason about.
const ramp = [0, 0.25, -0.5, 1, -1, 0.1];

describe("WAV peaks", () => {
  it("16-bit PCM mono", () => {
    const { peaks, bucketMs } = peaksFromWav(wav({ format: 1, bits: 16, channels: 1, rate: 100, samples: [ramp] }), 10);
    expect(bucketMs).toBe(10);
    expect(Array.from(peaks).map((v) => +v.toFixed(3))).toEqual([0, 0.25, 0.5, 1, 1, 0.1]);
  });

  it("float32 stereo takes the max across channels", () => {
    const { peaks } = peaksFromWav(wav({ format: 3, bits: 32, channels: 2, rate: 100, samples: [ramp, ramp.map((v) => v / 2 + 0.3)] }), 10);
    expect(Array.from(peaks).map((v) => +v.toFixed(3))).toEqual([0.3, 0.425, 0.5, 1, 1, 0.35]);
  });

  it("24-bit EXTENSIBLE", () => {
    const fmt = parseWavHeader(wav({ format: 0xfffe, sub: 1, bits: 24, channels: 1, rate: 100, samples: [ramp] }))!;
    expect(fmt).toMatchObject({ format: 1, bitsPerSample: 24, channels: 1 });
    const { peaks } = peaksFromWav(wav({ format: 0xfffe, sub: 1, bits: 24, channels: 1, rate: 100, samples: [ramp] }), 10);
    expect(peaks[3]).toBeCloseTo(1, 4);
    expect(peaks[2]).toBeCloseTo(0.5, 4);
  });

  it("unknown data size (interrupted recording) streams to EOF", () => {
    const bytes = wav({ format: 1, bits: 16, channels: 1, rate: 100, samples: [ramp], dataSize: 0 });
    expect(parseWavHeader(bytes)!.dataSize).toBeNull();
    expect(peaksFromWav(bytes, 10).peaks.length).toBe(6);
  });

  it("chunked streaming equals whole-buffer result", () => {
    const n = 4801;
    const sig = Array.from({ length: n }, (_, i) => Math.sin(i / 7) * ((i % 97) / 97));
    const bytes = wav({ format: 1, bits: 16, channels: 2, rate: 48000, samples: [sig, sig.map((v) => -v * 0.8)] });
    const whole = peaksFromWav(bytes, 10).peaks;
    const fmt = parseWavHeader(bytes)!;
    const acc = new PeakAccumulator(fmt, 480);
    const data = bytes.subarray(fmt.dataOffset);
    for (let o = 0; o < data.length; o += 333) acc.push(data.subarray(o, o + 333)); // odd sizes split frames
    expect(Array.from(acc.finish())).toEqual(Array.from(whole));
  });

  it("rejects non-WAV input", () => {
    expect(() => parseWavHeader(new TextEncoder().encode("hello world, not a wav"))).toThrow();
  });
});
