// TDD spec for the Rust↔JS field-shape drift guard (#44).
//
// The three committed golden JSON fixtures mirror the EXACT serde shape the
// Rust serializers emit — BenchmarkSnapshot / RunMetrics / ScalabilityPoint
// field names, including the nested Duration `{ secs, nanos }` and the
// RunStats / Provenance sub-objects. This spec pins two things:
//
//   * round-trip: the golden JSON renders the full, expected SVG set through
//     render.mjs (history trend + grouped-bar comparison + scalability), so a
//     field render.mjs reads that the producer stops emitting is caught by a
//     missing chart, not a silently-wrong one;
//   * drift guard: `resultsShapeOk` (the RunMetrics consumer probe) returns
//     true on the golden and FAILS LOUD (false) the moment a load-bearing
//     field — top-level or nested inside the Duration — is renamed/removed.
//
// The Rust side of the same contract (the golden field names match what the
// serializers actually emit) is asserted in tests/chart_shape_drift.rs and the
// scalability binary's own unit test.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, mkdtempSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname, basename } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  renderAll,
  buildComparisonCharts,
  resultsShapeOk,
  historyShapeOk,
  scalabilityShapeOk,
} from './render.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const FIX = join(HERE, 'fixtures');
const G_HISTORY = join(FIX, 'golden_history.json');
const G_RESULTS = join(FIX, 'golden_results.json');
const G_SCAL = join(FIX, 'golden_scalability.json');

// The complete SVG set the golden JSON must produce:
//   2 history-trend + 5 grouped-bar + 10 scalability (5 metrics × c1/c4).
const EXPECTED = [
  'chart_history_2048x2048_c0_time.svg',
  'chart_history_2048x2048_c0_memory.svg',
  'chart_wall_time.svg',
  'chart_peak_memory.svg',
  'chart_throughput.svg',
  'chart_efficiency.svg',
  'chart_resource_cost.svg',
  'scalability_wall_time_c1.svg',
  'scalability_peak_memory_c1.svg',
  'scalability_throughput_c1.svg',
  'scalability_efficiency_c1.svg',
  'scalability_resource_cost_c1.svg',
  'scalability_wall_time_c4.svg',
  'scalability_peak_memory_c4.svg',
  'scalability_throughput_c4.svg',
  'scalability_efficiency_c4.svg',
  'scalability_resource_cost_c4.svg',
];

function freshOut() {
  return mkdtempSync(join(tmpdir(), 'libviprs-golden-'));
}

function loadResults() {
  return JSON.parse(readFileSync(G_RESULTS, 'utf8'));
}

test('the golden JSON renders the full expected SVG set (round-trip)', () => {
  const outDir = freshOut();
  const written = renderAll({
    historyPath: G_HISTORY,
    resultsPath: G_RESULTS,
    scalabilityPath: G_SCAL,
    outDir,
  });
  const names = written.map((p) => basename(p)).sort();
  assert.deepEqual(names, [...EXPECTED].sort(), 'exactly the expected SVG set, nothing missing/spurious');
  for (const want of EXPECTED) {
    const svg = readFileSync(join(outDir, want), 'utf8');
    assert.match(svg, /^<svg/);
    assert.match(svg, /<\/svg>$/);
    assert.ok(!svg.includes('NaN') && !svg.includes('undefined'), `${want} leaks no NaN/undefined`);
  }
});

test('the golden round-trip is byte-identical across runs (determinism)', () => {
  const a = freshOut();
  const b = freshOut();
  const opts = { historyPath: G_HISTORY, resultsPath: G_RESULTS, scalabilityPath: G_SCAL };
  renderAll({ ...opts, outDir: a });
  renderAll({ ...opts, outDir: b });
  for (const name of EXPECTED) {
    assert.equal(readFileSync(join(a, name), 'utf8'), readFileSync(join(b, name), 'utf8'), `${name} stable`);
  }
});

test('render.mjs actually consumes the RunMetrics value fields (proof of reads)', () => {
  const outDir = freshOut();
  renderAll({ resultsPath: G_RESULTS, outDir });
  // wall_time {secs:0, nanos:85000000} → 85 ms label; libvips 140 ms.
  const wall = readFileSync(join(outDir, 'chart_wall_time.svg'), 'utf8');
  assert.ok(wall.includes('85ms'), 'monolithic wall time (from the nested Duration) is charted');
  assert.ok(wall.includes('140ms'), 'libvips wall time is charted');
  // peak_rss_bytes 209_715_200 / 1 MiB = 200 MB.
  const mem = readFileSync(join(outDir, 'chart_peak_memory.svg'), 'utf8');
  assert.ok(mem.includes('200MB'), 'peak RSS is derived from peak_rss_bytes and charted');
});

test('buildComparisonCharts emits exactly the five grouped-bar charts', () => {
  const charts = buildComparisonCharts(loadResults());
  const names = charts.map((c) => c.filename).sort();
  assert.deepEqual(names, [
    'chart_efficiency.svg',
    'chart_peak_memory.svg',
    'chart_resource_cost.svg',
    'chart_throughput.svg',
    'chart_wall_time.svg',
  ]);
  // One config group (2048x2048_c0) with a bar per engine.
  const wall = charts.find((c) => c.filename === 'chart_wall_time.svg').svg;
  for (const engine of ['Monolithic', 'Streaming', 'MapReduce', 'libvips']) {
    assert.ok(wall.includes(engine), `${engine} charted`);
  }
});

test('shape probes accept the committed golden fixtures', () => {
  assert.equal(resultsShapeOk(loadResults()), true, 'golden results match the RunMetrics shape');
  assert.equal(historyShapeOk(JSON.parse(readFileSync(G_HISTORY, 'utf8'))), true, 'golden history matches');
  assert.equal(scalabilityShapeOk(JSON.parse(readFileSync(G_SCAL, 'utf8'))), true, 'golden scalability matches');
});

test('resultsShapeOk treats absent/empty data as not-a-signal', () => {
  assert.equal(resultsShapeOk(null), true);
  assert.equal(resultsShapeOk([]), true);
});

test('a renamed/removed RunMetrics field trips the shape guard (drift caught)', () => {
  // Rename each load-bearing field in turn; every one must flip the guard.
  const rename = (obj, from, to) => {
    const clone = structuredClone(obj);
    clone[0][to] = clone[0][from];
    delete clone[0][from];
    return clone;
  };
  for (const [from, to] of [
    ['engine', 'engine_name'],
    ['width', 'w'],
    ['height', 'h'],
    ['concurrency', 'threads'],
    ['peak_rss_bytes', 'rss_bytes'],
    ['tiles_produced', 'tiles'],
    ['wall_time', 'wall_time_ns'],
  ]) {
    assert.equal(resultsShapeOk(rename(loadResults(), from, to)), false, `renaming ${from} trips the guard`);
  }
  // Nested Duration drift: rename secs/nanos inside wall_time.
  const secsDrift = structuredClone(loadResults());
  secsDrift[0].wall_time.seconds = secsDrift[0].wall_time.secs;
  delete secsDrift[0].wall_time.secs;
  assert.equal(resultsShapeOk(secsDrift), false, 'renaming the nested Duration secs trips the guard');
});
