#!/usr/bin/env node
// Import one archived causl-bench sweep into history.json.
//
// This is the causl-org half of causl-bench#99. `runners/archive/README.md`
// in that repository opens with:
//
//   "Every published number comes from a run in this directory. Nothing is
//    charted, quoted in an issue, or pushed to `causl-org` from a run that is
//    not archived here."
//
// Every gate that sentence needs exists on the causl-bench side: the
// aggregator refuses a dirty tree, refuses an unattested engine, computes four
// integrity digests, and writes `RUN.md` and `index.json`. None of them are
// reachable from here, because a `combined.json` handed to this script is just
// a file. So this script re-checks the parts that are checkable from the
// artefact itself and refuses the rest:
//
//   1. the run must be ARCHIVED: its four integrity digests must appear,
//      byte for byte, against a `runId` in the archive's `index.json`. A
//      `combined.json` sitting in a working tree is not citable, and this is
//      the check that makes the sentence above true rather than aspirational.
//   2. the tree must have been CLEAN: `dirtyTree` on any cell is a refusal.
//   3. the merge must not have been FORCED: `homogeneity.forced` means the
//      documents disagreed about the machine, the commit or the reps, and a
//      chart drawn across them is one line through two experiments.
//   4. no cell may be `failed`, and
//   5. at least one cell must be `ok`. A sweep that measured nothing exits 0
//      on the causl-bench side by design (its cells are honestly labelled);
//      publishing it as a run would put an empty entry on the chart.
//
// Usage:
//   node pages/benchmarks/import-run.mjs \
//     --combined ../causl-bench/runners/archive/<runId>/combined.json \
//     --archive  ../causl-bench/runners/archive \
//     [--history pages/benchmarks/history.json] \
//     [--dry-run]
//
// Exit codes: 0 imported · 1 refused · 2 usage.

import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));

const EXIT = { OK: 0, REFUSED: 1, USAGE: 2 };

const argv = process.argv.slice(2);
const flag = (name) => {
  const i = argv.indexOf(`--${name}`);
  return i === -1 ? undefined : argv[i + 1];
};
const has = (name) => argv.includes(`--${name}`);

// Unknown flags are a usage error, not a silent no-op. A typo that turns a
// gate off quietly is the failure mode this whole directory exists to avoid.
const KNOWN = new Set(['combined', 'archive', 'history', 'dry-run', 'help']);
const unknown = argv
  .filter((a) => a.startsWith('--'))
  .map((a) => a.slice(2).split('=')[0])
  .filter((a) => !KNOWN.has(a));
if (unknown.length > 0) {
  console.error(`unknown flag(s): ${unknown.map((u) => `--${u}`).join(', ')}`);
  console.error(`known flags: ${[...KNOWN].map((k) => `--${k}`).join(' ')}`);
  process.exit(EXIT.USAGE);
}

if (has('help') || !flag('combined') || !flag('archive')) {
  console.error(
    'usage: node pages/benchmarks/import-run.mjs --combined <combined.json> --archive <dir>\n' +
      '                                            [--history <history.json>] [--dry-run]',
  );
  process.exit(EXIT.USAGE);
}

const combinedPath = resolve(flag('combined'));
const archiveDir = resolve(flag('archive'));
const historyPath = resolve(flag('history') ?? join(here, 'history.json'));

const refusals = [];
const refuse = (why) => refusals.push(why);

if (!existsSync(combinedPath)) {
  console.error(`refused: ${combinedPath} does not exist`);
  process.exit(EXIT.REFUSED);
}

const combined = JSON.parse(readFileSync(combinedPath, 'utf8'));

// --- 1. archived ------------------------------------------------------------
const indexPath = join(archiveDir, 'index.json');
let archivedRun = null;
if (!existsSync(indexPath)) {
  refuse(
    `the archive has no index.json at ${indexPath}, so this run is not citable, and ` +
      'causl-bench/runners/archive/README.md forbids publishing from a run that is not archived',
  );
} else if (!combined.integrity) {
  refuse('combined.json carries no `integrity` block, so there is nothing to match against the archive');
} else {
  const index = JSON.parse(readFileSync(indexPath, 'utf8'));
  const runs = Array.isArray(index.runs) ? index.runs : [];
  const want = JSON.stringify(combined.integrity);
  archivedRun = runs.find((r) => JSON.stringify(r.integrity) === want) ?? null;
  if (!archivedRun) {
    refuse(
      `no run in ${indexPath} carries these integrity digests: the archive holds ` +
        `${runs.length} run(s) and none of them is this one. A combined.json in a working ` +
        'tree is not a citable artefact; re-run the sweep with `--archive runners/archive`.',
    );
  }
}

