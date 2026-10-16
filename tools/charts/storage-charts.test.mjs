import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { buildStorageCharts, attachWorkCounts, halfWidth, medianOf, unfitToRace } from './storage-charts.mjs';
import { loadContract, CONTRACT_PATH } from './comparability.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '..', '..');
const contract = loadContract(CONTRACT_PATH);

const cell = (over = {}) => ({
  backend: 'directory', scenario: 'read_random', metric: 'p50', unit: 'us',
  direction: 'lower-is-better', cell: '2048x2048@256+gradient', scale: 93,
  // `ci95` is [lo, hi] in the document, never a scalar. The first version of
  // this fixture used 0.5 and the suite agreed with the code while neither
  // agreed with the archive.
  source: 'gradient', outcome: 'ok', median: 10, ci95: [9.5, 10.6], confidence: 'high', ...over,
});
const doc = (cells, invariants = []) => ({ family: 'libviprs-storage', cells, invariants });

test('both backends on a comparable scenario produce a chart', () => {
  const { charts } = buildStorageCharts(doc([
    cell(), cell({ backend: 'pmtiles', median: 7 }),
  ]), { contract });
  assert.equal(charts.length, 1);
  assert.match(charts[0].filename, /^storage_read_random_p50\.svg$/);
  assert.equal((charts[0].svg.match(/class="bc-bar"/g) || []).length, 2);
  assert.ok(charts[0].svg.includes('Directory') && charts[0].svg.includes('PMTiles'));
});

test('the unit and direction reach the chart, so a shorter bar is not guessed at', () => {
  const { charts } = buildStorageCharts(doc([cell(), cell({ backend: 'pmtiles', median: 7 })]), { contract });
  assert.ok(charts[0].svg.includes('lower is better'));
  assert.ok(charts[0].svg.includes('us'));
});

test('a higher-is-better metric says so instead', () => {
  const { charts } = buildStorageCharts(doc([
    cell({ metric: 'lookups_per_s', unit: '1/s', direction: 'higher-is-better', median: 100 }),
    cell({ metric: 'lookups_per_s', unit: '1/s', direction: 'higher-is-better', backend: 'pmtiles', median: 140 }),
  ]), { contract });
  assert.ok(charts[0].svg.includes('higher is better'));
});

test('a single-backend scenario is held out, not drawn as a race', () => {
  const { charts, held } = buildStorageCharts(doc([
    cell({ scenario: 'decode_root', metric: 'p50', backend: 'pmtiles' }),
  ]), { contract });
  assert.equal(charts.length, 0);
  assert.ok(held.some((h) => h.scenario === 'decode_root'), JSON.stringify(held));
});

test('requests is held out, because it charts a prediction against an observation', () => {
  const { charts, held } = buildStorageCharts(doc([
    cell({ scenario: 'requests', metric: 'requests_declared', median: 94 }),
    cell({ scenario: 'requests', metric: 'requests', backend: 'pmtiles', median: 96 }),
  ]), { contract });
  assert.equal(charts.length, 0);
  assert.ok(held.some((h) => h.scenario === 'requests'));
});

test('a cell one backend never ran keeps its slot and reads n/a', () => {
  const { charts } = buildStorageCharts(doc([
    cell({ cell: 'a' }), cell({ cell: 'a', backend: 'pmtiles', median: 7 }),
    cell({ cell: 'b' }),
  ]), { contract });
  assert.equal((charts[0].svg.match(/class="bc-bar"/g) || []).length, 3);
  assert.equal((charts[0].svg.match(/>n\/a</g) || []).length, 1, 'pmtiles never ran cell b');
});

test('a failed or refused cell is not charted as a measurement', () => {
  const { charts } = buildStorageCharts(doc([
    cell(), cell({ backend: 'pmtiles', median: 7 }),
    cell({ cell: 'b', outcome: 'failed', median: 0 }),
    cell({ cell: 'b', backend: 'pmtiles', outcome: 'refused', median: 0 }),
  ]), { contract });
  // Not `>0<`: the y axis carries a zero tick by construction, so that matches
  // the axis rather than a bar. The value labels are the assertion.
  const values = [...charts[0].svg.matchAll(/class="bc-value"[^>]*>([^<]+)</g)].map((m) => m[1]);
  assert.deepEqual(values.sort(), ['10', '7'].sort(), 'only the two measured cells are labelled');
  assert.equal((charts[0].svg.match(/class="bc-bar"/g) || []).length, 2,
    'the failed and refused cells produce no bar at all');
});

