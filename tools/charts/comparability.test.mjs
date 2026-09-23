import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { loadContract, assessComparison, chartableRuns } from './comparability.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '..', '..');

const CONTRACT = {
  comparabilityTolerance: 2,
  equivalenceDefaults: { psnrFloorDb: 40, referenceTechnology: 'libvips' },
  scenarios: {
    pyramid: {
      status: 'comparable',
      work: ['tiles_produced', 'levels_processed'],
      equivalence: { kind: 'psnr', floorDb: 40 },
    },
  },
};

test('a comparable scenario must say what evidence it rests on', () => {
  assert.throws(() => loadContract({
    ...CONTRACT, scenarios: { pyramid: { status: 'comparable', work: ['tiles_produced'] } },
  }), /equivalence/i);
  assert.throws(() => loadContract({
    ...CONTRACT,
    scenarios: { pyramid: { status: 'comparable', work: ['tiles_produced'], equivalence: { kind: 'not-available' } } },
  }), /reason/i, 'not-available without a reason is a loophole, not a declaration');
});

test('a comparable scenario must name the work it is checked on', () => {
  assert.throws(() => loadContract({
    ...CONTRACT,
    scenarios: { pyramid: { status: 'comparable', equivalence: { kind: 'psnr' } } },
  }), /work/i);
});
const run = (engine, over = {}) => ({
  engine, width: 1024, height: 1024, scenario: 'pyramid',
  tiles_produced: 29, levels_processed: 11, tiles_skipped: 0,
  equivalence_psnr_db: engine === 'libvips' ? null : 100, ...over,
});
const FOUR = ['libvips', 'monolithic', 'streaming', 'mapreduce'].map((e) => run(e));

test('a scenario nobody declared fails, because there is no default', () => {
  const runs = [run('libvips', { scenario: 'undeclared' }), run('streaming', { scenario: 'undeclared' })];
  const { violations } = assessComparison(runs, CONTRACT);
  assert.ok(violations.some((v) => v.rule === 'every-scenario-is-declared'),
    `got ${violations.map((v) => v.rule).join(', ') || 'none'}`);
});

test('silence is not a status: an empty declaration is refused at load', () => {
  assert.throws(() => loadContract({ ...CONTRACT, scenarios: { pyramid: {} } }), /status/i);
  assert.throws(() => loadContract({ ...CONTRACT, scenarios: { pyramid: { status: 'probably' } } }), /status/i);
});

test('non-comparable and unknown each need a reason, and they are not synonyms', () => {
  assert.throws(() => loadContract({ ...CONTRACT, scenarios: { pyramid: { status: 'non-comparable' } } }), /reason/i);
  assert.throws(() => loadContract({ ...CONTRACT, scenarios: { pyramid: { status: 'unknown' } } }), /reason/i);
  const ok = loadContract({
    ...CONTRACT,
    scenarios: {
      a: { status: 'non-comparable', reason: 'different units of work under one id' },
      b: { status: 'unknown', reason: 'nobody has established the intended count' },
    },
  });
  assert.equal(ok.scenarios.a.status, 'non-comparable');
  assert.notEqual(ok.scenarios.a.status, ok.scenarios.b.status);
});

test('matching work and passing equivalence makes a cell chartable', () => {
  const { cells, violations } = assessComparison(FOUR, CONTRACT);
  assert.deepEqual(violations, []);
  assert.equal(cells.length, 1);
  assert.equal(cells[0].chartable, true);
  assert.deepEqual(cells[0].technologies.sort(), ['libvips', 'mapreduce', 'monolithic', 'streaming']);
});

test('work differing by more than the tolerance refuses comparable outright', () => {
  const skewed = [run('libvips'), run('streaming', { tiles_produced: 29 * 5 })];
  const { cells, violations } = assessComparison(skewed, CONTRACT);
  assert.ok(violations.some((v) => v.rule === 'declared-work-is-within-tolerance'));
  assert.equal(cells[0].chartable, false);
  assert.match(violations.find((v) => v.rule === 'declared-work-is-within-tolerance').message, /5x|5\.0x/);
});

test('a wall-clock ordering across an asymmetry is a restatement of it, so the ratio is named', () => {
  const skewed = [run('libvips'), run('streaming', { levels_processed: 33 })];
  const { violations } = assessComparison(skewed, CONTRACT);
  const v = violations.find((x) => x.rule === 'declared-work-is-within-tolerance');
  assert.ok(v, 'levels_processed is declared work too');
  assert.ok(v.message.includes('levels_processed'));
});

test('a comparable cell with no equivalence evidence is refused, not charted', () => {
  const noPsnr = FOUR.map((r) => ({ ...r, equivalence_psnr_db: null }));
  const { cells, violations } = assessComparison(noPsnr, CONTRACT);
  assert.ok(violations.some((v) => v.rule === 'comparable-cells-prove-equivalence'));
  assert.equal(cells[0].chartable, false);
});

test('the reference technology is exempt from proving equivalence against itself', () => {
  const { violations } = assessComparison(FOUR, CONTRACT);
  assert.deepEqual(violations, [], 'libvips carries a null psnr by definition and must not trip the rule');
});

