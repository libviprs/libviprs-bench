// TDD spec for render.mjs — the CLI that turns the harness JSON
// (benchmark_history.json + scalability_results.json) into the SVG files the
// report/article embed. Deterministic output; missing inputs are skipped,
// not fatal (a single bench run writes only one of the two JSON files).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, mkdtempSync, existsSync, readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname, basename } from 'node:path';
import { fileURLToPath } from 'node:url';

import { renderAll, historyShapeOk, scalabilityShapeOk } from './render.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const FIX = join(HERE, 'fixtures');
const HISTORY = join(FIX, 'sample_history.json');
const SCAL = join(FIX, 'sample_scalability.json');

const EXPECTED = [
  // history: one config (1024x1024 c0) → time + memory
  'chart_history_1024x1024_c0_time.svg',
  'chart_history_1024x1024_c0_memory.svg',
  // scalability: five metrics × two concurrencies (c1, c4)
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
  return mkdtempSync(join(tmpdir(), 'libviprs-charts-'));
}

function segmentsFor(svg, color) {
  const re = new RegExp(`<polyline points="([^"]*)"[^>]*stroke="${color}"`, 'g');
  const out = [];
  let m;
  while ((m = re.exec(svg)) !== null) out.push(m[1].trim());
  return out;
}

test('renderAll writes every expected SVG from the sample JSON', () => {
  const outDir = freshOut();
  // No results JSON here → only the history + scalability set (the grouped-bar
  // comparison charts have their own coverage), so this stays an exact-set check.
  const written = renderAll({
    historyPath: HISTORY,
    scalabilityPath: SCAL,
    resultsPath: join(FIX, 'no-results.json'),
    outDir,
  });
  const names = written.map((p) => basename(p)).sort();
  for (const want of EXPECTED) {
    assert.ok(names.includes(want), `render.mjs writes ${want}`);
    const full = join(outDir, want);
    assert.ok(existsSync(full), `${want} exists on disk`);
    const svg = readFileSync(full, 'utf8');
    assert.match(svg, /^<svg/);
    assert.match(svg, /<\/svg>$/);
  }
  // Exactly the expected set, nothing spurious.
  assert.deepEqual(names, [...EXPECTED].sort());
});

test('render output is deterministic (no timestamps / rng) across runs', () => {
  const a = freshOut();
  const b = freshOut();
  renderAll({ historyPath: HISTORY, scalabilityPath: SCAL, outDir: a });
  renderAll({ historyPath: HISTORY, scalabilityPath: SCAL, outDir: b });
  for (const name of readdirSync(a)) {
    assert.equal(
      readFileSync(join(a, name), 'utf8'),
      readFileSync(join(b, name), 'utf8'),
      `${name} is byte-identical across runs`,
    );
  }
});

test('the history time chart reflects the fixture gap (streaming broken)', () => {
  const outDir = freshOut();
  renderAll({ historyPath: HISTORY, scalabilityPath: SCAL, outDir });
  const svg = readFileSync(join(outDir, 'chart_history_1024x1024_c0_time.svg'), 'utf8');
  // Legends show the title-cased label, so match case-insensitively.
  for (const engine of ['monolithic', 'streaming', 'mapreduce', 'libvips']) {
    assert.ok(svg.toLowerCase().includes(engine), `history chart legends ${engine}`);
  }
  assert.ok(svg.includes('0.3.0') && svg.includes('0.3.3'), 'version ticks present');
  // streaming is absent from snapshot 2 → its line is broken (>1 segment).
  // (#4285f4=mono, #34a853=streaming — colours come from the shared map.)
  const streamingSegs = segmentsFor(svg, '#34a853');
  assert.ok(streamingSegs.length >= 2, 'streaming polyline is broken at the gap');
});

test('missing input files are skipped, not fatal', () => {
  const outDir = freshOut();
  const written = renderAll({
    historyPath: join(FIX, 'does-not-exist.json'),
    scalabilityPath: join(FIX, 'nope.json'),
    outDir,
  });
  assert.deepEqual(written, [], 'nothing written when no JSON is present');
});

test('history-only input still renders the history charts', () => {
  const outDir = freshOut();
  const written = renderAll({ historyPath: HISTORY, scalabilityPath: join(FIX, 'nope.json'), outDir });
  const names = written.map((p) => basename(p));
  assert.ok(names.includes('chart_history_1024x1024_c0_time.svg'));
  assert.ok(!names.some((n) => n.startsWith('scalability_')), 'no scalability charts without its JSON');
});

test('shape probes tell field-drift apart from absent/empty/valid data', () => {
  // Absent / empty inputs are NOT a shape signal.
  assert.equal(historyShapeOk(null), true);
  assert.equal(historyShapeOk([]), true);
  assert.equal(scalabilityShapeOk([]), true);
  // Well-shaped records pass.
  assert.equal(historyShapeOk([{ runs: [{ engine: 'x', wall_time: {}, width: 1 }] }]), true);
  assert.equal(scalabilityShapeOk([{ engine: 'x', megapixels: 1, wall_time_ms: 2 }]), true);
  // Records present but missing the load-bearing fields → drift → false.
  assert.equal(historyShapeOk([{ runs: [{ engine: 'x', wall_time_ns: 5 }] }]), false);
  assert.equal(scalabilityShapeOk([{ engine: 'x', mp: 1 }]), false);
});

test('the committed fixtures match the shapes render.mjs reads', () => {
  const history = JSON.parse(readFileSync(HISTORY, 'utf8'));
  const scal = JSON.parse(readFileSync(SCAL, 'utf8'));
  assert.equal(historyShapeOk(history), true, 'sample_history matches the RunMetrics shape');
  assert.equal(scalabilityShapeOk(scal), true, 'sample_scalability matches the ScalabilityPoint shape');
});

test('--linear option changes the scalability rendering', () => {
  const logDir = freshOut();
  const linDir = freshOut();
  renderAll({ scalabilityPath: SCAL, outDir: logDir, linear: false });
  renderAll({ scalabilityPath: SCAL, outDir: linDir, linear: true });
  const logSvg = readFileSync(join(logDir, 'scalability_wall_time_c1.svg'), 'utf8');
  const linSvg = readFileSync(join(linDir, 'scalability_wall_time_c1.svg'), 'utf8');
  assert.notEqual(logSvg, linSvg, 'linear vs log-log produce different SVGs');
});

test('--zoom adds large-image _zoom scalability variants while keeping full-range', () => {
  // #43: zoom is opt-in and ADDITIVE — the default full-range charts are still
  // written (they are the default view), and a `_zoom` variant restricted to
  // the large-image regime is written alongside.
  const outDir = freshOut();
  const written = renderAll({
    scalabilityPath: SCAL,
    historyPath: join(FIX, 'none.json'),
    resultsPath: join(FIX, 'none.json'),
    outDir,
    zoom: 10,
  });
  const names = written.map((p) => basename(p));
  assert.ok(names.includes('scalability_wall_time_c1.svg'), 'full-range chart still emitted (default view)');
  assert.ok(names.includes('scalability_wall_time_c1_zoom.svg'), 'zoomed large-image variant emitted');
  const full = readFileSync(join(outDir, 'scalability_wall_time_c1.svg'), 'utf8');
  const zoom = readFileSync(join(outDir, 'scalability_wall_time_c1_zoom.svg'), 'utf8');
  assert.notEqual(full, zoom, 'the zoomed regime renders differently from the full range');
});
