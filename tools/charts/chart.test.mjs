// TDD spec for the ported causl-bench JS SVG chart library, adapted to
// libviprs's engines (monolithic / streaming / mapreduce + the libvips
// oracle) and its benchmark_history.json / scalability_results.json shapes.
//
// Pins the behaviour that issues #20 and #21 (and sub-issues #28/#29/#34)
// ask for:
//   #20  history-trend alignment — a point's x is its SNAPSHOT INDEX in the
//        timeline, shared by every engine, so a missing/late engine never
//        shifts the others; a missing snapshot BREAKS that engine's polyline
//        (a gap) instead of drawing a line across it.
//   #21  scalability log-log axes — log10 on BOTH the megapixel x-axis and
//        the metric y-axis; a decade spans EQUAL pixels; a linear mode is
//        preserved behind an option.
//
// These are ported from causl-bench's chart.test.ts / chart-shapes.test.ts /
// report-charts.test.ts (structure, determinism contract, placeholder
// contract), using node's built-in test runner — no vitest, no TS build.

import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  COLORS,
  ENGINE_ORDER,
  formatNumber,
  formatLogTick,
  svgPlaceholder,
  renderHistoryTrend,
  renderScalabilityChart,
  log10Scale,
  linearScale,
  decadeTicks,
  enclosingDecades,
} from './chart.mjs';

/* -------------------------------------------------------------------------- */
/* Test helpers                                                               */
/* -------------------------------------------------------------------------- */

const ENGINES = ['monolithic', 'streaming', 'mapreduce', 'libvips'];

function expectValidSvg(svg) {
  assert.match(svg, /^<svg/);
  assert.match(svg, /<\/svg>$/);
  assert.ok(!svg.includes('NaN'), 'SVG must not leak NaN');
  assert.ok(!svg.includes('undefined'), 'SVG must not leak undefined');
}

// All polyline point-arrays whose stroke is `color`, in document order.
// Each returned entry is an array of {x, y}. A broken line yields more
// than one entry for the same colour.
function segmentsFor(svg, color) {
  const re = new RegExp(`<polyline points="([^"]*)"[^>]*stroke="${color}"`, 'g');
  const out = [];
  let m;
  while ((m = re.exec(svg)) !== null) {
    const pts = m[1]
      .trim()
      .split(/\s+/)
      .filter(Boolean)
      .map((pair) => {
        const [x, y] = pair.split(',').map(Number);
        return { x, y };
      });
    out.push(pts);
  }
  return out;
}

// History points mirroring the committed fixture's presence matrix.
// mono + libvips span every run; streaming skips run 2 (mid gap);
// mapreduce skips run 0 (late start).
function historyPoints() {
  const wall = {
    monolithic: [20, 19, 18, 17],
    streaming: [24, 23, null, 21],
    mapreduce: [null, 30, 28, 26],
    libvips: [40, 39, 38, 37],
  };
  const versions = ['0.3.0', '0.3.1', '0.3.2', '0.3.3'];
  const points = [];
  for (const engine of ENGINES) {
    wall[engine].forEach((value, runIndex) => {
      if (value === null) return;
      points.push({ runIndex, version: versions[runIndex], engine, value });
    });
  }
  return points;
}

// Scalability points: 4 engines across four decades of megapixels, value a
// clean power law of MP so a log-log plot is a straight, equal-step line.
function scalabilityPoints(slope = { monolithic: 10, streaming: 12, mapreduce: 11, libvips: 15 }) {
  const mps = [0.1, 1, 10, 100];
  const points = [];
  for (const engine of ENGINES) {
    for (const mp of mps) {
      points.push({ engine, megapixels: mp, value: slope[engine] * mp });
    }
  }
  return points;
}

/* -------------------------------------------------------------------------- */
/* Shared helpers                                                             */
/* -------------------------------------------------------------------------- */

test('formatNumber is locale-stable and finite-safe', () => {
  assert.equal(formatNumber(Number.NaN), 'n/a');
  assert.equal(formatNumber(Number.POSITIVE_INFINITY), 'n/a');
  assert.equal(formatNumber(150.7), '151'); // >= 100 rounds to an integer
  assert.equal(formatNumber(12.5), '12.5');
  assert.equal(formatNumber(1.234), '1.23'); // <100 keeps 2 dp, trimmed
  assert.equal(formatNumber(0.5), '0.5');
});

