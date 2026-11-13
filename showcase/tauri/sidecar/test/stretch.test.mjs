import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

// The overlay's time-stretch (roamies/stretch.js): faster speech at the same pitch, fed in chunks as it streams.
const { stretcher } = createRequire(import.meta.url)('../../../roamies/stretch.js');

const sr = 44100;
const tone = (secs, f) => Float32Array.from({ length: Math.round(sr * secs) }, (_, i) => 0.5 * Math.sin((2 * Math.PI * f * i) / sr));
// Upward zero crossings a second: the pitch.
const pitch = (x) => {
  let n = 0;
  for (let i = 1; i < x.length; i++) if (x[i - 1] < 0 && x[i] >= 0) n++;
  return n / (x.length / sr);
};

for (const speed of [1.3, 1.75, 0.8]) {
  const st = stretcher(sr, speed);
  const input = tone(2, 220);
  const parts = [];
  // Odd-sized chunks, as the voice services send them.
  for (let i = 0; i < input.length; i += 3001) parts.push(st.push(input.subarray(i, i + 3001)));
  parts.push(st.flush());
  const out = new Float32Array(parts.reduce((s, x) => s + x.length, 0));
  let k = 0;
  for (const x of parts) { out.set(x, k); k += x.length; }
  const want = input.length / speed;
  assert.ok(Math.abs(out.length - want) / want < 0.05, `${speed}x: ${out.length} samples, about ${Math.round(want)} wanted`);
  // The middle (past the fade in and out) keeps its pitch and its loudness.
  const mid = out.subarray(Math.round(out.length * 0.2), Math.round(out.length * 0.8));
  assert.ok(Math.abs(pitch(mid) - 220) < 6, `${speed}x keeps the pitch: ${pitch(mid).toFixed(1)} Hz`);
  const rms = Math.sqrt(mid.reduce((s, v) => s + v * v, 0) / mid.length);
  assert.ok(Math.abs(rms - 0.5 / Math.SQRT2) < 0.04, `${speed}x keeps the loudness: ${rms.toFixed(3)}`);
}
// Too little to make a frame yet: nothing comes out until more arrives.
assert.equal(stretcher(sr, 1.3).push(new Float32Array(100)).length, 0);
console.log('stretch tests: passed');
