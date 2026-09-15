// The importer's tests.
//
// Every refusal fires on the real archived document with exactly one thing
// changed (see `test-material.mjs` for why that rule is not a style note), and
// every one of them names the wrong implementation it goes red against.
//
// The control that holds the whole suite up is `the archived run imports`: a
// suite of refusals passes trivially against an importer that refuses
// everything, so the first thing proven is that the real run gets in.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

import {
  ARCHIVED_RUN_ID,
  CONFIG,
  IMPORTER,
  emptyHistory,
  mutate,
  pristine,
  scratch,
} from './test-material.mjs';

const EXIT = { OK: 0, REFUSED: 1, USAGE: 2 };

function runImport({ dir, runId }, history, extra = []) {
  const args = [
    IMPORTER,
    '--document', join(dir, `${runId}.json`),
    '--archive', dir,
    '--history', history,
    '--config', CONFIG,
    ...extra,
  ];
  // A process that could not be started is not a verdict. Under load this
  // returns `status: null` with an `error`, which reads downstream as "the
  // importer did not exit 0" and is indistinguishable from a refusal: that is
  // how a flaky container turns into a mutation the table believes it killed.
  // One retry, and then a failure that says what actually happened.
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const result = spawnSync(process.execPath, args, { encoding: 'utf8' });
    if (!result.error && result.status !== null) {
      return { code: result.status, out: result.stdout ?? '', err: result.stderr ?? '' };
    }
    if (attempt === 1) {
      throw new Error(
        `the importer could not be run (${result.error?.message ?? 'no exit status'}); ` +
          'this is the harness failing, not the importer refusing',
      );
    }
  }
  throw new Error('unreachable');
}

function readHistory(path) {
  return JSON.parse(readFileSync(path, 'utf8'));
}

/** Assert a refusal, and that it is a refusal rather than a crash.
 *
 *  The mutation table found two tests passing for the wrong reason here: with
 *  the check they were aiming at deleted outright, the importer threw, and the
 *  stack trace happened to contain the word the test was matching on. An
 *  uncaught exception and a refusal are both exit 1 and both go to stderr, so
 *  every refusal test asserts the banner and the absence of a stack: a crash
 *  cannot produce the first and cannot avoid the second.
 */
function refused(r, ...patterns) {
  assert.equal(r.code, EXIT.REFUSED, `expected a refusal, got ${r.code}:\n${r.err}`);
  assert.match(r.err, /REFUSED\. This run may not be published/);
  assert.doesNotMatch(r.err, /^\s+at .*:\d+:\d+/m, 'a stack trace is a crash, not a refusal');
  for (const pattern of patterns) assert.match(r.err, pattern);
}

/** Import the real run into a fresh history and return the single entry. */
function importedEntry() {
  const history = emptyHistory();
  const run = pristine();
  const r = runImport(run, history);
  assert.equal(r.code, EXIT.OK, `expected the real run to import:\n${r.err}`);
  const entries = readHistory(history);
  assert.equal(entries.length, 1);
  return entries[0];
}

// --- the control, and the acceptance criteria -------------------------------

test('the archived run imports and produces exactly one entry', () => {
  const entry = importedEntry();
  assert.equal(entry.runId, ARCHIVED_RUN_ID);
  assert.equal(entry.commit, '809ee8014d002518ce55edaceba698ca7a8b8a79');
  assert.equal(entry.integrity.document.startsWith('sha256:6a712af3'), true);
});

test('importing the same archived run twice leaves one entry', () => {
  // RED against an importer that appends, which draws one measurement as two
  // points on a trend line and makes a re-run look like a flat regression.
  const history = emptyHistory();
  const run = pristine();
  assert.equal(runImport(run, history).code, EXIT.OK);
  const first = readHistory(history);
  assert.equal(runImport(run, history).code, EXIT.OK);
  const second = readHistory(history);
  assert.equal(second.length, 1);
  assert.deepEqual(second, first);
});

test('history is one entry per line so a diff over it is readable', () => {
  // RED against `JSON.stringify(history, null, 2)`, which turns "we added a
  // sweep" into a fifty thousand line diff, and against a single compact array,
  // which turns it into an unreviewable "1 line changed".
  const history = emptyHistory();
  const run = pristine();
  assert.equal(runImport(run, history).code, EXIT.OK);
  const text = readFileSync(history, 'utf8');
  const lines = text.split('\n');
  assert.equal(lines[0], '[');
  assert.equal(lines[lines.length - 2], ']');
  const entryLines = lines.slice(1, -2);
  assert.equal(entryLines.length, 1);
  for (const line of entryLines) {
    assert.doesNotMatch(line, /\n/);
    JSON.parse(line.replace(/,$/, ''));
  }
});

