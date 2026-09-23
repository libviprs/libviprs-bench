import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { buildStorageCharts, attachWorkCounts } from './storage-charts.mjs';
import { loadContract, CONTRACT_PATH } from './comparability.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '..', '..');
const contract = loadContract(CONTRACT_PATH);

const cell = (over = {}) => ({
  backend: 'directory', scenario: 'read_random', metric: 'p50', unit: 'us',
  direction: 'lower-is-better', cell: '2048x2048@256+gradient', scale: 93,
  source: 'gradient', outcome: 'ok', median: 10, ci95: 0.5, ...over,
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
  assert.ok(charts.length >= 20, `expected a chart per comparable scenario and metric, got ${charts.length}`);
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