// --- 2..5. the checks that read the aggregate itself -------------------------
const cells = Array.isArray(combined.cells) ? combined.cells : [];

const dirty = cells.filter((c) => c.dirtyTree === true);
if (dirty.length > 0) {
  const runners = [...new Set(dirty.map((c) => c.runner))];
  refuse(`${dirty.length} cell(s) from ${runners.join(', ')} were measured against a dirty tree`);
}

if (combined.homogeneity?.forced === true) {
  refuse(
    'the merge was FORCED: the runner documents did not agree on the machine, the commit ' +
      'or the reps, so a single chart line would run through more than one experiment ' +
      `(reason given: ${combined.homogeneity.reason ?? 'none'})`,
  );
}

const failed = cells.filter((c) => c.outcome === 'failed');
if (failed.length > 0) {
  refuse(
    `${failed.length} cell(s) are \`failed\`: ` +
      failed
        .slice(0, 5)
        .map((c) => `${c.runner}/${c.scenario}@${c.scale}`)
        .join(', ') + (failed.length > 5 ? ', …' : ''),
  );
}

const okCells = cells.filter((c) => c.outcome === 'ok');
if (okCells.length === 0) {
  refuse(
    `the sweep published ${cells.length} cell(s) and none of them is \`ok\`, so there is no ` +
      'measurement here to chart',
  );
}

// --- 6. the run must actually compare something -----------------------------
//
// A sweep can measure 364 cells, exit 0, archive cleanly, and still rank almost
// nothing: `aggregate.mjs` excludes every low-confidence cell from `ranking`,
// so a group whose cells were all measured on a busy machine collapses to
// fewer than two comparable entries and publishes no comparison at all.
//
// This is not hypothetical. The first full sweep taken for this page ranked
// **1 of 79 groups**: 326 of its 364 ok cells were low-confidence, 273 of them
// because `machineLoad.quiet` was false. Every gate above passed it. A page
// built from that run would have shown a comparison the aggregate had refused
// to make, which is the same shape of defect as the panel this page retracts.
//
// So the publish boundary asks the question none of the others do: did this run
// produce a comparison? Groups the work-equivalence gate held out are NOT
// counted against it (those are correct refusals about scenario semantics, not
// evidence the run was noisy), and neither is a group withheld because a
// suppressed cell beat the leader.
const groups = Array.isArray(combined.ranking) ? combined.ranking : [];
const comparable = groups.filter((g) => !g.exclusionReason && !g.withheld);
const ranked = comparable.filter((g) => (g.entries ?? []).length >= 2);

if (groups.length > 0 && ranked.length === 0) {
  refuse(
    `the run ranks 0 of ${comparable.length} comparable group(s): it measured ` +
      `${okCells.length} cell(s) and compared none of them`,
  );
}

// The contamination check, and the reason it is not a threshold.
//
// "How many groups ranked" is a consequence, and picking a cut-off on it means
// inventing a number. `machineLoad.quiet` is a CAUSE and it is an observation:
// the runner sampled the load average when it measured the cell and recorded
// whether anything else was on the CPU. A run whose cells were mostly taken
// while the machine was busy is contaminated whether or not the ranking
// happened to survive it, and no number of repetitions fixes it: the
// competing work is not part of the workload under test.
//
// A simple majority is the line: past that, the typical cell of the run was
// measured under contention, so the run as a whole describes the machine as
// much as the libraries.
const withLoad = okCells.filter((c) => c.machineLoad && typeof c.machineLoad.quiet === 'boolean');
const noisy = withLoad.filter((c) => c.machineLoad.quiet === false);
if (withLoad.length > 0 && noisy.length > withLoad.length / 2) {
  const loads = noisy.map((c) => c.machineLoad.loadAvg1m).filter(Number.isFinite).sort((a, b) => a - b);
  const median = loads.length ? loads[Math.floor(loads.length / 2)] : null;
  refuse(
    `${noisy.length} of ${withLoad.length} measured cells record \`machineLoad.quiet: false\`, so the ` +
      'typical cell of this run was measured while other work was on the CPU' +
      (median === null ? '' : ` (median load average ${median} across ${noisy.length} cells)`) +
      '. Re-measure on an idle machine; repetitions do not remove competing work.',
  );
}

// Not a refusal (a run may legitimately rank a minority of its groups), but
// silence here is how "we published a sweep" comes to mean less than a reader
// assumes.
if (ranked.length > 0 && ranked.length < comparable.length / 2) {
  console.error(
    `NOTE: this run ranks ${ranked.length} of ${comparable.length} comparable group(s). ` +
      'The rest had fewer than two cells the aggregator was willing to compare.\n',
  );
}

