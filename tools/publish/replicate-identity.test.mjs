// What makes a row a replicate, and what only makes it look like one.
//
// A replicate is the same cell measured twice. The run declares which cell it
// measured twice and that cell names itself, so the question the importer has to
// answer is "is this row that cell", not "does this row resemble that cell".
//
// It used to answer the second one. The key was the section the page draws a row
// in plus the tile count, and in the `engines` family every eight-thread cell
// shares both with its single-thread twin: 162 rows arrived as replicates where
// 18 belong, and the 144 that were not replicates of anything were the whole
// concurrency arm. That is the same collision K2.3 wrote down from the other
// direction, where 21851 is both `8192x8192@64+gradient` and `+noise`, so a tile
// count cannot identify a cell.
//
// Every test here runs against a real archived document, mutated at most in the
// one way it is about, and names the wrong implementation it goes red against.
// The control that holds the file up is the first test: it has to find the 18
// genuine replicates, because "zero replicates" passes every assertion below
// about rows that are not replicates and proves nothing at all.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

import { CONFIG, IMPORTER, emptyHistory, mutate, pristine, scratch } from './test-material.mjs';

const EXIT = { OK: 0, REFUSED: 1 };

const DECLARED_ENGINES_CELL = '512x360@256+c1';

function runImport({ dir, runId }, history, config = CONFIG) {
  const args = [
    IMPORTER,
    '--document', join(dir, `${runId}.json`),
    '--archive', dir,
    '--history', history,
    '--config', config,
  ];
  // Same retry as the importer's own suite, and for the same reason: a process
  // that could not be started returns no exit status, which reads downstream as
  // "did not exit 0" and is indistinguishable from a refusal.
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const r = spawnSync(process.execPath, args, { encoding: 'utf8' });
    if (!r.error && r.status !== null) {
      return { code: r.status, out: r.stdout ?? '', err: r.stderr ?? '' };
    }
    if (attempt === 1) {
      throw new Error(
        `the importer could not be run (${r.error?.message ?? 'no exit status'}); this is the ` +
          'harness failing, not the importer refusing',
      );
    }
  }
  throw new Error('unreachable');
}

/** Import a run and return its single history entry. */
function imported(run, config = CONFIG) {
  const history = emptyHistory();
  const r = runImport(run, history, config);
  assert.equal(r.code, EXIT.OK, `expected this run to import:\n${r.err}`);
  const entries = JSON.parse(readFileSync(history, 'utf8'));
  assert.equal(entries.length, 1);
  return entries[0];
}

/** The shipped config with one thing changed, written where the importer can read it. */
function configWith(edit) {
  const config = JSON.parse(readFileSync(CONFIG, 'utf8'));
  edit(config);
  const path = join(scratch(), 'config.json');
  writeFileSync(path, `${JSON.stringify(config, null, 2)}\n`);
  return path;
}

/** The archived engines run cut down to the rows a test is about.
 *
 *  `pick` chooses them out of the real document, so every field on every row is
 *  still the producer's, and `relabel` then changes the one thing the test
 *  varies. Nothing here writes a row from scratch: a hand-built row agrees with
 *  the test by construction and would let a green result mean nothing.
 */
function engineRows(pick, relabel = () => {}) {
  return mutate({
    family: 'engines',
    edit: (doc) => {
      const chosen = pick(doc.cells).map((c) => structuredClone(c));
      assert.ok(chosen.length > 0, 'the row picker found nothing, so this test is about nothing');
      chosen.forEach(relabel);
      doc.cells = chosen;
    },
  });
}

/** The measured rows of one engine and one metric, by cell name. */
const rowsFor = (backend, key, names) => (cells) => {
  const found = names.map((name) =>
    cells.find(
      (c) => c.outcome === 'ok' && c.backend === backend && c.key === key && c.cell === name,
    ),
  );
  assert.ok(found.every(Boolean), `the archived run has no ${backend}/${key} row for each of ${names}`);
  return found;
};

// --- the acceptance criterion, and the control that makes it mean something ---

test('the engines run yields 18 replicates and publishes its eight-thread arm', () => {
  // RED against keying replicate detection on anything the `+c8` cells share
  // with their `+c1` twins: the section key and the tile count are shared by
  // construction, so the pre-fix importer reported 144 samples and 162
  // replicates, and 144 of those replicates were the concurrency experiment.
  const entry = imported(pristine('engines'));

  assert.equal(entry.replicate.cell, DECLARED_ENGINES_CELL);
  assert.equal(entry.replicates.length, 18);

  // The positive control. Eighteen is 3 engines x 6 metrics of one cell measured
  // a second time, so detection is still finding the genuine pass rather than
  // having been switched off: an importer that never files a replicate would
  // satisfy every "is not a replicate" assertion in this file.
  assert.ok(
    entry.replicates.every((s) => s.cell === DECLARED_ENGINES_CELL),
    'every replicate must be the cell the run says it measured twice',
  );
  assert.equal(new Set(entry.replicates.map((s) => `${s.library}/${s.key}`)).size, 18);
  assert.equal(new Set(entry.replicates.map((s) => s.library)).size, 3);

  // 16 cells x 3 engines x 6 metrics, the 306 measured rows less the 18 the
  // replicate pass repeats.
  assert.equal(entry.samples.length, 288);
  assert.equal(entry.samples.filter((s) => s.cell === DECLARED_ENGINES_CELL).length, 18);

  // The arm the defect swallowed, and the reason it matters: filed as
  // replicates, the concurrency story stops being renderable.
  const eightThread = entry.samples.filter((s) => s.cell.endsWith('+c8'));
  assert.equal(eightThread.length, 144);
  assert.equal(new Set(eightThread.map((s) => s.cell)).size, 8);
  assert.ok(
    !entry.replicates.some((s) => s.cell.endsWith('+c8')),
    'an eight-thread cell is a measurement of a different thing, not a second pass',
  );

  // And the identity has to identify: no two published samples may be the same
  // row. The pre-fix key would fail this on the engines run, which is what
  // filing 144 of them as replicates was hiding.
  const ids = entry.samples.map((s) => `${s.backend}/${s.cell}/${s.source}/${s.key}`);
  assert.equal(new Set(ids).size, ids.length, 'a sample identity must identify one sample');
});

