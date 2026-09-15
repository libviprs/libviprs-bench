// Every config path the importer reads has to be one the shipped config defines,
// or an absence that is deliberate and written down.
//
// This exists because ten keys were read and never defined, and every one of
// them fell back to a default that refused. The failure did not look like a
// broken lookup, it looked like a correct refusal: `publishableProfiles`
// defaulted to `[]`, so a `full` sweep was turned away by a sentence explaining
// why `ci` sweeps are turned away, and the only tell was an empty parenthesised
// list in the message. Two of the empty defaults were worse than noisy, they
// were silent: `invariantsExactWithinCommit` and `invariantsFilesystemDependent`
// both defaulted to an empty set, so the invariant-moved-within-a-commit
// refusal was checking nothing.
//
// The importer's own suite could not catch this, because it passes its own
// config and its own `--archive`, so the fallbacks are never walked. That is the
// same shape as the fixture problem this epic hit twice before: the test and the
// product were not looking at the same object.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '..', '..');
const source = readFileSync(join(here, 'import-run.mjs'), 'utf8');
const config = JSON.parse(readFileSync(join(repo, 'tools', 'contract', 'config.json'), 'utf8'));

/** Keys the code reads and the config deliberately leaves out, each with why. */
const INTENTIONAL_DEFAULTS = new Map([
  ['archiveBucketFrom', 'the default field list is causl\'s and libviprs has no reason to differ yet'],
  ['archiveDirByFamily', 'only needed when one family archives somewhere the prefix rule cannot derive'],
  ['declaredSuffix', 'the default `_declared` is the producer\'s own spelling'],
  ['refuse', 'the majorityNoisyCells check runs unless a config switches it off, and none does'],
  ['runnerToSeries', 'defined but empty, because seriesFrom already resolves every series this producer emits'],
]);

/** Strip line comments, so a key named only in the usage doc is not read as a read. */
function codeOnly(src) {
  return src
    .split('\n')
    .map((line) => (line.trimStart().startsWith('//') ? '' : line))
    .join('\n');
}

function pathsReadFrom(src) {
  const found = new Set();
  for (const m of codeOnly(src).matchAll(/producer\.([A-Za-z][A-Za-z0-9]*)/g)) found.add(m[1]);
  return [...found].sort();
}

/** Keys named in the usage comment, which must still be keys that exist. */
function pathsDocumented(src) {
  const found = new Set();
  for (const line of src.split('\n')) {
    if (!line.trimStart().startsWith('//')) continue;
    for (const m of line.matchAll(/producer\.([A-Za-z][A-Za-z0-9]*)/g)) found.add(m[1]);
  }
  return [...found].sort();
}

test('every producer key the importer reads is defined or an intentional default', () => {
  const read = pathsReadFrom(source);
  // A positive control: the walk has to actually find keys, or a green result
  // below means the regex stopped matching rather than the config being right.
  assert.ok(read.length >= 10, `expected the source walk to find keys, found ${read.length}`);

  const missing = read.filter(
    (k) => !(k in (config.producer ?? {})) && !INTENTIONAL_DEFAULTS.has(k),
  );
  assert.deepEqual(
    missing,
    [],
    `these keys are read by import-run.mjs and neither defined in config.json nor listed as an ` +
      `intentional default: ${missing.join(', ')}`,
  );
});

test('no config key is defined and read by nothing', () => {
  const read = new Set(pathsReadFrom(source));
  const defined = Object.keys(config.producer ?? {});
  const orphans = defined.filter((k) => !read.has(k));
  assert.deepEqual(
    orphans,
    [],
    `these keys sit in config.json and nothing reads them, which is how archiveDir came to be ` +
      `set correctly while the code looked at archiveRoot: ${orphans.join(', ')}`,
  );
});

test('a full sweep is publishable and a ci sweep is not', () => {
  const p = config.producer?.publishableProfiles;
  assert.ok(Array.isArray(p) && p.length > 0, 'an empty list makes every profile unpublishable');
  assert.ok(p.includes('full'), 'full is the calibrated sweep and is the thing worth publishing');
  assert.ok(!p.includes('ci'), 'ci proves the harness runs and is never a measurement');
});

test('the invariant refusal is checking a non-empty set', () => {
  const exact = config.producer?.invariantsExactWithinCommit ?? [];
  assert.ok(exact.length > 0, 'an empty set means the invariant-moved refusal checks nothing');
  for (const name of ['output_bytes', 'filesystem_entries', 'tiles_produced']) {
    assert.ok(exact.includes(name), `${name} reproduced byte for byte across every export this epic saw`);
  }
  const fsDep = config.producer?.invariantsFilesystemDependent ?? [];
  assert.ok(
    fsDep.includes('allocated_bytes'),
    'allocated_bytes is st_blocks * 512 and the first full capture caught it moving between reps',
  );
});

test('the usage comment does not name a key that no longer exists', () => {
  // `producer.archiveDir` outlived the key it documented, which is how a reader
  // ends up setting something nothing consults.
  const documented = pathsDocumented(source);
  const known = new Set([...Object.keys(config.producer ?? {}), ...INTENTIONAL_DEFAULTS.keys()]);
  const stale = documented.filter((k) => !known.has(k));
  assert.deepEqual(stale, [], `the usage comment names keys that do not exist: ${stale.join(', ')}`);
});
