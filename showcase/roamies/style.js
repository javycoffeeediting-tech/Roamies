// The look of everything around the characters (Settings › Appearance › Style): "retro", the LED panel world
// (pixel frames, Martian Mono and Doto), or "modern" (rounded, flat glass, the system's own sans-serif type).
// The canvas code names its fonts by family, so modern adds faces under those same names, built from the system's
// sans (faces added here win over the stylesheet's), and retro takes them away again. glass.js and ink.js ask
// R.modern() when they draw a frame; agent.js when it lays out the bubble's words.
(() => {
  const R = window.Roamie;
  if (!R || typeof document === 'undefined') return;
  R.SANS = '"Segoe UI Variable Text", "Segoe UI", system-ui, -apple-system, "Helvetica Neue", Ubuntu, Cantarell, Arial, sans-serif';
  R.style = 'retro';
  R.modern = () => R.style === 'modern';
  // The system's sans by weight: regular, semibold and bold faces, whichever this computer has.
  const LOCAL = {
    regular: ['Segoe UI Variable Text', 'Segoe UI', 'SF Pro Text', 'Helvetica Neue', 'Ubuntu', 'Cantarell', 'Arial'],
    semi: ['Segoe UI Variable Text Semibold', 'Segoe UI Semibold', 'SF Pro Text Semibold', 'Helvetica Neue Medium', 'Ubuntu Medium', 'Arial'],
    bold: ['Segoe UI Variable Text Bold', 'Segoe UI Bold', 'SF Pro Text Bold', 'Helvetica Neue Bold', 'Ubuntu Bold', 'Cantarell Bold', 'Arial Bold'],
  };
  const src = (names) => names.map((n) => `local("${n}")`).join(', ');
  let faces = [];
  R.setStyle = (style) => {
    const next = style === 'modern' ? 'modern' : 'retro';
    document.documentElement.dataset.style = next;
    if (next === R.style) return;
    R.style = next;
    for (const f of faces) document.fonts.delete(f);
    faces = [];
    if (next === 'modern' && typeof FontFace !== 'undefined') {
      for (const family of ['Martian Mono', 'Doto']) {
        for (const [weight, names] of [['100 449', LOCAL.regular], ['450 649', LOCAL.semi], ['650 1000', LOCAL.bold]]) {
          const f = new FontFace(family, src(names), { weight, stretch: '50% 200%' });
          faces.push(f);
          document.fonts.add(f);
          f.load().then(() => R.wake?.()).catch(() => {});
        }
      }
    }
    R.wake?.();
  };
})();