test('the entry says which family it came from and so does every sample', () => {
  // Two families feed this page. RED against an importer that drops the family,
  // which merges a `storage` generate.wall and an `engines` generate.wall into
  // one line on one chart.
  const entry = importedEntry();
  assert.equal(entry.family, 'storage');
  assert.equal(entry.runner, 'libviprs-storage');
  assert.ok(entry.samples.length > 0);
  assert.ok(entry.samples.every((s) => s.family === 'storage'));
  assert.ok(entry.skipped.every((s) => s.family === 'storage'));
});

// --- the refusals -----------------------------------------------------------

test('an emulated run is refused', () => {
  // The published PMTiles numbers this epic replaced were almost certainly
  // Rosetta and nothing recorded it. RED against an importer that reads the
  // dirty flag and not the probe.
  const r = runImport(mutate({ set: { 'provenance.emulated': true } }), emptyHistory());
  refused(r, /emulat/i);
});

test('an unknown emulation verdict is refused', () => {
  // RED against `emulated !== true`, which admits every run whose probe could
  // not tell. An unobserved run is not a native one.
  const r = runImport(mutate({ set: { 'provenance.emulated': 'unknown' } }), emptyHistory());
  refused(r, /emulat/i);
});

test('an absent emulation verdict is refused', () => {
  // "absent" is the state the published PMTiles numbers are in. RED against a
  // truthiness check, which reads a missing key as false and admits it.
  const r = runImport(mutate({ remove: ['provenance.emulated'] }), emptyHistory());
  refused(r, /emulat/i);
});

test('a document whose digests do not verify is refused', () => {
  // One measurement edited and the integrity block left alone. RED against an
  // importer that trusts the digests it was handed instead of recomputing them:
  // a number nobody can tie to a file is not evidence.
  const run = mutate({
    reseal: false,
    edit(doc) {
      doc.cells[0].median = doc.cells[0].median * 2;
    },
  });
  const r = runImport(run, emptyHistory());
  refused(r, /digests do not verify/, /cells/);
});

test('a document carrying no integrity block at all is refused', () => {
  const r = runImport(mutate({ remove: ['integrity'], reseal: false }), emptyHistory());
  refused(r, /carries no `integrity` block/);
});

test('a document that is not archived is refused', () => {
  // A sealed document sitting in a working tree is not citable. RED against an
  // importer that reads the document and never the archive.
  const run = mutate({ index: false });
  const r = runImport(run, emptyHistory());
  // Not "exit 1 with the words index.json somewhere in it": with this check
  // deleted the importer threw reading the file that is not there, and the
  // stack trace carried the phrase the test was matching on.
  refused(r, /has no index\.json/, /not citable/);
});

test('a document whose digest is not the one the index records is refused', () => {
  // The document verifies against itself and the archive says something else,
  // which is what a swapped file looks like. RED against an importer that
  // verifies internally and never joins to the index.
  const run = mutate({ edit(doc) { doc.cells[0].median += 1; } });
  const index = JSON.parse(readFileSync(join(run.dir, 'index.json'), 'utf8'));
  index[0].documentDigest = 'sha256:' + '0'.repeat(64);
  writeFileSync(join(run.dir, 'index.json'), JSON.stringify(index, null, 2));
  const r = runImport(run, emptyHistory());
  refused(r, /the archive index records documentDigest/);
});

test('the ci profile is refused', () => {
  // `ci` proves the harness runs and is never a measurement, and it archives
  // indistinguishably from `full`. RED against an importer that admits any
  // profile, which lets a three-rep smoke run sit in the same era as a
  // calibrated sweep.
  // Both the label and the resolved invocation, so the only check that can
  // refuse this is the publishable-profile one. With only the label changed, the
  // agreement check below fired instead and this test passed against an importer
  // that publishes every profile there is.
  const r = runImport(
    mutate({ set: { profile: 'ci', 'provenance.invocation.resolved.profile': 'ci' } }),
    emptyHistory(),
  );
  refused(r, /is not publishable/, /\bci\b/);
});

test('a profile the document and its invocation disagree about is refused', () => {
  // RED against an importer that reads one of the two. A document that says
  // `full` over an invocation that resolved `ci` is a relabelled smoke run.
  const r = runImport(
    mutate({ set: { 'provenance.invocation.resolved.profile': 'ci' } }),
    emptyHistory(),
  );
  refused(r, /invocation resolved/);
});

