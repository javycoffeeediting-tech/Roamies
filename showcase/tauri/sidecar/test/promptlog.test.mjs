import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { historyLine, logPrompt, stamp } from '../promptlog.mjs';

// Prompt history: one tab-separated line per request, in local time, newlines folded so each request is one line.
const at = new Date(2026, 10, 13, 9, 5, 7);
assert.equal(stamp(at), '2026-11-13 09:05:07');
assert.equal(historyLine('  what time\n is it ', { voice: true, at }), '2026-11-13 09:05:07\tvoice\twhat time is it\n');
assert.equal(historyLine('open mail', { at }), '2026-11-13 09:05:07\ttyped\topen mail\n');
assert.equal(historyLine('   '), null);
assert.equal(historyLine(undefined), null);

// Appended in order to a file whose folder is made when missing.
const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'roamies-history-'));
const file = path.join(dir, 'nested', 'prompt_history.txt');
logPrompt('first', { at }, file);
logPrompt('', {}, file);
await logPrompt('second', { voice: true, at }, file);
assert.equal(await fs.readFile(file, 'utf8'), '2026-11-13 09:05:07\ttyped\tfirst\n2026-11-13 09:05:07\tvoice\tsecond\n');
await fs.rm(dir, { recursive: true, force: true });
console.log('promptlog tests: passed');