// --- the two axes a cell can differ on that a tile count cannot see -----------

test('two cells differing only in concurrency are not replicates of one another', () => {
  // The acceptance criterion in its smallest form: one metric, one engine, two
  // thread budgets. Same tile count, same source, same section, different cell.
  // RED against the pre-fix key, which files the second row as a second pass of
  // the first.
  const pair = engineRows(
    rowsFor('mapreduce', 'pyramid.wall', ['8192x5760@256+c1', '8192x5760@256+c8']),
  );
  const entry = imported(pair);
  assert.equal(entry.samples.length, 2);
  assert.equal(entry.replicates.length, 0);
  assert.deepEqual(entry.samples.map((s) => s.cell).sort(), [
    '8192x5760@256+c1',
    '8192x5760@256+c8',
  ]);
  // They are not the same measurement, which is the fact the collision denied:
  // the concurrency arm exists because these two numbers differ.
  assert.notEqual(entry.samples[0].median, entry.samples[1].median);

  // The control on the control. In a two-row document "no replicates" is also
  // what you get from an importer that has stopped detecting them, so the same
  // two rows with the same cell name must still come out as one sample and one
  // replicate.
  const same = engineRows(
    rowsFor('mapreduce', 'pyramid.wall', ['8192x5760@256+c1', '8192x5760@256+c8']),
    (c) => {
      c.cell = '8192x5760@256+c1';
    },
  );
  const collapsed = imported(same);
  assert.equal(collapsed.samples.length, 1);
  assert.equal(collapsed.replicates.length, 1);
});

test('two cells differing only in source are not replicates of one another', () => {
  // K2.3's collision, in the family that cannot spell its way out of it. A
  // storage cell is named `8192x8192@64+gradient`, so the source is inside the
  // name; an engines cell is named `512x360@256+c1` and is not, so two sources
  // of one engines cell share everything the name carries.
  //
  // GREEN today, and the reason is worth writing down rather than being
  // reassured by: it passes because `sections.scenarioFrom` happens to list
  // `source`, which is a decision about how the page groups rows for drawing. It
  // is RED against an identity keyed on the cell name alone, and the test below
  // it is the one that shows the identity no longer borrows the section key.
  let index = 0;
  const pair = engineRows(
    rowsFor('streaming', 'pyramid.wall', ['4096x2880@256+c1', '4096x2880@256+c8']),
    (c) => {
      // One cell name, two source images: the shape an engines run would have
      // the day it sweeps a second source, and the shape a storage run has
      // whenever the name is read without it.
      c.cell = '4096x2880@256+c1';
      c.source = index++ === 0 ? 'gradient' : 'noise';
    },
  );
  const entry = imported(pair);
  assert.equal(entry.samples.length, 2);
  assert.equal(entry.replicates.length, 0);
  assert.deepEqual(entry.samples.map((s) => s.source).sort(), ['gradient', 'noise']);
});

test('replicate identity does not borrow the key the page draws sections with', () => {
  // `sections.scenarioFrom` decides how rows are grouped into sections on the
  // page, and the importer's own default for it is `['key']`. Reading identity
  // out of it means a decision about drawing silently decides which rows are
  // published at all.
  //
  // RED today: with `scenarioFrom: ['key']` the storage run's gradient and noise
  // cells collide on tile count, and the importer files 142 replicates and 190
  // samples instead of 48 and 284. The numbers asserted here are the ones the
  // shipped config produces, so the point is that they no longer move when that
  // knob does.
  const sectionsByKeyAlone = configWith((config) => {
    config.sections.scenarioFrom = ['key'];
  });
  const entry = imported(pristine('storage'), sectionsByKeyAlone);

  assert.equal(entry.replicates.length, 48);
  assert.equal(entry.samples.length, 284);
  assert.ok(
    entry.replicates.every((s) => s.cell === '2048x2048@256+gradient'),
    'the storage run declares one replicate cell and nothing else is one',
  );
  // The pair the tile count cannot tell apart, both published as themselves.
  const collide = entry.samples.filter((s) => s.scale === 21851 && s.key === 'generate.wall');
  assert.deepEqual(collide.map((s) => s.source).sort(), ['gradient', 'noise']);
});

// --- and the hole an identity can have --------------------------------------

test('a measured cell with no cell name is refused, not treated as every other row', () => {
  // The failure this whole epic keeps meeting: a missing key falling back to
  // something that looks safe. An identity built by skipping absent fields turns
  // a document with no `cell` on it into one row repeated 305 times, and the
  // importer reports a large, confident replicate count for a run it never
  // identified. RED against building the identity with a `.filter()` over the
  // fields that happen to be there.
  const stripped = mutate({
    family: 'engines',
    edit: (doc) => {
      for (const c of doc.cells) delete c.cell;
    },
  });
  const history = emptyHistory();
  const r = runImport(stripped, history);
  assert.equal(r.code, EXIT.REFUSED, `expected a refusal, got ${r.code}:\n${r.err}`);
  assert.match(r.err, /REFUSED\. This run may not be published/);
  assert.doesNotMatch(r.err, /^\s+at .*:\d+:\d+/m, 'a stack trace is a crash, not a refusal');
  assert.match(r.err, /carry no `cell`/);
});
