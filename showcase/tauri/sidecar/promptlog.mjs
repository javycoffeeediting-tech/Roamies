// Prompt history: every request asked (spoken or typed), one line each with when and how, appended to a plain text
// file the user can read and count (prompt_history.txt in a Roamies folder in their home folder, by default).

import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';

export const HISTORY_FILE = path.join(os.homedir(), 'Roamies', 'prompt_history.txt');

const two = (n) => String(n).padStart(2, '0');
/** Local time, sortable: 2026-11-13 14:02:11. */
export const stamp = (d) => `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())} ${two(d.getHours())}:${two(d.getMinutes())}:${two(d.getSeconds())}`;

/** One line: "<time>\t<voice|typed>\t<what was asked, on one line>", or null when there's nothing to log. */
export function historyLine(text, { voice = false, at = new Date() } = {}) {
  const one = String(text ?? '').replace(/\s+/g, ' ').trim();
  return one ? `${stamp(at)}\t${voice ? 'voice' : 'typed'}\t${one}\n` : null;
}

// Appends run one after another, so lines never interleave.
let queue = Promise.resolve();
export function logPrompt(text, opts = {}, file = HISTORY_FILE) {
  const line = historyLine(text, opts);
  if (!line) return queue;
  queue = queue.then(async () => {
    await fs.mkdir(path.dirname(file), { recursive: true });
    await fs.appendFile(file, line, 'utf8');
  }).catch((e) => console.error('prompt history', e.message));
  return queue;
}