if (refusals.length > 0) {
  console.error('REFUSED. This run may not be published:\n');
  for (const r of refusals) console.error(`  · ${r}`);
  console.error('');
  process.exit(EXIT.REFUSED);
}

// --- the entry ---------------------------------------------------------------

/** Runner directory name → dashboard series id.
 *
 *  These are two different names and conflating them is how the withdrawn data
 *  got here in the first place. A runner id names a directory in `causl-bench`;
 *  a series id names a line on the chart and a `library` value in
 *  `history.json`. They agree for every comparator, so the map holds exactly
 *  one entry.
 *
 *  `causl-wasm-ts` is the runner that measures the `@causl/causl-wasm-ts`
 *  client on the Rust `causl-core-rs` engine. It publishes as `causl-wasm`,
 *  because that is the one wasm series this page has: the earlier `causl-wasm`
 *  and `causl-wasm-all` series were the TypeScript engine under a wasm label
 *  and their rows are deleted from `history.json`. Without this map an import
 *  would silently reintroduce `causl-wasm-ts` as a second, differently-named
 *  line for the same engine, and the chart would split in two again.
 *
 *  Note this does NOT rename the package. `libraries[<series>].package` still
 *  reads `@causl/causl-wasm-ts`, which is what npm installs. */
const RUNNER_TO_SERIES = {
  'causl-wasm-ts': 'causl-wasm',
};
const seriesId = (runner) => RUNNER_TO_SERIES[runner] ?? runner;

/** p95 straight off the recorded per-rep samples: not a model, the 95th
 *  percentile of the numbers the runner actually kept. `null` when the cell
 *  recorded no samples (every non-`ok` outcome). */
function p95(samples) {
  if (!Array.isArray(samples) || samples.length === 0) return null;
  const s = [...samples].sort((a, b) => a - b);
  const idx = Math.min(s.length - 1, Math.ceil(0.95 * s.length) - 1);
  return s[Math.max(0, idx)];
}

/** Cells the aggregator kept out of `ranking`, keyed `runner/scenario@scale`,
 *  each carrying the reason it was held out. #122's engine-build hold-out and
 *  #109's work-equivalence exclusions both land here, and the page must say so
 *  next to the numbers rather than in a footnote. */
const excluded = new Map();
for (const group of combined.ranking ?? []) {
  for (const e of group.excluded ?? []) {
    excluded.set(`${e.runner}/${group.scenario}@${group.scale}`, e.reason ?? e.status ?? 'excluded');
  }
  for (const a of group.absent ?? []) {
    excluded.set(`${a.runner}/${group.scenario}@${group.scale}`, a.excludedBy ?? 'absent');
  }
}

const samples = [];
const skipped = [];

for (const c of cells) {
  const key = `${c.runner}/${c.scenario}@${c.scale}`;
  const heldOut = excluded.get(key) ?? null;

  if (c.outcome === 'ok') {
    samples.push({
      library: seriesId(c.runner),
      scenario: c.scenario,
      scale: c.scale,
      medianMs: c.medianMs,
      p95Ms: p95(c.samples),
      // This harness measures process RSS, not V8 heap. The legacy field is
      // carried as null rather than filled with a different quantity under an
      // old name; `peakRssMb` is the one that was actually measured.
      peakHeapMb: null,
      peakRssMb: c.peakRssMb ?? null,
      throughput: Number.isFinite(c.medianMs) && c.medianMs > 0 ? 1000 / c.medianMs : null,
      efficiency: null,
      resourceCost: null,
      // Everything below is new, and is what makes the row auditable.
      reps: c.reps ?? null,
      cov: c.cov ?? null,
      ci95Ms: c.ci95Ms ?? null,
      confidence: c.confidence ?? null,
      lowConfidenceReasons: c.lowConfidenceReasons ?? [],
      recomputes: c.recomputes ?? null,
      steadyStateReached: c.steadyState?.reached ?? null,
      engine: c.engine ?? null,
      engineSource: c.engineSource ?? null,
      attested: c.engineAttested ?? null,
      rankable: heldOut === null,
      heldOutReason: heldOut,
    });
  } else {
    skipped.push({
      library: seriesId(c.runner),
      scenario: c.scenario,
      scale: c.scale,
      status: 'SKIP',
      outcome: c.outcome,
      reason: c.reason ?? null,
      engine: c.engine ?? null,
      // NOT `attested`. On a measured row `attested: false` means "this number
      // was produced by an engine other than the one it was labelled with",
      // which is the fact that got two whole series deleted from this feed. A
      // cell that produced no number has no timed work to attest, which is a
      // different fact, and giving the two the same field value would let a
      // reader (or a future filter) count skipped cells as withdrawn ones.
      engineAttested: c.engineAttested ?? null,
    });
  }
}

