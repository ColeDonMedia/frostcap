/**
 * Streaming WAV parsing + peak extraction (pure; used by peaks.worker.ts and tests).
 * Supports PCM 8/16/24/32-bit int, IEEE float 32/64, and WAVE_FORMAT_EXTENSIBLE.
 * Tolerates a zero / 0xFFFFFFFF data size (interrupted WASAPI recordings).
 */

export interface WavFormat {
  /** 1 = PCM int, 3 = IEEE float (after resolving EXTENSIBLE). */
  format: 1 | 3;
  channels: number;
  sampleRate: number;
  bitsPerSample: number;
  /** Byte offset of the first sample. */
  dataOffset: number;
  /** Byte length of sample data, or null when unknown (stream until EOF). */
  dataSize: number | null;
}

/** Parse the header from the first bytes of the file. Returns null if more bytes are needed. Throws on non-WAV. */
export function parseWavHeader(b: Uint8Array): WavFormat | null {
  if (b.length < 12) return null;
  const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const tag = (o: number) => String.fromCharCode(b[o], b[o + 1], b[o + 2], b[o + 3]);
  if (tag(0) !== "RIFF" && tag(0) !== "RF64") throw new Error("not a RIFF file");
  if (tag(8) !== "WAVE") throw new Error("not a WAVE file");
  let p = 12;
  let fmt: Omit<WavFormat, "dataOffset" | "dataSize"> | null = null;
  while (p + 8 <= b.length) {
    const id = tag(p);
    const size = dv.getUint32(p + 4, true);
    if (id === "fmt ") {
      if (p + 8 + 16 > b.length) return null;
      let format = dv.getUint16(p + 8, true);
      const channels = dv.getUint16(p + 10, true);
      const sampleRate = dv.getUint32(p + 12, true);
      const bitsPerSample = dv.getUint16(p + 22, true);
      if (format === 0xfffe) {
        if (p + 8 + 26 > b.length) return null;
        format = dv.getUint16(p + 8 + 24, true); // SubFormat GUID starts with the format tag
      }
      if (format !== 1 && format !== 3) throw new Error(`unsupported WAV format ${format}`);
      fmt = { format, channels, sampleRate, bitsPerSample };
    } else if (id === "data") {
      if (!fmt) throw new Error("data chunk before fmt chunk");
      const known = size !== 0 && size !== 0xffffffff;
      return { ...fmt, dataOffset: p + 8, dataSize: known ? size : null };
    }
    p += 8 + size + (size & 1);
  }
  return null;
}

/**
 * Accumulates max |amplitude| per bucket of `bucketFrames` frames across all
 * channels. Feed sample bytes (after the header) in any chunking.
 */
export class PeakAccumulator {
  private readonly bytesPerSample: number;
  private readonly frameBytes: number;
  private carry = new Uint8Array(0);
  private frameInBucket = 0;
  private cur = 0;
  private out: number[] = [];

  constructor(
    private fmt: WavFormat,
    private bucketFrames: number,
  ) {
    this.bytesPerSample = fmt.bitsPerSample / 8;
    this.frameBytes = this.bytesPerSample * fmt.channels;
    if (!Number.isInteger(this.bytesPerSample) || this.frameBytes <= 0) throw new Error("bad sample size");
  }

  push(chunk: Uint8Array) {
    let buf = chunk;
    if (this.carry.length) {
      buf = new Uint8Array(this.carry.length + chunk.length);
      buf.set(this.carry);
      buf.set(chunk, this.carry.length);
    }
    const whole = buf.length - (buf.length % this.frameBytes);
    const dv = new DataView(buf.buffer, buf.byteOffset, whole);
    const { format, bitsPerSample } = this.fmt;
    const bps = this.bytesPerSample;
    for (let o = 0; o < whole; o += this.frameBytes) {
      for (let c = 0, so = o; c < this.fmt.channels; c++, so += bps) {
        let v: number;
        if (format === 3) v = bitsPerSample === 64 ? dv.getFloat64(so, true) : dv.getFloat32(so, true);
        else if (bitsPerSample === 16) v = dv.getInt16(so, true) / 32768;
        else if (bitsPerSample === 24) {
          const x = dv.getUint8(so) | (dv.getUint8(so + 1) << 8) | (dv.getInt8(so + 2) << 16);
          v = x / 8388608;
        } else if (bitsPerSample === 32) v = dv.getInt32(so, true) / 2147483648;
        else v = (dv.getUint8(so) - 128) / 128;
        const a = v < 0 ? -v : v;
        if (a > this.cur) this.cur = a;
      }
      if (++this.frameInBucket === this.bucketFrames) {
        this.out.push(Math.min(1, this.cur));
        this.cur = 0;
        this.frameInBucket = 0;
      }
    }
    this.carry = buf.slice(whole);
  }

  /** Peaks (0..1), one per bucket; a trailing partial bucket is included. */
  finish(): Float32Array {
    if (this.frameInBucket > 0) this.out.push(Math.min(1, this.cur));
    this.frameInBucket = 0;
    this.cur = 0;
    return Float32Array.from(this.out);
  }
}

export interface Peaks {
  /** Milliseconds per bucket. */
  bucketMs: number;
  peaks: Float32Array;
}

/** Whole-buffer convenience (tests, small files). */
export function peaksFromWav(bytes: Uint8Array, bucketMs = 10): Peaks {
  const fmt = parseWavHeader(bytes);
  if (!fmt) throw new Error("truncated WAV header");
  const frames = Math.max(1, Math.round((fmt.sampleRate * bucketMs) / 1000));
  const acc = new PeakAccumulator(fmt, frames);
  const end = fmt.dataSize == null ? bytes.length : Math.min(bytes.length, fmt.dataOffset + fmt.dataSize);
  acc.push(bytes.subarray(fmt.dataOffset, end));
  return { bucketMs: (frames * 1000) / fmt.sampleRate, peaks: acc.finish() };
}