test('equivalence below the floor is refused', () => {
  const poor = FOUR.map((r) => (r.engine === 'streaming' ? { ...r, equivalence_psnr_db: 12 } : r));
  const { violations } = assessComparison(poor, CONTRACT);
  assert.ok(violations.some((v) => v.rule === 'comparable-cells-prove-equivalence'));
});

test('a non-comparable cell is held out of the chart and still censused', () => {
  const contract = loadContract({
    ...CONTRACT,
    scenarios: { pyramid: { status: 'non-comparable', reason: 'the engines tile differently here', work: ['tiles_produced'] } },
  });
  const skewed = [run('libvips'), run('streaming', { tiles_produced: 29 * 5 })];
  const { cells, violations } = assessComparison(skewed, contract);
  assert.equal(cells[0].chartable, false, 'held out of every cross-technology comparison');
  assert.ok(violations.some((v) => v.rule === 'held-out-cells-keep-their-census'),
    'the count is the evidence for the reason it is held out, so it keeps being checked');
  assert.ok(!violations.some((v) => v.rule === 'declared-work-is-within-tolerance'),
    'that rule refuses comparable; this cell is already held out and is checked under its own name');
});

test('an unknown cell is held out without inventing a number for it', () => {
  const contract = loadContract({
    ...CONTRACT,
    scenarios: { pyramid: { status: 'unknown', reason: 'nobody has established what this should count' } },
  });
  const { cells } = assessComparison(FOUR, contract);
  assert.equal(cells[0].chartable, false);
  assert.equal(cells[0].status, 'unknown');
});

test('chartableRuns drops exactly the held-out cells and keeps the rest', () => {
  const runs = [...FOUR, ...FOUR.map((r) => ({ ...r, width: 512, height: 512, equivalence_psnr_db: null }))];
  const assessment = assessComparison(runs, CONTRACT);
  const kept = chartableRuns(runs, assessment);
  assert.equal(kept.length, 4, 'the 512x512 cell has no equivalence evidence and goes');
  assert.ok(kept.every((r) => r.width === 1024));
});

test('a per-cell override holds out one cell while the scenario stays comparable', () => {
  const contract = loadContract({
    ...CONTRACT,
    scenarios: {
      pyramid: {
        ...CONTRACT.scenarios.pyramid,
        cells: { '512x512': { status: 'unknown', reason: 'nobody measured the output equivalence here' } },
      },
    },
  });
  const runs = [...FOUR, ...FOUR.map((r) => ({ ...r, width: 512, height: 512, equivalence_psnr_db: null }))];
  const { cells, violations } = assessComparison(runs, contract);
  assert.deepEqual(violations, [], 'a declared hold-out is not a violation');
  assert.equal(cells.find((c) => c.config === '1024x1024').chartable, true);
  assert.equal(cells.find((c) => c.config === '512x512').chartable, false);
  assert.equal(cells.find((c) => c.config === '512x512').status, 'unknown');
});

test('a per-cell override needs a reason, exactly like a scenario one', () => {
  assert.throws(() => loadContract({
    ...CONTRACT,
    scenarios: { pyramid: { ...CONTRACT.scenarios.pyramid, cells: { '512x512': { status: 'unknown' } } } },
  }), /reason/i);
  assert.throws(() => loadContract({
    ...CONTRACT,
    scenarios: { pyramid: { ...CONTRACT.scenarios.pyramid, cells: { '512x512': { status: 'maybe' } } } },
  }), /status/i);
});

/* The gate was made green by declaring the gap it found, so the control that
 * matters is that it can still go red. A gate nobody has seen fail is not a
 * gate. */
test('an UNDECLARED cell with missing evidence still fails, so the gate is not silenced', () => {
  const contract = loadContract(JSON.parse(readFileSync(join(here, 'comparability.json'), 'utf8')));
  const fresh = FOUR.map((r) => ({ ...r, width: 8192, height: 8192, equivalence_psnr_db: null }));
  const { cells, violations } = assessComparison(fresh, contract, { scenario: 'pyramid' });
  assert.ok(violations.some((v) => v.rule === 'comparable-cells-prove-equivalence'),
    'a new config with no PSNR must still be refused');
  assert.equal(cells[0].chartable, false);
});

/* The point of the gate, against the data actually in the repo. */
test('the checked-in comparison report passes, with its known gap declared', () => {
  const runs = JSON.parse(readFileSync(join(repo, 'report', 'benchmark_results.json'), 'utf8'));
  const contract = loadContract(JSON.parse(readFileSync(join(here, 'comparability.json'), 'utf8')));
  const { cells, violations } = assessComparison(runs, contract, { scenario: 'pyramid' });
  assert.ok(cells.length >= 4, `expected a cell per config, got ${cells.length}`);
  for (const cell of cells) {
    assert.ok(cell.work.ratio <= contract.comparabilityTolerance,
      `${cell.config} work ratio ${cell.work.ratio}`);
  }
  assert.deepEqual(violations, [], 'every hold-out is declared, so the gate is green');
  const gaps = cells.filter((c) => c.config.startsWith('512x512'));
  assert.equal(gaps.length, 2, 'both 512x512 cells are present');
  for (const gap of gaps) {
    assert.equal(gap.chartable, false, 'and neither is raced against libvips');
    assert.equal(gap.status, 'unknown', 'held out as unknown, which is a to-do with a name on it (#108)');
  }
  assert.equal(cells.filter((c) => c.chartable).length, 6);
});
