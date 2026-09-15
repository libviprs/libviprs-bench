// A refusal may not present an empty enumeration as if it were the rule.
//
// `profile "ci" is not publishable ()` was the visible symptom of #82: ten
// config keys the importer read and the config never defined, each falling back
// to a default that refused. Every one of those refusals was a correct-looking
// sentence about the document, and the only thing on screen that said otherwise
// was the empty pair of brackets at the end of one of them.
//
// #82 defined the keys. This file is about the shape, which #82 left exactly as
// it was: a rule with nothing in it refuses every document that reaches it, and
// the refusal it prints is written as a verdict on the run. So an empty allowed
// set now reports as a CONFIGURATION FAULT and short-circuits the report before
// any document verdict is printed.
//
// Every test here runs the real importer against the real archived document
// with one thing changed, the same rule `test-material.mjs` sets out, and each
// names the wrong implementation it goes red against.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

import { CONFIG, IMPORTER, emptyHistory, pristine, scratch } from './test-material.mjs';

const EXIT = { OK: 0, REFUSED: 1, USAGE: 2 };

/** The shipped config with one producer key replaced, written to a temp file. */
function configWith(producerPatch) {
  const config = JSON.parse(readFileSync(CONFIG, 'utf8'));
  config.producer = { ...config.producer, ...producerPatch };
  const path = join(scratch(), 'config.json');
  writeFileSync(path, JSON.stringify(config, null, 2));
  return path;
}

function runImport(config) {
  const { dir, runId } = pristine();
  const result = spawnSync(
    process.execPath,
    [
      IMPORTER,
      '--document', join(dir, `${runId}.json`),
      '--archive', dir,
      '--history', emptyHistory(),
      '--config', config,
    ],
    { encoding: 'utf8' },
  );
  if (result.error || result.status === null) {
    throw new Error(
      `the importer could not be run (${result.error?.message ?? 'no exit status'}); ` +
        'this is the harness failing, not the importer refusing',
    );
  }
  return { code: result.status, out: result.stdout ?? '', err: result.stderr ?? '' };
}

// The control the rest of this file hangs on. Every assertion below is that
// something is refused, and a suite of those passes trivially against an
// importer that refuses everything.
test('the archived run imports against the shipped config', () => {
  const r = runImport(CONFIG);
  assert.equal(r.code, EXIT.OK, `${r.err}\n${r.out}`);
});

/** Every allowed set that, emptied, must report as a configuration fault. */
const ALLOWED_SETS = [
  ['publishableProfiles', 'the #82 symptom itself'],
  ['families', 'every document becomes an unknown family'],
  ['attestedFrom', 'no cell carries a field the importer looks at'],
  ['invariantNames', 'every invariant becomes a stray'],
  ['invariantsExactWithinCommit', 'the invariant-moved check compares nothing'],
];

for (const [key, why] of ALLOWED_SETS) {
  // RED against the importer as #82 left it: with the key emptied it prints a
  // refusal about the document, and for `publishableProfiles` that refusal is
  // literally `profile "full" is not publishable ()`.
  test(`an empty producer.${key} refuses as a config fault, not a verdict (${why})`, () => {
    const r = runImport(configWith({ [key]: [] }));
    assert.equal(r.code, EXIT.REFUSED, `${r.err}\n${r.out}`);
    assert.match(
      r.err,
      /REFUSED, and not because of this run/,
      `an empty ${key} has to say the config is the problem:\n${r.err}`,
    );
    assert.match(r.err, new RegExp(`producer\\.${key} is `), r.err);
    assert.doesNotMatch(
      r.err,
      /^REFUSED\. This run may not be published/m,
      `an empty ${key} must not print a single verdict about the run:\n${r.err}`,
    );
  });
}

// RED against every version of this file that formats an allowed set with a
// bare `.join(', ')`. The brackets are what a reader sees, so the brackets are
// what is asserted, and `()` is the exact string #82 shipped.
test('no refusal anywhere prints an empty parenthesised list', () => {
  for (const [key] of ALLOWED_SETS) {
    const r = runImport(configWith({ [key]: [] }));
    assert.equal(r.code, EXIT.REFUSED);
    assert.doesNotMatch(
      r.err,
      /\(\)/,
      `emptying ${key} produced an empty parenthesised list, which is the #82 message:\n${r.err}`,
    );
    assert.doesNotMatch(
      r.err,
      /: *$/m,
      `emptying ${key} produced a refusal that trails off into nothing:\n${r.err}`,
    );
  }
});

// RED against a fix that only reaches the key #82 happened to expose. Emptying
// `invariantNames` on the archived document refuses through a different
// sentence, and the check that reads `invariantsExactWithinCommit` refuses
// through no sentence at all: it just stops comparing. Both are the same defect
// and neither is visible from the `publishableProfiles` message.
test('a rule that would fail silently is still a config fault', () => {
  const r = runImport(configWith({ invariantsExactWithinCommit: [] }));
  assert.equal(r.code, EXIT.REFUSED, `${r.err}\n${r.out}`);
  assert.match(r.err, /REFUSED, and not because of this run/, r.err);
  assert.match(r.err, /compares nothing/, r.err);
});

// RED against a `ruleSet` that treats every empty set as a fault. Both shipped
// configs leave `outcomes.refusing` empty on purpose: no outcome is refused on
// its name alone today, and turning that into a fault would refuse the archived
// run for being configured the way it is configured.
test('an outcome set that is legitimately empty is not a fault', () => {
  const r = runImport(configWith({ outcomes: { measured: ['ok'], structural: ['failed', 'refused'], refusing: [] } }));
  assert.equal(r.code, EXIT.OK, `${r.err}\n${r.out}`);
});

// ... and the whole classification going empty still is.
test('an outcome classification with nothing in it is a config fault', () => {
  const r = runImport(configWith({ outcomes: { measured: [], structural: [], refusing: [] } }));
  assert.equal(r.code, EXIT.REFUSED, `${r.err}\n${r.out}`);
  assert.match(r.err, /REFUSED, and not because of this run/, r.err);
  assert.match(r.err, /producer\.outcomes is /, r.err);
});