const runners = combined.runners ?? {};
const anyRunner = Object.values(runners)[0] ?? {};

/** Rewrites every `runner` field in a carried-verbatim block to its series id.
 *
 *  `ranking` and `engineFloor` are copied out of the aggregate unchanged in
 *  every other respect, but their `runner` values have to agree with
 *  `samples[].library` or the page joins them against nothing: the renderer
 *  looks a cell up by `${library}/${scenario}@${scale}`, so a ranking that
 *  still said `causl-wasm-ts` while the samples said `causl-wasm` would render
 *  every causl-wasm cell as "no cell". This walks the whole subtree rather
 *  than naming the six arrays that carry a runner today, because the
 *  aggregator has added one before and a missed array fails silently. */
function toSeriesIds(node) {
  if (Array.isArray(node)) return node.map(toSeriesIds);
  if (node === null || typeof node !== 'object') return node;
  const out = {};
  for (const [k, v] of Object.entries(node)) {
    out[k] = k === 'runner' && typeof v === 'string' ? seriesId(v) : toSeriesIds(v);
  }
  return out;
}

const entry = {
  capturedAt: combined.combinedAt,
  version: archivedRun.runId,
  runId: archivedRun.runId,
  source: 'causl-bench',
  commit: archivedRun.commit ?? null,
  integrity: combined.integrity,
  host: {
    os: anyRunner.os ?? null,
    arch: anyRunner.arch ?? null,
    cpuModel: anyRunner.cpuModel ?? null,
    node: anyRunner.node ?? null,
  },
  // Keyed by series id; `package` keeps the real npm name, which for the
  // causl-wasm series is `@causl/causl-wasm-ts` and is NOT the series id.
  libraries: Object.fromEntries(
    Object.entries(runners).map(([id, p]) => [
      seriesId(id),
      {
        package: p.library?.name ?? null,
        version: p.library?.version ?? null,
        engine: p.engine ?? null,
        engineAttested: p.engineAttested ?? null,
      },
    ]),
  ),
  schedule: combined.schedule ?? null,
  // The aggregator's own ranking, carried verbatim apart from the runner→series
  // rename. The page renders the comparison from THIS rather than re-deriving
  // "who was fastest" from the medians, because the aggregator applies the tie
  // band, the work-equivalence exclusions and the engine-build hold-out, and a
  // renderer that sorted medians itself would quietly disagree with all three.
  ranking: toSeriesIds(combined.ranking ?? null),
  engineFloor: toSeriesIds(combined.engineFloor ?? null),
  samples,
  skipped,
};

const history = JSON.parse(readFileSync(historyPath, 'utf8'));
if (!Array.isArray(history)) {
  console.error(`refused: ${historyPath} is not a JSON array`);
  process.exit(EXIT.REFUSED);
}

// Re-importing the same archived run replaces its entry rather than appending a
// second copy: the run id is derived from the run, so this is idempotent.
const before = history.length;
const kept = history.filter((h) => h.runId !== entry.runId);
kept.push(entry);

console.log(`run       ${entry.runId}`);
console.log(`captured  ${entry.capturedAt}`);
console.log(`host      ${entry.host.cpuModel} · ${entry.host.os}/${entry.host.arch} · node ${entry.host.node}`);
console.log(`libraries ${Object.entries(entry.libraries).map(([k, v]) => `${k}=${v.package}@${v.version}`).join(' ')}`);
console.log(`samples   ${samples.length} ok · ${skipped.length} not-ok · ${samples.filter((s) => !s.rankable).length} held out of ranking`);
console.log(`history   ${before} → ${kept.length} entries`);

if (has('dry-run')) {
  console.log('\n--dry-run: nothing written');
  process.exit(EXIT.OK);
}

// One run per line: the whole file is 1.1 MB, so a pretty-printed array turns
// "we added a sweep" into a 50 000-line diff and a single compact line turns it
// into an unreviewable "1 line changed". One compact object per line makes
// adding a run exactly one added line, which is what it is.
writeFileSync(historyPath, `[\n${kept.map((e) => JSON.stringify(e)).join(',\n')}\n]\n`);
console.log(`\nwrote ${historyPath}`);
console.log('now run: node pages/benchmarks/regen-history-sample.mjs');
