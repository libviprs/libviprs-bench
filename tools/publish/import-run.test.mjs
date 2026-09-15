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
  const result = spawnSync(
    process.execPath,
    [
      IMPORTER,
      '--document', join(dir, `${runId}.json`),
      '--archive', dir,
      '--history', history,
      '--config', CONFIG,
      ...extra,
    ],
    { encoding: 'utf8' },
  );
  return { code: result.status, out: result.stdout ?? '', err: result.stderr ?? '' };
}

function readHistory(path) {
  return JSON.parse(readFileSync(path, 'utf8'));
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
  assert.equal(r.code, EXIT.REFUSED);
  assert.match(r.err, /emulat/i);
});

test('an unknown emulation verdict is refused', () => {
  // RED against `emulated !== true`, which admits every run whose probe could
  // not tell. An unobserved run is not a native one.
  const r = runImport(mutate({ set: { 'provenance.emulated': 'unknown' } }), emptyHistory());
  assert.equal(r.code, EXIT.REFUSED);
  assert.match(r.err, /emulat/i);
});

test('an absent emulation verdict is refused', () => {
  // "absent" is the state the published PMTiles numbers are in. RED against a
  // truthiness check, which reads a missing key as false and admits it.
  const r = runImport(mutate({ remove: ['provenance.emulated'] }), emptyHistory());
  assert.equal(r.code, EXIT.REFUSED);
  assert.match(r.err, /emulat/i);
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
  assert.equal(r.code, EXIT.REFUSED);
  assert.match(r.err, /digest/i);
  assert.match(r.err, /cells/, 'the refusal must name the block that moved');
});

test('a document carrying no integrity block at all is refused', () => {
  const r = runImport(mutate({ remove: ['integrity'], reseal: false }), emptyHistory());
  assert.equal(r.code, EXIT.REFUSED);
  assert.match(r.err, /integrity/i);
});

test('a document that is not archived is refused', () => {
  // A sealed document sitting in a working tree is not citable. RED against an
  // importer that reads the document and never the archive.
  const run = mutate({ index: false });
  const r = runImport(run, emptyHistory());
  assert.equal(r.code, EXIT.REFUSED);
  assert.match(r.err, /archive|index\.json/i);
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
  assert.equal(r.code, EXIT.REFUSED);
  assert.match(r.err, /index/i);
});

test('the ci profile is refused', () => {
  // `ci` proves the harness runs and is never a measurement, and it archives
  // indistinguishably from `full`. RED against an importer that admits any
  // profile, which lets a three-rep smoke run sit in the same era as a
  // calibrated sweep.
  const r = runImport(mutate({ set: { profile: 'ci' } }), emptyHistory());
  assert.equal(r.code, EXIT.REFUSED);
  assert.match(r.err, /profile/i);
  assert.match(r.err, /\bci\b/);
});

test('a profile the document and its invocation disagree about is refused', () => {
  // RED against an importer that reads one of the two. A document that says
  // `full` over an invocation that resolved `ci` is a relabelled smoke run.
  const r = runImport(
    mutate({ set: { 'provenance.invocation.resolved.profile': 'ci' } }),
    emptyHistory(),
  );
  assert.equal(r.code, EXIT.REFUSED);
  assert.match(r.err, /profile/i);
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
  assert.equal(r.code, EXIT.REFUSED);
  assert.match(r.err, /invariant/i);
  assert.match(r.err, /output_bytes/);
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
  assert.equal(assertions.code, EXIT.REFUSED);
  assert.match(assertions.err, /debug/i);

  const profile = runImport(
    mutate({ set: { 'provenance.node.buildProfile': 'debug' } }),
    emptyHistory(),
  );
  assert.equal(profile.code, EXIT.REFUSED);
  assert.match(profile.err, /debug|release/i);
});

test('a dirty tree is refused', () => {
  const library = runImport(
    mutate({ set: { 'provenance.library.dirty': true } }),
    emptyHistory(),
  );
  assert.equal(library.code, EXIT.REFUSED);
  assert.match(library.err, /dirty/i);

  const harness = runImport(mutate({ set: { 'provenance.dirty': true } }), emptyHistory());
  assert.equal(harness.code, EXIT.REFUSED);
  assert.match(harness.err, /dirty/i);
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
  assert.equal(r.code, EXIT.REFUSED);
  assert.match(r.err, /no|none/i);
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
  assert.equal(r.code, EXIT.REFUSED);
  assert.match(r.err, /quiet|contention|load/i);
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
  assert.equal(r.code, EXIT.REFUSED);
  assert.match(r.err, /attest/i);
});

