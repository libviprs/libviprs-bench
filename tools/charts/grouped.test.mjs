// TDD spec for the grouped-bar COMPARISON renderers (#42) — the JS port of
// causl-bench's renderMetricGroupedBars / renderWallTimeBars /
// renderPeakMemoryBars / renderThroughputBars / renderResourceCostBars,
// adapted to libviprs's engines (monolithic / streaming / mapreduce + the
// libvips oracle) and the flat benchmark_results.json (Vec<RunMetrics>) shape.
//
// Finishing this JS migration lets the Rust plotters `generate_charts` — the
// last plotters user — be deleted and the dependency dropped entirely.
//
// Structure/determinism contract mirrors chart.test.mjs: canonical ENGINE_ORDER
// iteration, all number formatting through the shared helpers, no timestamps /
// rng, so the same input yields byte-identical SVG.

import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  COLORS,
  renderMetricGroupedBars,
  renderWallTimeBars,
  renderPeakMemoryBars,
  renderThroughputBars,
  renderEfficiencyBars,
  renderResourceCostBars,
} from './chart.mjs';

const ENGINES = ['monolithic', 'streaming', 'mapreduce', 'libvips'];

function expectValidSvg(svg) {
  assert.match(svg, /^<svg/);
  assert.match(svg, /<\/svg>$/);
  assert.ok(!svg.includes('NaN'), 'SVG must not leak NaN');
  assert.ok(!svg.includes('undefined'), 'SVG must not leak undefined');
}

// One bar per (config, engine). Two configs; the 2048 group is 4x taller so
// the geometry tests can see the value → height mapping.
function metricRows() {
  const base = { monolithic: 20, streaming: 24, mapreduce: 22, libvips: 40 };
  const rows = [];
  for (const config of ['1024x1024_c0', '2048x2048_c0']) {
    const scale = config.startsWith('2048') ? 4 : 1;
    for (const engine of ENGINES) rows.push({ config, engine, value: base[engine] * scale });
  }
  return rows;
}

// All <rect> with the given fill, as {x,y,width,height}, in document order.
// Both the bars and the 10x10 legend swatch share the shape, so callers
// filter the swatch out by its width.
function rectsFor(svg, color) {
  const re = new RegExp(
    `<rect x="([^"]*)" y="([^"]*)" width="([^"]*)" height="([^"]*)" fill="${color}"`,
    'g',
  );
  const out = [];
  let m;
  while ((m = re.exec(svg)) !== null) {
    out.push({ x: Number(m[1]), y: Number(m[2]), width: Number(m[3]), height: Number(m[4]) });
  }
  return out;
}

// Bars only (drop the 10px legend swatch).
function barsFor(svg, color) {
  return rectsFor(svg, color).filter((r) => r.width !== 10);
}

test('renderMetricGroupedBars emits a well-formed SVG with a bar per engine per config', () => {
  const svg = renderMetricGroupedBars(metricRows(), { title: 'Wall Time', unitSuffix: 'ms' });
  expectValidSvg(svg);
  assert.ok(svg.includes('Wall Time'), 'title present');
  // 2 configs x 4 engines = 8 bars total.
  const allBars = ENGINES.flatMap((e) => barsFor(svg, COLORS[e]));
  assert.equal(allBars.length, 8, 'one bar per (config, engine) cell');
  // Legend + group labels carry every engine (title-cased) and every config.
  for (const engine of ENGINES) assert.ok(svg.toLowerCase().includes(engine), `legend ${engine}`);
  assert.ok(svg.includes('1024x1024_c0') && svg.includes('2048x2048_c0'), 'config group labels');
});

test('each engine gets one bar per config at an increasing x, taller for the bigger value', () => {
  const svg = renderMetricGroupedBars(metricRows(), { title: 't', unitSuffix: 'ms' });
  for (const engine of ENGINES) {
    const bars = barsFor(svg, COLORS[engine]);
    assert.equal(bars.length, 2, `${engine} has a bar in each config group`);
    // Config groups run left→right (1024 then 2048).
    assert.ok(bars[0].x < bars[1].x, `${engine} second group sits right of the first`);
    // Same bar width within an engine.
    assert.ok(Math.abs(bars[0].width - bars[1].width) < 1e-9, `${engine} bar width is uniform`);
    // The 2048 config is 4x the value → a strictly taller bar.
    assert.ok(bars[1].height > bars[0].height, `${engine} taller bar for the bigger value`);
  }
});