test('an invariant that moved within one commit is refused', () => {
  // `filesystem_entries`, `tiles_produced` and `output_bytes` reproduced byte
  // for byte across every export this epic has seen. A change at a fixed commit
  // is a defect, not a delta.
  //
  // RED against an importer that only ever appends, which charts the defect as
  // a step and calls it a finding.
  const history = emptyHistory();
  assert.equal(runImport(pristine(), history).code, EXIT.OK);

  const second = mutate({
    set: { startedAt: '2026-09-14T16:00:00.000Z' },
    edit(doc) {
      const row = doc.invariants.find((i) => i.name === 'output_bytes');
      row.value += 1;
    },
  });
  const r = runImport(second, history);
  refused(r, /invariant\(s\) moved within one commit/, /output_bytes/);
  assert.equal(readHistory(history).length, 1, 'the refused run must not land');
});

test('an invariant that moved across commits is imported as a finding', () => {
  // The control that stops the test above from being "refuse every change".
  // Across commits a moved invariant is the epic's actual claim changing and
  // the page renders it as a step with the commit that moved it.
  //
  // RED against an importer that refuses any invariant change anywhere, which
  // would make the suite unable to ever publish a second commit.
  const history = emptyHistory();
  assert.equal(runImport(pristine(), history).code, EXIT.OK);

  const second = mutate({
    set: {
      startedAt: '2026-09-14T16:00:00.000Z',
      'provenance.library.commit': 'f'.repeat(40),
      'provenance.library.mainCommit': 'f'.repeat(40),
      'provenance.commit': 'e'.repeat(40),
    },
    edit(doc) {
      const row = doc.invariants.find((i) => i.name === 'output_bytes');
      row.value += 1;
    },
  });
  const r = runImport(second, history);
  assert.equal(r.code, EXIT.OK, r.err);
  assert.equal(readHistory(history).length, 2);
});

test('a debug build is refused', () => {
  // Already refused at the archive door; this is the door it must not get back
  // in through. RED against an importer that reads provenance for the host and
  // not for the toolchain.
  const assertions = runImport(
    mutate({ set: { 'provenance.node.debugAssertions': true } }),
    emptyHistory(),
  );
  refused(assertions, /debugAssertions/);

  const profile = runImport(
    mutate({ set: { 'provenance.node.buildProfile': 'debug' } }),
    emptyHistory(),
  );
  refused(profile, /buildProfile/);
});

test('a dirty tree is refused', () => {
  const library = runImport(
    mutate({ set: { 'provenance.library.dirty': true } }),
    emptyHistory(),
  );
  refused(library, /provenance\.library\.dirty/);

  const harness = runImport(mutate({ set: { 'provenance.dirty': true } }), emptyHistory());
  refused(harness, /provenance\.dirty/);
});

test('a run with no ok cells is refused', () => {
  // An empty reading is a refusal, not a result. RED against an importer that
  // publishes an entry with an empty `samples` array, which the page renders as
  // a gap that looks like a missing run rather than a failed one.
  const r = runImport(
    mutate({
      edit(doc) {
        for (const c of doc.cells) {
          c.outcome = 'failed';
          c.reason = c.reason ?? 'forced for the test';
        }
      },
    }),
    emptyHistory(),
  );
  refused(r, /none of them is `ok`/);
});

test('a run measured mostly under contention is refused', () => {
  // Repetitions do not remove competing work. RED against an importer that
  // reads `confidence` and never `machineLoad`.
  const r = runImport(
    mutate({
      edit(doc) {
        for (const c of doc.cells.slice(0, Math.ceil(doc.cells.length * 0.8))) {
          if (c.machineLoad) {
            c.machineLoad.quiet = false;
            c.machineLoad.loadAvg1m = 12.5;
          }
        }
      },
    }),
    emptyHistory(),
  );
  refused(r, /machineLoad\.quiet: false/);
});

test('an unattested ok cell is refused', () => {
  // Attestation is observed, never asserted, and a cell that claims a
  // measurement without it is a number with no witness. RED against an importer
  // that carries `attested` through to the page and lets the reader decide.
  const r = runImport(
    mutate({
      edit(doc) {
        doc.cells.find((c) => c.outcome === 'ok').storageAttested = false;
      },
    }),
    emptyHistory(),
  );
  refused(r, /without `attested: true`/);
});

