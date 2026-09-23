import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { releasesOf, seriesForMetric, buildReleaseCharts } from './release-history.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '..', '..');

const report = (version, commit, startedAt, cells) => ({
  family: 'libviprs-engines',
  startedAt,
  provenance: { library: { name: 'libviprs', version, commit } },
  cells,
});
const cell = (backend, over = {}) => ({
  backend, scenario: 'pyramid', metric: 'wall', unit: 'ms', direction: 'lower-is-better',
  cell: '512x360@256+c1', scale: 13, source: 'gradient', outcome: 'ok', median: 100, ci95: 2, ...over,
});

const THREE = [
  report('0.4.0', 'aaa', '2026-07-01T00:00:00Z', [cell('streaming', { median: 120 }), cell('mapreduce', { median: 140 })]),
  report('0.5.0', 'bbb', '2026-08-01T00:00:00Z', [cell('streaming', { median: 100 }), cell('mapreduce', { median: 150 })]),
  report('0.6.0', 'ccc', '2026-09-01T00:00:00Z', [cell('streaming', { median: 90 }), cell('mapreduce', { median: 155 })]),
];

test('releases come back in chronological order, one entry per version', () => {
  const releases = releasesOf(THREE);
  assert.deepEqual(releases.map((r) => r.version), ['0.4.0', '0.5.0', '0.6.0']);
  assert.deepEqual(releases.map((r) => r.step), [0, 1, 2]);
});

test('two runs of one version collapse to the most recent, deterministically', () => {
  const twice = [...THREE, report('0.6.0', 'ddd', '2026-09-20T00:00:00Z', [cell('streaming', { median: 80 })])];
  const releases = releasesOf(twice);
  assert.equal(releases.length, 3, 'still three releases');
  const latest = releases.find((r) => r.version === '0.6.0');
  assert.equal(latest.commit, 'ddd', 'the later run wins');
  assert.equal(releasesOf(twice.slice().reverse()).find((r) => r.version === '0.6.0').commit, 'ddd',
    'and input order does not change that');
});

test('a series carries one point per release per backend, labelled with the version', () => {
  const { points } = seriesForMetric(THREE, { scenario: 'pyramid', metric: 'wall' });
  assert.equal(points.length, 6);
  const streaming = points.filter((p) => p.series === 'streaming').sort((a, b) => a.step - b.step);
  assert.deepEqual(streaming.map((p) => p.value), [120, 100, 90]);
  assert.deepEqual(streaming.map((p) => p.label), ['0.4.0', '0.5.0', '0.6.0']);
});

test('the chosen cell is the same across releases, so the line is like-for-like', () => {
  const mixed = [
    report('0.4.0', 'a', '2026-07-01T00:00:00Z', [cell('streaming', { cell: 'small', scale: 1, median: 10 }), cell('streaming', { cell: 'big', scale: 99, median: 500 })]),
    report('0.5.0', 'b', '2026-08-01T00:00:00Z', [cell('streaming', { cell: 'big', scale: 99, median: 480 })]),
  ];
  const { points, cell: chosen } = seriesForMetric(mixed, { scenario: 'pyramid', metric: 'wall' });
  assert.equal(chosen, 'big', 'the cell present in every release wins over a larger one that is not');
  assert.deepEqual(points.map((p) => p.value), [500, 480]);
});

test('a release missing the cell leaves a gap rather than a zero', () => {
  const holed = [
    THREE[0],
    report('0.5.0', 'b', '2026-08-01T00:00:00Z', [cell('mapreduce', { median: 150 })]),
    THREE[2],
  ];
  const { points } = seriesForMetric(holed, { scenario: 'pyramid', metric: 'wall' });
  const streaming = points.filter((p) => p.series === 'streaming').sort((a, b) => a.step - b.step);
  assert.equal(streaming.length, 3, 'the step keeps its slot');
  assert.ok(Number.isNaN(streaming[1].value), 'and the missing release is NaN, never 0');
});

test('a cell that failed or was refused does not become a measurement', () => {
  const bad = [
    THREE[0],
    report('0.5.0', 'b', '2026-08-01T00:00:00Z', [cell('streaming', { outcome: 'failed', median: 0 })]),
    THREE[2],
  ];
  const { points } = seriesForMetric(bad, { scenario: 'pyramid', metric: 'wall' });
  const streaming = points.filter((p) => p.series === 'streaming').sort((a, b) => a.step - b.step);
  assert.ok(Number.isNaN(streaming[1].value), 'a failed cell is absent, not a zero');
});

test('three releases render a trend, and the direction is stated', () => {
  const { charts } = buildReleaseCharts(THREE);
  assert.ok(charts.length > 0);
  const wall = charts.find((c) => c.filename.includes('wall'));
  assert.ok(wall, charts.map((c) => c.filename).join(', '));
  assert.ok(wall.svg.startsWith('<svg') && wall.svg.endsWith('</svg>'));
  assert.ok(/lower is better/.test(wall.svg), 'so a downward line reads unambiguously as an improvement');
  assert.ok(wall.svg.includes('0.4.0') && wall.svg.includes('0.6.0'));
});

/* One release is not a trend, and drawing it as one would be the worst outcome:
 * a chart that looks like a measurement of change and is not. */
test('fewer than two releases refuses to draw, and says why', () => {
  const { charts, skipped } = buildReleaseCharts([THREE[1]]);
  assert.deepEqual(charts, []);
  assert.ok(skipped.some((s) => /one release|at least two|fewer than two/i.test(s.reason)),
    skipped.map((s) => s.reason).join('; '));
});

test('the real archive is read, and today it cannot draw a release trend yet', () => {
  const reports = loadArchive();
  assert.ok(reports.length >= 4, `expected archived reports, got ${reports.length}`);
  const versions = new Set(reports.map((r) => r.provenance.library.version));
  assert.equal(versions.size, 1,
    `the archive holds ${[...versions].join(', ')}; when a second release lands this assertion is the thing to update`);
  const { charts, skipped } = buildReleaseCharts(reports.filter((r) => r.family === 'libviprs-engines'));
  assert.deepEqual(charts, [], 'one release must not be drawn as a trend');
  assert.ok(skipped.length > 0, 'and the run says so rather than writing nothing quietly');
});

function loadArchive() {
  const out = [];
  for (const family of ['engines', 'storage']) {
    const dir = join(repo, 'archive', family);
    for (const f of readdirSync(dir)) {
      if (!f.endsWith('.json') || f === 'index.json') continue;
      out.push(JSON.parse(readFileSync(join(dir, f), 'utf8')));
    }
  }
  return out;
}