test('a document from another family or schema version is refused', () => {
  const family = runImport(mutate({ set: { family: 'libviprs-something' } }), emptyHistory());
  assert.equal(family.code, EXIT.REFUSED);
  assert.match(family.err, /family/i);

  const version = runImport(mutate({ set: { schemaVersion: 2 } }), emptyHistory());
  assert.equal(version.code, EXIT.REFUSED);
  assert.match(version.err, /schema/i);
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
  assert.equal(r.code, EXIT.REFUSED);
  assert.match(r.err, /emulat/i);
  assert.match(r.err, /dirty/i);
  assert.match(r.err, /debug/i);
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
    (s) => s.library === 'directory' && s.scenario === 'read_concurrent@4.lookups_per_s',
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

  const p99 = entry.samples.filter((s) => s.scenario.endsWith('.p99'));
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
    (s) => s.library === 'pmtiles' && s.scenario === 'read_random.p50' && s.scale === 21851,
  );
  assert.equal(p50.gated, true);
  assert.equal(p50.tolerancePct, 14.2);

  const p99 = entry.samples.find(
    (s) => s.library === 'pmtiles' && s.scenario === 'read_random.p99' && s.scale === 21851,
  );
  assert.equal(p99.gated, false);
  assert.equal(p99.tolerancePct, null);

  const unfitted = entry.samples.find(
    (s) => s.library === 'directory' && s.scenario === 'read_random.p50' && s.scale === 21851,
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

  const decode = entry.skipped.filter((s) => s.scenario === 'decode_root.p50');
  assert.equal(decode.length, 7);
  assert.ok(decode.every((s) => s.library.startsWith('directory')));

  const refused = entry.skipped.filter((s) => s.outcome === 'refused');
  assert.equal(refused.length, 4);
  assert.ok(refused.every((s) => /allocated_bytes differs/.test(s.reason)));
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
  const p50 = entry.samples.find((s) => s.scenario === 'read_random.p50' && s.scale === 21851);
  assert.equal(p50.unit, 'us');
  assert.equal(p50.direction, 'lower-is-better');
  assert.equal(p50.medianMs, p50.median / 1000);
  assert.equal(p50.throughput, null);

  const rate = entry.samples.find(
    (s) => s.scenario === 'read_random.lookups_per_s' && s.scale === 21851,
  );
  assert.equal(rate.unit, '1/s');
  assert.equal(rate.direction, 'higher-is-better');
  assert.equal(rate.medianMs, null);
  assert.equal(rate.throughput, rate.median);

  const wall = entry.samples.find((s) => s.scenario === 'generate.wall' && s.scale === 21851);
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
  assert.ok(declared.every((s) => s.scenario.endsWith('_declared')));
  assert.ok(declared.every((s) => s.gated === false));
  const measured = entry.samples.filter((s) => s.scenario === 'requests.requests');
  assert.ok(measured.length > 0);
  assert.ok(measured.every((s) => s.declared === false));
});

test('the source image is an era of its own and not a second line on one chart', () => {
  // gradient and noise are different inputs, so folding them into one series
  // would draw two experiments as one line. RED against an importer that keys
  // a series on the backend alone.
  const entry = importedEntry();
  const libraries = new Set(entry.samples.map((s) => s.library));
  assert.deepEqual(
    [...libraries].sort(),
    ['directory', 'directory+noise', 'pmtiles', 'pmtiles+noise'],
  );
  const noise = entry.samples.filter((s) => s.library.endsWith('+noise'));
  assert.ok(noise.every((s) => s.sourceImage === 'noise'));
  assert.ok(noise.every((s) => s.backend === s.library.replace('+noise', '')));
});

test('the entry carries the era axes the page breaks a line on', () => {
  // A run on another machine or another filesystem starts a new era instead of
  // drawing a line across two experiments. RED against an entry that records
  // the host as prose and leaves the page to parse it.
  const entry = importedEntry();
  assert.equal(entry.host.fingerprint, ARCHIVED_RUN_ID.split('-').pop());
  assert.equal(entry.host.arch, 'aarch64');
  assert.equal(entry.host.os, 'linux');
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
  assert.equal(r.code, EXIT.REFUSED);
  assert.match(r.err, /run id/i);
});