test('a cell attested under the new field name imports, and under neither is refused', () => {
  // The field is `attested` on lane/k2.2-engines-document and `storageAttested`
  // in every document archived before it, because widening the cell shape to a
  // second family turned a field name into a family name. The archive keeps old
  // documents forever, so both names have to read.
  //
  // RED against reading one name only, in either direction: every document of
  // the other shape then comes back `undefined`, which reads as "unattested"
  // and refuses the whole archive while looking like a gate doing its job.
  const renamed = runImport(
    mutate({
      edit(doc) {
        for (const c of doc.cells) {
          c.attested = c.storageAttested;
          delete c.storageAttested;
        }
      },
    }),
    emptyHistory(),
  );
  assert.equal(renamed.code, EXIT.OK, renamed.err);

  // And a cell carrying neither name is a shape this importer has not been
  // taught, which is a different fact from a cell that failed attestation.
  const neither = runImport(
    mutate({
      edit(doc) {
        for (const c of doc.cells) delete c.storageAttested;
      },
    }),
    emptyHistory(),
  );
  refused(neither, /none of the attestation fields/, /shape the importer has not been taught/);
});

test('the run id the document states must be the one its evidence derives', () => {
  // Every archived document written so far carries `runId: null`, because the
  // producer declared the field and never populated it (fixed on
  // lane/k2.2-engines-document). So this importer never reads it: the id is
  // derived from `startedAt`, the library commit and the host bucket, and joined
  // to the archive index.
  //
  // RED against keying the entry on `doc.runId`, which today would file every
  // run under `null` and give an idempotency that works because everything
  // collides. Once the producer does write it, the two must agree.
  const r = runImport(
    mutate({ set: { runId: '20260101T000000Z-' + 'a'.repeat(40) + '-12345678' } }),
    emptyHistory(),
  );
  refused(r, /states runId/, /derives/);

  // The control: the archived document states null, the entry is keyed on a real
  // id, and importing it twice still leaves one entry.
  const history = emptyHistory();
  const run = pristine();
  assert.equal(run.doc.runId, null, 'the producer has never populated this field');
  assert.equal(runImport(run, history).code, EXIT.OK);
  assert.equal(runImport(run, history).code, EXIT.OK);
  const entries = readHistory(history);
  assert.equal(entries.length, 1);
  assert.equal(entries[0].runId, ARCHIVED_RUN_ID);
  assert.notEqual(entries[0].runId, null);
});

test('a cell stamped dirty is refused', () => {
  // `--allow-dirty` stamps every cell so a reader quoting one cell knows, and
  // the producer has never written the field, so this path has never been
  // exercised by a real run. It is exercised here. RED against an importer that
  // reads the document header and not the cells.
  const r = runImport(
    mutate({
      edit(doc) {
        doc.cells[0].dirty = true;
      },
    }),
    emptyHistory(),
  );
  refused(r, /stamped `dirty`/);
});

test('a dirty flag that is absent is refused, not read as clean', () => {
  // RED against `dirty === true`, which reads a missing flag as a clean tree.
  // The producer is forbidden `skip_serializing_if` for exactly this reason, and
  // an importer that accepts absence undoes that at the other end.
  refused(runImport(mutate({ remove: ['provenance.dirty'] }), emptyHistory()), /provenance\.dirty/);
  refused(
    runImport(mutate({ remove: ['provenance.library.dirty'] }), emptyHistory()),
    /provenance\.library\.dirty/,
  );
});

test('an outcome the config does not classify is refused, not silently skipped', () => {
  // The producer has five outcomes today and the config buckets all of them. The
  // day it grows a sixth, an importer that treats anything not `ok` as a skip
  // drops every cell that has it and the page shows a gap.
  //
  // RED against a two-way `outcome === 'ok' ? sample : skip`.
  const r = runImport(
    mutate({
      edit(doc) {
        const c = doc.cells.find((x) => x.outcome === 'ok');
        c.outcome = 'degraded';
        c.reason = 'an outcome from a producer this importer has not met';
      },
    }),
    emptyHistory(),
  );
  refused(r, /does not classify/, /degraded/);
});

