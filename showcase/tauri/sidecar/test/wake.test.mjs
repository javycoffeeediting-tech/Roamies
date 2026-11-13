import assert from 'node:assert/strict';
import { DEFAULT_WAKE_WORDS, MAX_WAKE_LENGTH, MAX_WAKE_WORDS, cleanWakeWords, customWake, sayWakeWords, wakeWordsFor } from '../wake.mjs';

// The user's wake phrases: typed as a list in the panel, tidied before they're saved and sent to the voice helper.
assert.deepEqual(cleanWakeWords(['  Hey   Jarvis ', 'Computer']), ['Hey Jarvis', 'Computer']);
assert.deepEqual(cleanWakeWords('Hey Jarvis, Computer\nOK Roamie'), ['Hey Jarvis', 'Computer', 'OK Roamie'], 'one per comma or line');
assert.deepEqual(cleanWakeWords(['Hey Jarvis', 'hey, jarvis!', '...', '', 42, null]), ['Hey Jarvis'], 'repeats (as heard), blanks and junk dropped');
assert.equal(cleanWakeWords(['x'.repeat(100)])[0].length, MAX_WAKE_LENGTH);
assert.equal(cleanWakeWords(Array.from({ length: 20 }, (_, i) => `Phrase ${i}`)).length, MAX_WAKE_WORDS);
assert.deepEqual(cleanWakeWords(undefined), []);
assert.deepEqual(cleanWakeWords({ words: ['a'] }), []);
assert.deepEqual(cleanWakeWords(['Hola Señora']), ['Hola Señora'], 'letters beyond English count');

// Where they can be changed, and what's listened for.
assert.ok(customWake('win32') && customWake('linux') && !customWake('darwin'));
assert.deepEqual(wakeWordsFor('darwin', ['Jarvis']), DEFAULT_WAKE_WORDS, 'the Mac helper hears "Hey Roamies" only');
assert.deepEqual(wakeWordsFor('win32', ['Jarvis']), ['Jarvis']);
assert.deepEqual(wakeWordsFor('win32', []), DEFAULT_WAKE_WORDS, 'an emptied list falls back to the default');
assert.deepEqual(wakeWordsFor('linux', undefined), DEFAULT_WAKE_WORDS);

assert.equal(sayWakeWords(['Hey Roamies']), '“Hey Roamies”');
assert.equal(sayWakeWords(['Hey Roamies', 'Jarvis']), '“Hey Roamies” or “Jarvis”');
assert.equal(sayWakeWords(['A', 'B', 'C']), '“A”, “B” or “C”');
assert.equal(sayWakeWords([]), '');
console.log('wake tests: passed');