test('an undeclared scenario is refused rather than charted', () => {
  assert.throws(() => buildStorageCharts(doc([
    cell({ scenario: 'brand_new' }), cell({ scenario: 'brand_new', backend: 'pmtiles' }),
  ]), { contract }), (e) => {
    assert.match(e.message, /brand_new/);
    return true;
  });
});

test('work counts are attached from the invariants, so the census is real', () => {
  const cells = [cell(), cell({ backend: 'pmtiles', median: 7 })];
  const invariants = [
    { library: 'directory', scale: 93, source: 'gradient', name: 'tiles_produced', value: 93 },
    { library: 'pmtiles', scale: 93, source: 'gradient', name: 'tiles_produced', value: 93 },
  ];
  const attached = attachWorkCounts(doc(cells, invariants));
  assert.equal(attached.cells[0].tiles_produced, 93);
  assert.equal(attached.cells[1].tiles_produced, 93);
});

test('a work asymmetry over the tolerance stops the chart', () => {
  const invariants = [
    { library: 'directory', scale: 93, source: 'gradient', name: 'tiles_produced', value: 93 },
    { library: 'pmtiles', scale: 93, source: 'gradient', name: 'tiles_produced', value: 93 * 5 },
  ];
  const { charts, violations } = buildStorageCharts(
    doc([cell(), cell({ backend: 'pmtiles', median: 7 })], invariants), { contract });
  assert.ok(violations.some((v) => v.rule === 'declared-work-is-within-tolerance'), JSON.stringify(violations));
  assert.equal(charts.length, 0);
});

/* Against the archive actually in the repo. */
test('the archived storage run charts its comparable scenarios and holds out the rest', () => {
  const dir = join(repo, 'archive', 'storage');
  const file = readdirSync(dir).filter((f) => f.endsWith('.json') && f !== 'index.json').sort().pop();
  const document = JSON.parse(readFileSync(join(dir, file), 'utf8'));
  const { charts, held, violations } = buildStorageCharts(document, { contract });

  assert.deepEqual(violations, [], violations.map((v) => v.message).join('; '));
  // Fewer than the 24 the first version drew, and that is the fix working: a
  // (scenario, metric) pair whose every row is timer-saturated or
  // oversubscribed now has nothing left to race and is held out.
  assert.ok(charts.length >= 15, `expected a chart per raceable scenario and metric, got ${charts.length}`);
  const unfit = held.filter((h) => h.status === 'unfit-to-race');
  assert.ok(unfit.length > 0, 'and the rows the harness does not stand behind are named');
  for (const c of charts) {
    assert.ok(c.svg.startsWith('<svg') && c.svg.endsWith('</svg>'), `${c.filename} is not an SVG`);
    assert.ok(!/NaN/.test(c.svg), `${c.filename} leaks NaN`);
    assert.ok(c.svg.length <= 24_000, `${c.filename} is ${c.svg.length} bytes`);
  }
  const names = charts.map((c) => c.filename);
  assert.ok(names.some((n) => n.includes('generate')), 'the pyramid build into each backend is charted');
  assert.ok(names.some((n) => n.includes('read_random')), 'and the reads over it');
  assert.ok(!names.some((n) => n.includes('decode_root')), 'decode_root is pmtiles only');
  assert.ok(!names.some((n) => n.includes('requests')), 'requests compares a prediction to an observation');
  assert.ok(held.some((h) => h.scenario === 'decode_root') && held.some((h) => h.scenario === 'requests'));
});


/* The three defects this file did not catch the first time. Each one is here
 * with the real shape the archive carries, because a fixture that is simpler
 * than the data is how all three got through. */