test('an invariant name the config does not classify is refused', () => {
  // An invariant nobody classified is neither exact nor filesystem-dependent, so
  // it would be carried onto the page and never compared: a claim no refusal can
  // contradict. RED against carrying the invariants array through untouched.
  const r = runImport(
    mutate({
      edit(doc) {
        doc.invariants.push({
          library: 'pmtiles',
          scale: 93,
          source: 'gradient',
          name: 'inode_count',
          value: 1,
          unit: 'count',
        });
      },
    }),
    emptyHistory(),
  );
  refused(r, /not in the config's list/, /inode_count/);
});

test('a measured cell missing a field the config names is refused', () => {
  // Two families feed this page and the second one is being built in another
  // lane. Its document could arrive with a cell shape this reader does not have,
  // and `scenarioOf` would quietly shorten the section name while `direction`
  // came out null, which is a page that is wrong rather than a page that is
  // missing. RED against reading whatever is there and publishing the result.
  for (const field of ['backend', 'key', 'source', 'scale', 'unit', 'direction']) {
    const r = runImport(
      mutate({
        edit(doc) {
          delete doc.cells.find((c) => c.outcome === 'ok')[field];
        },
      }),
      emptyHistory(),
    );
    refused(r, new RegExp(`\\b${field}\\b`), /measured cell/);
  }
});

test('a runner that disagrees with its family is refused', () => {
  // Both are constants the producer stamps, so they say the same thing twice and
  // a disagreement means the document was assembled rather than measured.
  // RED against reading one of the two.
  const r = runImport(mutate({ set: { runner: 'libviprs-engines' } }), emptyHistory());
  refused(r, /does not match family/);
});

test('a document from another family or schema version is refused', () => {
  const family = runImport(mutate({ set: { family: 'libviprs-something' } }), emptyHistory());
  refused(family, /is not one this config knows/);

  const version = runImport(mutate({ set: { schemaVersion: 2 } }), emptyHistory());
  refused(version, /schemaVersion/);
});

test('every reason is reported, never the first', () => {
  // A sweep that was emulated on a dirty tree with a debug build has three
  // problems, and finding them one re-run at a time turns a forty minute sweep
  // into an afternoon. RED against an early return on the first refusal.
  const r = runImport(
    mutate({
      set: {
        'provenance.emulated': true,
        'provenance.dirty': true,
        'provenance.node.debugAssertions': true,
      },
    }),
    emptyHistory(),
  );
  refused(r, /emulat/i, /dirty/i, /debugAssertions/);
});

test('an unknown flag is a usage error and not a silent no op', () => {
  // A typo that turns a gate off quietly is the failure mode this whole
  // directory exists to avoid.
  const run = pristine();
  const r = runImport(run, emptyHistory(), ['--no-really-import-it']);
  assert.equal(r.code, EXIT.USAGE);
});

test('a dry run writes nothing', () => {
  const history = emptyHistory();
  const r = runImport(pristine(), history, ['--dry-run']);
  assert.equal(r.code, EXIT.OK);
  assert.deepEqual(readHistory(history), []);
});

// --- what it carries through rather than drops ------------------------------

test('every sample carries its confidence and the reasons for it', () => {
  // 106 of 332 cells in the first real capture were low confidence for timer
  // saturation, and the page must not present those as firm.
  //
  // RED against an importer that keeps `confidence` and drops
  // `lowConfidenceReasons`, which leaves the page able to grey a cell out but
  // not to say why.
  const entry = importedEntry();
  assert.ok(entry.samples.every((s) => s.confidence === 'high' || s.confidence === 'low'));
  assert.ok(entry.samples.every((s) => Array.isArray(s.lowConfidenceReasons)));

  const saturatedIn = (rows) =>
    rows.filter((s) => s.lowConfidenceReasons.some((r) => /timer saturated/.test(r)));

  // 106 counts every measured cell in the document, and 11 of those are the
  // replicate pass measuring the 93 cell a second time. The page sees 95 of
  // them as samples and the other 11 as the control they were taken for, and
  // nothing is lost between the two.
  assert.equal(saturatedIn([...entry.samples, ...entry.replicates]).length, 106);
  const saturated = saturatedIn(entry.samples);
  assert.equal(saturated.length, 95);
  assert.ok(saturated.every((s) => s.confidence === 'low'));
  assert.ok(saturated.every((s) => s.timerSaturated === true));

  const low = entry.samples.filter((s) => s.confidence === 'low');
  assert.ok(low.every((s) => s.lowConfidenceReasons.length > 0), 'low without a reason is a lie');
  const high = entry.samples.filter((s) => s.confidence === 'high');
  assert.ok(high.every((s) => s.lowConfidenceReasons.length === 0));
});

test('the replicate spread reaches the entry and every sample it was measured for', () => {
  // The page draws bands from it and calls a delta `noise` when the spread
  // covers it. RED against an importer that drops the `replicate` block, which
  // leaves the page with no noise figure at all and every wobble a regression.
  const entry = importedEntry();
  assert.equal(entry.replicate.cell, '2048x2048@256+gradient');
  assert.equal(entry.replicate.replicateReps, 2);
  assert.ok(Object.keys(entry.replicate.spreadPct).length > 40);

  const withSpread = entry.samples.filter((s) => typeof s.replicateSpreadPct === 'number');
  assert.ok(withSpread.length > 0);
  // The worst one in this capture is 53.3% on directory read_concurrent@4
  // throughput, which is the reason a 10% constant would be a fiction.
  const worst = Math.max(...withSpread.map((s) => s.replicateSpreadPct));
  assert.ok(worst > 50, `expected the 53% outlier to survive, saw ${worst}`);

  const one = entry.samples.find(
    (s) => s.library === 'directory' && s.key === 'read_concurrent@4.lookups_per_s',
  );
  assert.equal(
    one.replicateSpreadPct,
    entry.replicate.spreadPct['directory.read_concurrent@4.lookups_per_s'],
  );
  assert.equal(one.replicateSpreadCell, '2048x2048@256+gradient');
});

test('nothing is gated without a fitted baseline and a p99 is never gated', () => {
  // An ungateable metric must never get a verdict chip. There is no calibration
  // yet, so every cell is published as measured and not gated.
  //
  // RED against an importer that writes `gated: true` by default or carries a
  // constant tolerance, which is causl's ten percent and is a guess.
  const entry = importedEntry();
  assert.ok(entry.samples.every((s) => s.gated === false));
  assert.ok(entry.samples.every((s) => s.tolerancePct === null));
  assert.ok(entry.samples.every((s) => typeof s.ungateableReason === 'string'));

  const p99 = entry.samples.filter((s) => s.key.endsWith('.p99'));
  assert.ok(p99.length > 0);
  assert.ok(p99.every((s) => /ungateable/.test(s.ungateableReason)));
});

test('a fitted baseline gates the metrics it fitted and still never a p99', () => {
  // RED against an importer that ignores the baseline, and against one that
  // gates everything the baseline mentions including the metrics the
  // calibration named ungateable.
  const baseline = join(scratch(), 'baseline.json');
  writeFileSync(
    baseline,
    JSON.stringify({
      host8: ARCHIVED_RUN_ID.split('-').pop(),
      fsType: 'unknown',
      cells: {
        'pmtiles/read_random.p50@21851': { tolerancePct: 14.2, gateable: true },
        'pmtiles/read_random.p99@21851': { tolerancePct: 61.0, gateable: false },
      },
    }),
  );

  const history = emptyHistory();
  const r = runImport(pristine(), history, ['--baseline', baseline]);
  assert.equal(r.code, EXIT.OK, r.err);
  const entry = readHistory(history)[0];

  const p50 = entry.samples.find(
    (s) => s.library === 'pmtiles' && s.key === 'read_random.p50' && s.scale === 21851,
  );
  assert.equal(p50.gated, true);
  assert.equal(p50.tolerancePct, 14.2);

  const p99 = entry.samples.find(
    (s) => s.library === 'pmtiles' && s.key === 'read_random.p99' && s.scale === 21851,
  );
  assert.equal(p99.gated, false);
  assert.equal(p99.tolerancePct, null);

  const unfitted = entry.samples.find(
    (s) => s.library === 'directory' && s.key === 'read_random.p50' && s.scale === 21851,
  );
  assert.equal(unfitted.gated, false);
});

test('a baseline fitted on another host or filesystem gates nothing', () => {
  // RED against an importer that reads tolerances out of a baseline without
  // checking it describes this machine.
  const baseline = join(scratch(), 'baseline.json');
  writeFileSync(
    baseline,
    JSON.stringify({
      host8: 'deadbeef',
      fsType: 'unknown',
      cells: { 'pmtiles/read_random.p50@21851': { tolerancePct: 14.2, gateable: true } },
    }),
  );
  const history = emptyHistory();
  const r = runImport(pristine(), history, ['--baseline', baseline]);
  assert.equal(r.code, EXIT.OK, r.err);
  const entry = readHistory(history)[0];
  assert.ok(entry.samples.every((s) => s.gated === false));
});

test('the invariants are carried as an equality table and keep the epics claim', () => {
  // One archive entry against 22127 filesystem entries for the same pyramid,
  // with the two backends' output_bytes within 0.016%. That is the result the
  // PMTiles work was actually for.
  //
  // RED against an importer that folds invariants into `samples` and lets the
  // page chart them, or drops them because they never move.
  const entry = importedEntry();
  assert.equal(entry.invariants.length, 106);

  const at = (library, name) =>
    entry.invariants.find(
      (i) => i.library === library && i.name === name && i.scale === 21851 && i.source === 'gradient',
    );

  assert.equal(at('pmtiles', 'filesystem_entries').value, 1);
  assert.equal(at('directory', 'filesystem_entries').value, 22127);
  assert.equal(at('pmtiles', 'tiles_produced').value, 21851);
  assert.equal(at('directory', 'tiles_produced').value, 21851);

  const p = at('pmtiles', 'output_bytes').value;
  const d = at('directory', 'output_bytes').value;
  const spread = (Math.abs(p - d) / Math.max(p, d)) * 100;
  assert.ok(spread < 0.016, `output_bytes must agree within 0.016%, saw ${spread}`);

  // Exact, so no dispersion fields wander in beside them.
  for (const row of entry.invariants) {
    assert.deepEqual(Object.keys(row).sort(), ['exact', 'library', 'name', 'scale', 'source', 'unit', 'value']);
  }
  assert.equal(at('pmtiles', 'output_bytes').exact, true);
  // allocated_bytes is the filesystem's answer, not the code's, so it is
  // carried and marked inexact rather than compared across hosts.
  const allocated = entry.invariants.find((i) => i.name === 'allocated_bytes');
  assert.equal(allocated.exact, false);
});

test('the modelled columns are carried with their parameters and never as a measurement', () => {
  // RED against an importer that puts a modelled cost into `samples`, where the
  // page would draw it on a measured axis with a band it never had.
  const entry = importedEntry();
  assert.equal(entry.modelled.length, 16);
  assert.ok(entry.modelled.every((m) => m.model && Object.keys(m.model).length > 0));
  assert.ok(entry.samples.every((s) => !/sync_cost|remote_cost/.test(s.scenario)));
});

test('a non ok cell becomes a skip with its reason and does not refuse the run', () => {
  // causl refuses a run with any failed cell because a failed cell there means a
  // runner crashed. In this family `failed` is the honest label for a scenario
  // that does not apply to a backend: a directory tree has no root directory to
  // decode. RED against inheriting causl's rule, which refuses this real run.
  const entry = importedEntry();
  assert.equal(entry.skipped.length, 11);
  assert.ok(entry.skipped.every((s) => typeof s.reason === 'string' && s.reason.length > 0));

  const decode = entry.skipped.filter((s) => s.key === 'decode_root.p50');
  assert.equal(decode.length, 7);
  assert.ok(decode.every((s) => s.library === 'directory'));
  assert.ok(decode.every((s) => s.kind === 'structural'));

  // A cell that refused itself because allocated_bytes moved between reps of one
  // pyramid is a different fact from a scenario that cannot apply, and the page
  // has to be able to tell them apart. RED against folding both into `SKIP`.
  const selfRefused = entry.skipped.filter((s) => s.outcome === 'refused');
  assert.equal(selfRefused.length, 4);
  assert.ok(selfRefused.every((s) => /allocated_bytes differs/.test(s.reason)));
  assert.ok(selfRefused.every((s) => s.library === 'pmtiles'));
});

test('the replicate cell is published once and its second pass is kept beside it', () => {
  // The 93 cell is measured first and last, so it appears twice in the
  // document. RED against an importer that emits both as samples, which draws
  // one run as two points and makes the replicate control look like a
  // regression.
  const entry = importedEntry();
  const keys = entry.samples.map((s) => `${s.library}/${s.scenario}@${s.scale}`);
  assert.equal(new Set(keys).size, keys.length, 'a sample key must identify one sample');

  const replicated = entry.replicates;
  assert.equal(replicated.length, 48);
  assert.ok(replicated.every((s) => s.scale === 93));
  assert.equal(entry.samples.filter((s) => s.scale === 93).length, 48);
  assert.equal(entry.samples.length, 284, '332 ok cells less the 48 the replicate pass repeats');
});

test('a sample keeps its unit and direction and never converts a rate into milliseconds', () => {
  // RED against an importer that writes every median into `medianMs`, which is
  // how a field called milliseconds comes to hold lookups per second.
  const entry = importedEntry();
  const p50 = entry.samples.find((s) => s.key === 'read_random.p50' && s.scale === 21851);
  assert.equal(p50.unit, 'us');
  assert.equal(p50.direction, 'lower-is-better');
  assert.equal(p50.medianMs, p50.median / 1000);
  assert.equal(p50.throughput, null);

  const rate = entry.samples.find(
    (s) => s.key === 'read_random.lookups_per_s' && s.scale === 21851,
  );
  assert.equal(rate.unit, '1/s');
  assert.equal(rate.direction, 'higher-is-better');
  assert.equal(rate.medianMs, null);
  assert.equal(rate.throughput, rate.median);

  const wall = entry.samples.find((s) => s.key === 'generate.wall' && s.scale === 21851);
  assert.equal(wall.unit, 'ms');
  assert.equal(wall.medianMs, wall.median);
});

test('the declared request counts are marked declared and are never gated', () => {
  // The directory backend does not issue requests to anything; its count is
  // declared, one object per tile. RED against a page that renders it beside a
  // measured count with no mark.
  const entry = importedEntry();
  const declared = entry.samples.filter((s) => s.declared === true);
  assert.ok(declared.length > 0);
  assert.ok(declared.every((s) => s.key.endsWith('_declared')));
  assert.ok(declared.every((s) => s.gated === false));
  const measured = entry.samples.filter((s) => s.key === 'requests.requests');
  assert.ok(measured.length > 0);
  assert.ok(measured.every((s) => s.declared === false));
});

test('the source image is part of the section and not folded into the tile count', () => {
  // The tile count is not unique across cells: 21851 is both `8192x8192@64+gradient`
  // and `8192x8192@64+noise` in this one run, and 16369 is two `4096x6256@46`
  // cells. A section keyed on the tile count alone draws two different inputs as
  // one line and nothing on the page says so.
  //
  // RED against an importer that leaves `source` out of the scenario, which is
  // provable here rather than assertable: dropping it collides real sections.
  const entry = importedEntry();
  assert.deepEqual([...new Set(entry.samples.map((s) => s.library))].sort(), [
    'directory',
    'pmtiles',
  ]);
  assert.ok(entry.samples.every((s) => / · (gradient|noise)$/.test(s.scenario)));

  const withSource = new Set(entry.samples.map((s) => `${s.library}/${s.scenario}@${s.scale}`));
  const withoutSource = new Set(entry.samples.map((s) => `${s.library}/${s.key}@${s.scale}`));
  assert.equal(withSource.size, entry.samples.length);
  assert.ok(
    withoutSource.size < entry.samples.length,
    'if dropping the source collided nothing, this test would be proving nothing',
  );
  assert.equal(entry.samples.length - withoutSource.size, 94, '94 real sections would collide');

  // The named case, so the count above cannot drift into meaning something else.
  const at21851 = entry.samples.filter(
    (s) => s.library === 'pmtiles' && s.key === 'read_random.p50' && s.scale === 21851,
  );
  assert.equal(at21851.length, 2);
  assert.deepEqual(at21851.map((x) => x.source).sort(), ['gradient', 'noise']);
  assert.notEqual(at21851[0].scenario, at21851[1].scenario);
  assert.notEqual(at21851[0].median, at21851[1].median);
});

test('the entry carries the era axes the page breaks a line on', () => {
  // A run on another machine or another filesystem starts a new era instead of
  // drawing a line across two experiments. RED against an entry that records
  // the host as prose and leaves the page to parse it.
  const entry = importedEntry();
  // Two fingerprints over two lists. The archive files a run under one and the
  // page breaks an era on the other, and letting either stand in for the other
  // silently changes what counts as the same machine.
  assert.equal(entry.host.archiveBucket, ARCHIVED_RUN_ID.split('-').pop());
  assert.match(entry.host.fingerprint, /^[0-9a-f]{8}$/);
  assert.notEqual(entry.host.fingerprint, entry.host.archiveBucket);
  assert.equal(entry.host.arch, 'aarch64');
  assert.equal(entry.host.os, 'linux');
  assert.equal(entry.host.fsType, 'unknown');
  assert.equal(entry.filesystem.fsType, 'unknown');
  assert.equal(entry.emulated, false);
  assert.ok(Array.isArray(entry.emulationEvidence) && entry.emulationEvidence.length === 4);
  assert.equal(entry.profile, 'full');
  assert.equal(entry.measurement.reps.read, 20);
});

test('a run id the index states but the document does not derive is refused', () => {
  // The id is derived from the run's own evidence, so an index row that names a
  // different one is an index that has been edited. RED against an importer
  // that takes the run id from the index and asks nothing.
  const run = mutate({});
  const index = JSON.parse(readFileSync(join(run.dir, 'index.json'), 'utf8'));
  index[0].runId = '20260101T000000Z-' + 'a'.repeat(40) + '-12345678';
  index[0].file = `${run.runId}.json`;
  writeFileSync(join(run.dir, 'index.json'), JSON.stringify(index, null, 2));
  const r = runImport(run, emptyHistory());
  refused(r, /carries the run id/);
});