test('engines are drawn in canonical order within a group (bar x increases by engine)', () => {
  const svg = renderMetricGroupedBars(metricRows(), { title: 't', unitSuffix: 'ms' });
  // First-group x for each engine, in ENGINE_ORDER, must strictly increase.
  const firstGroupX = ENGINES.map((e) => barsFor(svg, COLORS[e])[0].x);
  for (let i = 1; i < firstGroupX.length; i++) {
    assert.ok(firstGroupX[i] > firstGroupX[i - 1], 'engine bars ordered left→right by ENGINE_ORDER');
  }
});

test('the tallest bar fills the plot area (value → height scaling)', () => {
  const svg = renderMetricGroupedBars(metricRows(), { title: 't', unitSuffix: 'ms' });
  // maxV = libvips@2048 = 160. Its bar height must equal the plot height
  // (height 320 − 2·56 padding − 26 legend = 182).
  const tallest = barsFor(svg, COLORS.libvips).reduce((a, b) => (b.height > a.height ? b : a));
  assert.ok(Math.abs(tallest.height - 182) < 1e-6, `tallest bar spans the plot height, got ${tallest.height}`);
});

test('renderMetricGroupedBars is byte-for-byte deterministic', () => {
  const opts = { title: 'Wall Time', unitSuffix: 'ms' };
  assert.equal(renderMetricGroupedBars(metricRows(), opts), renderMetricGroupedBars(metricRows(), opts));
});

test('renderMetricGroupedBars placeholders empty input (no throw)', () => {
  expectValidSvg(renderMetricGroupedBars([], { title: 'Wall Time', unitSuffix: 'ms' }));
});

test('a missing (config, engine) cell renders as a zero-height bar, engine still shown', () => {
  // libvips only present in the second config → a zero bar in the first.
  const rows = [
    { config: 'a', engine: 'monolithic', value: 10 },
    { config: 'a', engine: 'streaming', value: 12 },
    { config: 'b', engine: 'monolithic', value: 20 },
    { config: 'b', engine: 'streaming', value: 24 },
    { config: 'b', engine: 'libvips', value: 40 },
  ];
  const svg = renderMetricGroupedBars(rows, { title: 't', unitSuffix: '' });
  expectValidSvg(svg);
  const vipsBars = barsFor(svg, COLORS.libvips);
  assert.equal(vipsBars.length, 2, 'a bar slot in every config group (union of engines)');
  const zero = vipsBars.find((b) => b.height === 0);
  assert.ok(zero, 'the config missing libvips renders a zero-height bar');
  assert.ok(svg.toLowerCase().includes('libvips'), 'engine kept in the legend');
});

test('non-canonical engines are drawn with a fallback colour, not dropped', () => {
  const rows = [
    { config: 'a', engine: 'libvips', value: 5 },
    { config: 'a', engine: 'gpu', value: 3 },
  ];
  const svg = renderMetricGroupedBars(rows, { title: 't', unitSuffix: '' });
  expectValidSvg(svg);
  assert.ok(svg.includes('gpu'), 'non-canonical engine in the legend');
  const canonical = new Set(Object.values(COLORS));
  const fills = [...svg.matchAll(/<rect[^>]*fill="([^"]+)"/g)].map((m) => m[1]);
  assert.ok(fills.some((f) => !canonical.has(f)), 'a fallback colour is used for the extra engine');
});

test('value labels reflect the data through the shared number formatter', () => {
  const rows = [
    { config: 'a', engine: 'monolithic', value: 85 },
    { config: 'a', engine: 'libvips', value: 140 },
  ];
  const svg = renderMetricGroupedBars(rows, { title: 't', unitSuffix: 'ms' });
  assert.ok(svg.includes('85ms'), 'small value keeps decimals/units');
  assert.ok(svg.includes('140ms'), '>=100 value rounds to an integer');
});

test('the metric wrappers carry the right titles/units', () => {
  const rows = metricRows();
  assert.match(renderWallTimeBars(rows), /Wall Time/);
  assert.match(renderPeakMemoryBars(rows), /Peak RSS/);
  assert.match(renderThroughputBars(rows), /Throughput/);
  assert.match(renderEfficiencyBars(rows), /Efficiency/);
  assert.match(renderResourceCostBars(rows), /Resource Cost/);
  // Each wrapper is a thin call over the shared generic → still valid SVG.
  for (const svg of [
    renderWallTimeBars(rows),
    renderPeakMemoryBars(rows),
    renderThroughputBars(rows),
    renderEfficiencyBars(rows),
    renderResourceCostBars(rows),
  ]) {
    expectValidSvg(svg);
  }
});
