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
  svgPlaceholder,
  renderHistoryTrend,
  renderScalabilityChart,
  log10Scale,
  linearScale,
  decadeTicks,
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
  for (const engine of ENGINES) assert.ok(svg.includes(engine), `legend has ${engine}`);
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
