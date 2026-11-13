// Speeds speech up (or slows it down) without changing its pitch: WSOLA, streaming. Each output frame is taken from
// about where the input should be at this speed, nudged by up to a few milliseconds to the spot that lines up best
// with the previous frame's natural continuation, then overlap-added with a Hann window. push() takes mono samples
// as they arrive and returns what's finished; flush() returns the rest once the reply has ended.
// Plain script for the overlay (window.Roamie.stretcher) and a CommonJS module for the tests.
(() => {
  function stretcher(sr, speed) {
    const N = Math.max(64, Math.round(sr * 0.03) & ~1); // 30 ms frames
    const Hs = N / 2, Ha = Hs * speed; // output hop, input hop
    const T = Math.round(sr * 0.008); // how far a frame may move to line up (8 ms)
    const win = new Float32Array(N);
    for (let i = 0; i < N; i++) win[i] = 0.5 - 0.5 * Math.cos((2 * Math.PI * i) / N); // halves overlap-add to 1
    let inp = new Float32Array(0), base = 0; // inp[0] is input sample number `base`
    let ideal = 0, prev = -1;
    let acc = new Float32Array(N);

    const append = (x) => { const n = new Float32Array(inp.length + x.length); n.set(inp); n.set(x, inp.length); inp = n; };
    const at = (abs) => inp[abs - base];

    function run(out, stopAt = Infinity) {
      for (;;) {
        const target = Math.round(ideal);
        if (target >= stopAt) return;
        const lo = prev < 0 ? target : Math.max(base, target - T), hi = prev < 0 ? target : target + T;
        if (hi + N > base + inp.length) return;
        let p = lo;
        if (prev >= 0) {
          // The frame that sounds most like what would naturally have come next.
          const nat = prev + Hs;
          let best = -Infinity;
          for (let c = lo; c <= hi; c++) {
            let s = 0;
            for (let j = 0; j < Hs; j += 2) s += at(c + j) * at(nat + j);
            if (s > best) { best = s; p = c; }
          }
        }
        for (let i = 0; i < N; i++) acc[i] += win[i] * at(p + i);
        out.push(acc.slice(0, Hs));
        acc.copyWithin(0, Hs); acc.fill(0, N - Hs);
        prev = p; ideal += Ha;
        // Keep only what later frames can still reach.
        const keep = Math.max(base, Math.min(Math.round(ideal) - T, prev + Hs) - 1);
        if (keep > base) { inp = inp.slice(keep - base); base = keep; }
      }
    }

    const join = (parts) => {
      const n = parts.reduce((s, x) => s + x.length, 0), o = new Float32Array(n);
      let k = 0;
      for (const x of parts) { o.set(x, k); k += x.length; }
      return o;
    };

    return {
      sr, speed,
      push(x) { append(x); const out = []; run(out); return join(out); },
      // The reply is over: play out what's left, padded with silence so the last frames can be taken.
      flush() {
        const end = base + inp.length;
        append(new Float32Array(N * 2 + T * 2 + Math.ceil(Ha) * 2));
        const out = []; run(out, end); out.push(acc.slice(0, Hs));
        inp = new Float32Array(0); acc = new Float32Array(N);
        return join(out);
      },
    };
  }
  if (typeof window !== 'undefined' && window.Roamie) window.Roamie.stretcher = stretcher;
  if (typeof module !== 'undefined') module.exports = { stretcher };
})();