test('COLORS + ENGINE_ORDER cover every libviprs engine and the oracle', () => {
  for (const engine of ENGINES) {
    assert.ok(ENGINE_ORDER.includes(engine), `${engine} in ENGINE_ORDER`);
    assert.match(COLORS[engine], /^#[0-9a-f]{6}$/i, `${engine} has a hex colour`);
  }
  // libvips (the oracle) leads, then the three libviprs engines.
  assert.equal(ENGINE_ORDER[0], 'libvips');
});

test('svgPlaceholder is a well-formed no-data SVG carrying its label', () => {
  const svg = svgPlaceholder(400, 200, 'wall time');
  expectValidSvg(svg);
  assert.match(svg, /no data/);
  assert.match(svg, /wall time/);
});

/* -------------------------------------------------------------------------- */
/* #20 — history trend                                                        */
/* -------------------------------------------------------------------------- */

test('renderHistoryTrend emits a well-formed SVG with every engine + version tick', () => {
  const svg = renderHistoryTrend(historyPoints(), {
    title: 'Wall Time History — 1024x1024 c0',
    unitSuffix: 'ms',
  });
  expectValidSvg(svg);
  // Legends print the title-cased display label (Monolithic/Streaming/…),
  // so match case-insensitively on the engine key.
  for (const engine of ENGINES) {
    assert.ok(svg.toLowerCase().includes(engine), `legend has ${engine}`);
  }
  // Version labels become x-axis ticks.
  assert.ok(svg.includes('0.3.0'));
  assert.ok(svg.includes('0.3.3'));
});

test('renderHistoryTrend placeholders empty input (no throw)', () => {
  expectValidSvg(renderHistoryTrend([], { title: 'Wall Time', unitSuffix: 'ms' }));
});

test('renderHistoryTrend is byte-for-byte deterministic', () => {
  const opts = { title: 'Wall Time', unitSuffix: 'ms' };
  assert.equal(
    renderHistoryTrend(historyPoints(), opts),
    renderHistoryTrend(historyPoints(), opts),
  );
});

test('#20 history points align at their SNAPSHOT-INDEX x, not per-engine position', () => {
  const svg = renderHistoryTrend(historyPoints(), { title: 't', unitSuffix: 'ms' });

  // The spine engine is present in every run — one contiguous 4-point line.
  const monoSegs = segmentsFor(svg, COLORS.monolithic);
  assert.equal(monoSegs.length, 1, 'contiguous engine draws exactly one polyline');
  const mono = monoSegs[0];
  assert.equal(mono.length, 4, 'mono has a point per snapshot');
  const xByRun = mono.map((p) => p.x); // xByRun[i] = x of snapshot i

  // mapreduce is ABSENT from snapshot 0 → its first point is snapshot 1.
  // The regression this pins: the old code indexed x by the per-engine
  // array position, so mapreduce's first point landed at x(run0). The fix
  // lands it at x(run1).
  const mrSegs = segmentsFor(svg, COLORS.mapreduce);
  const mr = mrSegs.flat();
  assert.equal(mr.length, 3, 'mapreduce has three points (runs 1,2,3)');
  assert.ok(
    Math.abs(mr[0].x - xByRun[1]) < 1e-6,
    'late-starting engine aligns at snapshot-1 x',
  );
  assert.ok(
    Math.abs(mr[0].x - xByRun[0]) > 1e-6,
    'late-starting engine is NOT parked at snapshot-0 x',
  );
  // Its last point aligns at the final snapshot too.
  assert.ok(Math.abs(mr[2].x - xByRun[3]) < 1e-6, 'aligns at snapshot-3 x');
});

test('#20 a missing snapshot BREAKS the polyline (gap), not a line across it', () => {
  const svg = renderHistoryTrend(historyPoints(), { title: 't', unitSuffix: 'ms' });
  const monoSegs = segmentsFor(svg, COLORS.monolithic);
  const xByRun = monoSegs[0].map((p) => p.x);

  // streaming is present at runs {0,1,3}, ABSENT at run 2 → the line must
  // break into a [0,1] segment and a separate [3] segment.
  const segs = segmentsFor(svg, COLORS.streaming);
  const totalPts = segs.reduce((n, s) => n + s.length, 0);
  assert.equal(totalPts, 3, 'streaming contributes three points total');
  assert.ok(segs.length >= 2, 'streaming line is broken into at least two segments');

  // No single segment may span the gap: none contains both the run-1 x and
  // the run-3 x (that would be a line drawn straight across missing run 2).
  const spansGap = segs.some(
    (s) =>
      s.some((p) => Math.abs(p.x - xByRun[1]) < 1e-6) &&
      s.some((p) => Math.abs(p.x - xByRun[3]) < 1e-6),
  );
  assert.ok(!spansGap, 'no polyline segment bridges the missing snapshot');

  // The pre-gap segment is a real 2-point line at runs 0 and 1.
  const preGap = segs.find((s) => s.length === 2);
  assert.ok(preGap, 'a two-point pre-gap segment exists');
  assert.ok(Math.abs(preGap[0].x - xByRun[0]) < 1e-6);
  assert.ok(Math.abs(preGap[1].x - xByRun[1]) < 1e-6);
});

/* -------------------------------------------------------------------------- */
/* #21 — scalability log-log                                                  */
/* -------------------------------------------------------------------------- */

test('log10Scale maps a decade to EQUAL pixels', () => {
  const s = log10Scale(1, 1000, 0, 300); // three decades over 300px
  assert.ok(Math.abs(s(1) - 0) < 1e-9);
  assert.ok(Math.abs(s(1000) - 300) < 1e-9);
  const d1 = s(10) - s(1);
  const d2 = s(100) - s(10);
  const d3 = s(1000) - s(100);
  assert.ok(Math.abs(d1 - 100) < 1e-6, 'each decade is 100px');
  assert.ok(Math.abs(d1 - d2) < 1e-6 && Math.abs(d2 - d3) < 1e-6, 'decades are equal-width');
});

test('linearScale maps proportionally (linear mode preserved)', () => {
  const l = linearScale(0, 100, 0, 300);
  assert.ok(Math.abs(l(0) - 0) < 1e-9);
  assert.ok(Math.abs(l(50) - 150) < 1e-9);
  assert.ok(Math.abs(l(100) - 300) < 1e-9);
});

test('decadeTicks yields power-of-ten majors and 2..9 minors within range', () => {
  const { major, minor } = decadeTicks(0.1, 100);
  assert.equal(major.length, 4);
  [0.1, 1, 10, 100].forEach((v, i) => assert.ok(Math.abs(major[i] - v) < 1e-9));
  // Minors are the 2..9 multiples of each decade, never a power of ten.
  assert.ok(minor.some((v) => Math.abs(v - 5) < 1e-9), 'includes 5');
  assert.ok(minor.some((v) => Math.abs(v - 50) < 1e-9), 'includes 50');
  assert.ok(minor.some((v) => Math.abs(v - 0.5) < 1e-9), 'includes 0.5');
  assert.ok(
    minor.every((v) => Math.abs(Math.log10(v) - Math.round(Math.log10(v))) > 1e-9),
    'no minor tick is a power of ten',
  );
});

test('renderScalabilityChart emits one polyline per engine + title', () => {
  const svg = renderScalabilityChart(scalabilityPoints(), {
    title: 'Wall Time Scalability',
    xLabel: 'Megapixels',
    yLabel: 'Time (ms)',
    unitSuffix: 'ms',
  });
  expectValidSvg(svg);
  assert.ok(svg.includes('Wall Time Scalability'));
  for (const engine of ENGINES) {
    assert.equal(segmentsFor(svg, COLORS[engine]).length, 1, `${engine} draws one line`);
  }
});

test('renderScalabilityChart placeholders empty input', () => {
  expectValidSvg(
    renderScalabilityChart([], { title: 'Wall Time', xLabel: 'MP', yLabel: 'ms', unitSuffix: 'ms' }),
  );
});

test('renderScalabilityChart is byte-for-byte deterministic', () => {
  const opts = { title: 'Wall Time', xLabel: 'MP', yLabel: 'ms', unitSuffix: 'ms' };
  assert.equal(
    renderScalabilityChart(scalabilityPoints(), opts),
    renderScalabilityChart(scalabilityPoints(), opts),
  );
});

test('#21 log-LOG maps BOTH axes: equal decade steps on x AND y', () => {
  // monolithic value = 10 * MP → across MP [0.1,1,10,100] the values are
  // [1,10,100,1000]: four decades on x AND four on y. In a true log-log
  // plot both the x-steps and the y-steps between consecutive points are
  // equal.
  const svg = renderScalabilityChart(scalabilityPoints(), {
    title: 't',
    xLabel: 'MP',
    yLabel: 'ms',
    unitSuffix: 'ms',
    logScale: true,
  });
  const line = segmentsFor(svg, COLORS.monolithic)[0];
  assert.equal(line.length, 4);
  const dx = [line[1].x - line[0].x, line[2].x - line[1].x, line[3].x - line[2].x];
  const dy = [line[1].y - line[0].y, line[2].y - line[1].y, line[3].y - line[2].y];
  assert.ok(Math.abs(dx[0] - dx[1]) < 1e-4 && Math.abs(dx[1] - dx[2]) < 1e-4, 'x decades equal');
  assert.ok(Math.abs(dy[0] - dy[1]) < 1e-4 && Math.abs(dy[1] - dy[2]) < 1e-4, 'y decades equal');
});

test('#21 linear mode is preserved behind an option and differs from log-log', () => {
  const base = { title: 't', xLabel: 'MP', yLabel: 'ms', unitSuffix: 'ms' };
  const logSvg = renderScalabilityChart(scalabilityPoints(), { ...base, logScale: true });
  const linSvg = renderScalabilityChart(scalabilityPoints(), { ...base, logScale: false });
  expectValidSvg(linSvg);
  assert.notEqual(logSvg, linSvg, 'log and linear renderings differ');

  // In LINEAR mode the megapixel steps 0.1→1→10→100 are increasing, so the
  // x-gaps grow (they are NOT equal decades).
  const line = segmentsFor(linSvg, COLORS.monolithic)[0];
  const dx = [line[1].x - line[0].x, line[2].x - line[1].x, line[3].x - line[2].x];
  assert.ok(dx[0] < dx[1] && dx[1] < dx[2], 'linear x-gaps grow with megapixels');
});

/* -------------------------------------------------------------------------- */
/* Review follow-ups: axis math, degenerate/failure inputs, resilience.       */
/* -------------------------------------------------------------------------- */

test('formatLogTick keeps small/large decades distinct (no 0.001→0 collision)', () => {
  assert.equal(formatLogTick(0.001), '1e-3');
  assert.equal(formatLogTick(0.0001), '1e-4'); // distinct from 0.001
  assert.equal(formatLogTick(0.01), '0.01');
  assert.equal(formatLogTick(0.1), '0.1');
  assert.equal(formatLogTick(1), '1');
  assert.equal(formatLogTick(1000), '1000');
  assert.equal(formatLogTick(100000), '1e5');
  assert.equal(formatLogTick(Number.NaN), 'n/a');
});

test('enclosingDecades snaps out to powers of ten', () => {
  assert.deepEqual(enclosingDecades(0.18, 11.8), [0.1, 100]);
  assert.deepEqual(enclosingDecades(1, 100), [1, 100]); // exact powers unchanged
  const [lo, hi] = enclosingDecades(5, 5); // single value expands each side
  assert.ok(lo < 5 && hi > 5);
  assert.deepEqual(enclosingDecades(-1, 10), [-1, 10]); // non-positive passed through
});

test('log10Scale guards a non-positive domain / value (no NaN leak)', () => {
  assert.equal(log10Scale(-1, 10, 0, 300)(5), 0); // bad domain → rangeMin
  assert.equal(log10Scale(1, 10, 0, 300)(-5), 0); // non-positive value → rangeMin
  assert.ok(Number.isFinite(log10Scale(1, 1000, 0, 300)(10)));
});

test('decadeTicks handles zero-span and non-positive bounds', () => {
  assert.deepEqual(decadeTicks(0, 10), { major: [], minor: [] });
  assert.deepEqual(decadeTicks(-5, 10), { major: [], minor: [] });
  assert.deepEqual(decadeTicks(10, 10).major, [10]); // exact power, zero span
});

test('#21 log domain snaps so the extreme data points sit inside the frame', () => {
  const padding = 64;
  const width = 700;
  const mps = [0.18, 0.74, 2.95, 11.8]; // non-power endpoints
  const points = mps.map((mp) => ({ engine: 'monolithic', megapixels: mp, value: mp * 10 }));
  const line = segmentsFor(
    renderScalabilityChart(points, { title: 't', logScale: true }),
    COLORS.monolithic,
  )[0];
  const xsPlotted = line.map((p) => p.x);
  assert.ok(Math.max(...xsPlotted) < width - padding - 1e-6, 'largest point left of the right frame');
  assert.ok(Math.min(...xsPlotted) > padding + 1e-6, 'smallest point right of the left frame');
});

test('#21 a sub-decade metric range still yields labeled decade ticks', () => {
  // efficiency-like values all inside one decade (3..3.9): raw min/max would
  // emit zero major ticks; snapping to [1,10] labels the axis.
  const points = [0.5, 1, 2, 4].map((mp, i) => ({
    engine: 'monolithic',
    megapixels: mp,
    value: 3 + i * 0.3,
  }));
  const svg = renderScalabilityChart(points, { title: 't', yLabel: 'eff', logScale: true });
  expectValidSvg(svg);
  const endLabels = [...svg.matchAll(/text-anchor="end"[^>]*>([^<]+)<\/text>/g)].map((m) => m[1]);
  assert.ok(endLabels.includes('1'), 'y axis labels the 1 decade');
  assert.ok(endLabels.includes('10'), 'y axis labels the 10 decade');
});

test('#21 log mode omits <=0 / non-finite points without leaking NaN or bridging', () => {
  const points = [
    { engine: 'streaming', megapixels: 1, value: 5 },
    { engine: 'streaming', megapixels: 10, value: 0 }, // failed → dropped
    { engine: 'streaming', megapixels: 100, value: 50 },
    { engine: 'monolithic', megapixels: 1, value: Number.NaN }, // all unplottable
    { engine: 'monolithic', megapixels: 10, value: -3 },
  ];
  const svg = renderScalabilityChart(points, { title: 't', logScale: true });
  expectValidSvg(svg);
  assert.ok(!svg.includes('Infinity'), 'no Infinity leak');
  // streaming's dropped middle point breaks the line: the two survivors are
  // isolated single-point segments, never one polyline bridging 1→100.
  const segs = segmentsFor(svg, COLORS.streaming);
  assert.ok(
    segs.every((s) => s.length === 1),
    'no polyline segment bridges the dropped size',
  );
  // monolithic had input points but none plottable → still disclosed in legend.
  assert.ok(svg.toLowerCase().includes('monolithic'), 'unplottable engine kept in legend');
  assert.match(svg, /omitted/, 'omitted-points annotation present');
});

test('#21 zero-span, single-point, and non-canonical inputs render safely', () => {
  // all-equal x and y (zero span)
  const flat = ['monolithic', 'streaming'].flatMap((engine) =>
    [1, 1, 1].map((mp) => ({ engine, megapixels: mp, value: 7 })),
  );
  expectValidSvg(renderScalabilityChart(flat, { title: 't', logScale: true }));
  // single point per engine
  const single = ENGINES.map((engine) => ({ engine, megapixels: 5, value: 10 }));
  expectValidSvg(renderScalabilityChart(single, { title: 't', logScale: true }));
  // a non-canonical engine must be DRAWN (fallback colour), not dropped
  const withExtra = [
    { engine: 'libvips', megapixels: 1, value: 5 },
    { engine: 'gpu', megapixels: 1, value: 3 },
    { engine: 'gpu', megapixels: 10, value: 30 },
  ];
  const svg = renderScalabilityChart(withExtra, { title: 't', logScale: true });
  expectValidSvg(svg);
  assert.ok(svg.includes('gpu'), 'non-canonical engine appears in legend');
  const canonical = new Set(Object.values(COLORS));
  const strokes = [...svg.matchAll(/<polyline[^>]*stroke="([^"]+)"/g)].map((m) => m[1]);
  assert.ok(strokes.some((s) => !canonical.has(s)), 'non-canonical engine drawn with a fallback colour');
});

test('#20 single-snapshot and all-non-finite history render safely', () => {
  const one = ENGINES.map((engine) => ({ runIndex: 0, version: 'v1', engine, value: 5 }));
  expectValidSvg(renderHistoryTrend(one, { title: 't', unitSuffix: 'ms' }));
  const bad = ENGINES.map((engine) => ({ runIndex: 0, version: 'v1', engine, value: Number.NaN }));
  expectValidSvg(renderHistoryTrend(bad, { title: 't', unitSuffix: 'ms' }));
});

test('#20 a wholly-absent snapshot does not shatter the lines into single dots', () => {
  // Config present only at runIndex 0 and 2; NO engine occupies run 1. The
  // two engines must draw a continuous 0→2 line, not two isolated points.
  const points = [];
  for (const engine of ['monolithic', 'libvips']) {
    for (const runIndex of [0, 2]) {
      points.push({ runIndex, version: `v${runIndex}`, engine, value: 10 + runIndex });
    }
  }
  const svg = renderHistoryTrend(points, { title: 't', unitSuffix: 'ms' });
  const seg = segmentsFor(svg, COLORS.monolithic);
  assert.equal(seg.length, 1, 'one continuous segment across the wholly-absent snapshot');
  assert.equal(seg[0].length, 2, 'both points joined');
});

test('COLORS and ENGINE_ORDER are frozen (palette/order determinism contract)', () => {
  assert.ok(Object.isFrozen(COLORS));
  assert.ok(Object.isFrozen(ENGINE_ORDER));
});

test('interpolated text is XML-escaped (no malformed SVG)', () => {
  const svg = renderScalabilityChart(scalabilityPoints(), { title: 'A & B <x>', xLabel: 'm', yLabel: 'v' });
  expectValidSvg(svg);
  assert.ok(svg.includes('A &amp; B &lt;x&gt;'), 'title special chars escaped');
});

test('min/max over a large series does not overflow the stack', () => {
  // Math.max(...arr) throws RangeError past ~100k args; the reduce-based
  // helpers must not.
  const n = 200000;
  const points = Array.from({ length: n }, () => ({ engine: 'monolithic', megapixels: 5, value: 10 }));
  assert.doesNotThrow(() => renderScalabilityChart(points, { title: 't', logScale: true }));
});

/* -------------------------------------------------------------------------- */
/* #43 — large-image x-axis zoom (xMin) on the scalability renderer.          */
/* The removed Rust `--crop` cropped the axis to the large-image regime by     */
/* DEFAULT; the JS replacement is an opt-in xMin, and the full-range log-log   */
/* chart stays the default.                                                    */
/* -------------------------------------------------------------------------- */

test('#43 xMin zooms into the large-image regime: filters out smaller sizes and rescales', () => {
  const pts = [0.1, 1, 10, 100].map((mp) => ({ engine: 'monolithic', megapixels: mp, value: mp * 10 }));
  const full = renderScalabilityChart(pts, { title: 't', logScale: true });
  const zoom = renderScalabilityChart(pts, { title: 't', logScale: true, xMin: 10 });

  const fullLine = segmentsFor(full, COLORS.monolithic)[0];
  const zoomLine = segmentsFor(zoom, COLORS.monolithic)[0];
  assert.equal(fullLine.length, 4, 'full-range plots every size');
  assert.equal(zoomLine.length, 2, 'zoom keeps only the >= xMin sizes (10, 100 MP)');

  // After the rescale the smallest retained size (10 MP) snaps to the left
  // frame (padding = 64), where in the full-range plot it sat mid-axis.
  const padding = 64;
  assert.ok(Math.abs(zoomLine[0].x - padding) < 1e-3, '10 MP sits on the left frame after zoom');
  assert.ok(
    fullLine.find((p) => Math.abs(p.x - padding) < 1e-3) === undefined,
    '10 MP is NOT on the left frame in the full-range plot',
  );
});

test('#43 default (no xMin) leaves the full-range rendering unchanged', () => {
  const pts = scalabilityPoints();
  const opts = { title: 't', xLabel: 'MP', yLabel: 'ms', logScale: true };
  assert.equal(
    renderScalabilityChart(pts, opts),
    renderScalabilityChart(pts, { ...opts, xMin: undefined }),
    'omitting xMin and passing xMin:undefined render identically',
  );
});

test('#43 an xMin above every size yields a safe placeholder (no throw / NaN)', () => {
  const pts = [0.1, 1, 10].map((mp) => ({ engine: 'monolithic', megapixels: mp, value: mp * 10 }));
  const svg = renderScalabilityChart(pts, { title: 'wall', logScale: true, xMin: 1000 });
  expectValidSvg(svg);
  assert.match(svg, /no data/);
});
