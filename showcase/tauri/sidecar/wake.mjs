// Wake phrases: what the user can say to start talking hands-free. On Windows and Linux the voice helper listens
// for the user's own list (voice-win/src/wake.rs matches them, near-misses included); the Mac helper listens for
// "Hey Roamies" only.

export const DEFAULT_WAKE_WORDS = ['Hey Roamies'];
export const MAX_WAKE_WORDS = 8;
export const MAX_WAKE_LENGTH = 40;

/** Whether the phrases can be changed on this platform (the Parakeet helper's). */
export const customWake = (platform) => platform === 'win32' || platform === 'linux';

/** The user's phrases, from a list or from text with one per line or comma: tidied, blanks and repeats dropped,
 *  at most MAX_WAKE_WORDS of at most MAX_WAKE_LENGTH characters each. */
export function cleanWakeWords(given) {
  const raw = Array.isArray(given) ? given : typeof given === 'string' ? given.split(/[,\n]/) : [];
  const out = [];
  const seen = new Set();
  for (const item of raw) {
    if (typeof item !== 'string') continue;
    const phrase = item.replace(/\s+/g, ' ').trim().slice(0, MAX_WAKE_LENGTH).trim();
    // Compared the way the helper hears them: letters and digits only, any case.
    const key = phrase.toLowerCase().replace(/[^\p{L}\p{N}]/gu, '');
    if (!key || seen.has(key)) continue;
    seen.add(key);
    out.push(phrase);
    if (out.length === MAX_WAKE_WORDS) break;
  }
  return out;
}

/** The phrases listened for: the user's (where they can be changed, and some are set), else "Hey Roamies". */
export const wakeWordsFor = (platform, saved) => {
  if (!customWake(platform)) return DEFAULT_WAKE_WORDS;
  const words = cleanWakeWords(saved);
  return words.length ? words : DEFAULT_WAKE_WORDS;
};

/** "Hey Roamies", or "Hey Roamies" or "Jarvis": for the panel and the agent's prompt. */
export const sayWakeWords = (words) => {
  const q = words.map((w) => `“${w}”`);
  return q.length < 2 ? (q[0] ?? '') : `${q.slice(0, -1).join(', ')} or ${q.at(-1)}`;
};