test('ci95 is [lo, hi] in the document, and the whisker covers it', () => {
  assert.equal(halfWidth(0.5, 10), 0.5, 'a scalar still works for anyone who sends one');
  // Not an equality: 10.6 - 10 is 0.6000000000000005 in binary floating point,
  // and pinning that literal tests the representation rather than the rule.
  assert.ok(Math.abs(halfWidth([9.5, 10.6], 10) - 0.6) < 1e-9,
    'the longer arm, so the whisker is never short');
  assert.equal(halfWidth(undefined, 10), null);
  assert.equal(halfWidth([Number.NaN, 2], 1), null);
  assert.equal(halfWidth([10, 10], 10), null, 'a zero-width interval draws no whisker');
});

test('a chart with an error label actually draws whiskers', () => {
  const { charts } = buildStorageCharts(doc([cell(), cell({ backend: 'pmtiles', median: 7, ci95: [6.6, 7.5] })]), { contract });
  assert.ok(charts[0].svg.includes('class="bc-whisker"'),
    'the label said 95% CI while Number.isFinite([lo,hi]) was false and no whisker was ever drawn');
});

test('repeated placements of one cell are aggregated, not overwritten', () => {
  const placements = [89.58, 91.5, 104.48, 127.67, 109.05, 106.98];
  const cells = placements.map((m) => cell({ backend: 'pmtiles', median: m, ci95: [m - 1, m + 1] }));
  const { charts } = buildStorageCharts(doc([...cells, cell({ median: 8 })]), { contract });
  const labels = [...charts[0].svg.matchAll(/class="bc-value"[^>]*>([^<]+)</g)].map((m) => Number(m[1]));
  // The label is formatted, and a value at or above 100 renders as an integer,
  // so the assertion is against the drawn form of the median rather than the
  // median itself.
  const drawnMedian = Math.round(medianOf(placements));
  assert.ok(labels.includes(drawnMedian),
    `expected the median ${medianOf(placements)} of six placements to draw as ${drawnMedian}, got ${labels.join(', ')}`);
  assert.ok(!labels.includes(Math.round(placements[placements.length - 1])),
    'the last placement must not win by iteration order');
});

test('the spread across placements becomes the whisker, being wider than any one interval', () => {
  const placements = [89.58, 127.67];
  const cells = placements.map((m) => cell({ backend: 'pmtiles', median: m, ci95: [m - 0.1, m + 0.1] }));
  const { charts } = buildStorageCharts(doc([...cells, cell({ median: 8 })]), { contract });
  assert.ok(charts[0].svg.includes('class="bc-whisker"'));
  assert.ok(charts[0].svg.includes('placement spread'), 'and the label says which it is');
});

test('a row the harness does not stand behind is not raced', () => {
  assert.deepEqual(unfitToRace({ confidence: 'high' }), []);
  assert.deepEqual(unfitToRace({ timerSaturated: true }), ['timer saturated']);
  assert.deepEqual(unfitToRace({ oversubscribed: true }), ['oversubscribed']);
  assert.ok(unfitToRace({ confidence: 'low', lowConfidenceReasons: ['cov 0.47'] })[0].includes('cov 0.47'));
});

test('a timer-saturated measurement is held out rather than charted as a latency', () => {
  const { charts, held } = buildStorageCharts(doc([
    cell({ median: 8 }),
    cell({ backend: 'pmtiles', median: 0.11, timerSaturated: true, confidence: 'low', lowConfidenceReasons: ['timer saturated'] }),
  ]), { contract });
  const drawn = charts.length ? (charts[0].svg.match(/class="bc-bar"/g) || []).length : 0;
  assert.equal(drawn, 1, 'only the row that clears the clock floor is drawn');
  assert.ok(held.some((h) => h.status === 'unfit-to-race'), JSON.stringify(held));
});

test('the archived run has rows of every unfit kind, so this is not a theoretical guard', () => {
  const dir = join(repo, 'archive', 'storage');
  const file = readdirSync(dir).filter((f) => f.endsWith('.json') && f !== 'index.json').sort().pop();
  const cells = JSON.parse(readFileSync(join(dir, file), 'utf8')).cells.filter((c) => c.outcome === 'ok');
  assert.ok(cells.some((c) => c.timerSaturated), 'the archive carries timer-saturated rows');
  assert.ok(cells.some((c) => c.oversubscribed), 'and oversubscribed ones');
  assert.ok(cells.some((c) => c.confidence === 'low'), 'and low-confidence ones');
  assert.ok(cells.every((c) => !Number.isFinite(c.ci95)), 'and ci95 is never a scalar');
});
