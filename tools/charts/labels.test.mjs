// Metric-labelling spec for the render.mjs chart titles (#25).
//
// The grouped-bar comparison charts already name each metric's unit + direction
// in their titles (chart.mjs wrappers). The SCALABILITY and HISTORY charts must
// do the same: a reader must never have to guess whether a line going up is good
// or bad, nor what "throughput" is measured in. This pins the titles the two
// render.mjs builders emit.

import { test } from 'node:test';
import assert from 'node:assert/strict';

import { buildScalabilityCharts, buildHistoryCharts } from './render.mjs';

const SCAL_POINTS = [
  {
    engine: 'monolithic',
    megapixels: 1,
    concurrency: 1,
    wall_time_ms: 10,
    peak_rss_mb: 5,
    tiles_produced: 100,
    tiles_per_second: 100,
    tiles_per_second_per_mb: 20,
    resource_cost: 0.01,
  },
  {
    engine: 'monolithic',
    megapixels: 4,
    concurrency: 1,
    wall_time_ms: 40,
    peak_rss_mb: 8,
    tiles_produced: 400,
    tiles_per_second: 200,
    tiles_per_second_per_mb: 25,
    resource_cost: 0.02,
  },
];

// Two snapshots for one config → a history trend exists.
function snapshot(version, wallSecs, rssBytes) {
  return {
    version,
    runs: [
      {
        engine: 'monolithic',
        width: 1024,
        height: 1024,
        concurrency: 0,
        wall_time: { secs: wallSecs, nanos: 0 },
        peak_rss_bytes: rssBytes,
        tiles_produced: 85,
      },
    ],
  };
}

test('scalability chart titles state their unit and direction', () => {
  const byName = Object.fromEntries(
    buildScalabilityCharts(SCAL_POINTS).map((c) => [c.filename, c.svg]),
  );

  // Wall time / peak RSS / resource cost: lower is better.
  assert.match(byName['scalability_wall_time_c1.svg'], /lower is better/, 'wall time direction');
  assert.match(byName['scalability_peak_memory_c1.svg'], /lower is better/, 'peak RSS direction');
  assert.match(byName['scalability_resource_cost_c1.svg'], /lower is better/, 'resource cost direction');

  // Throughput / efficiency: higher is better, and throughput names its tiles/s unit.
  const tput = byName['scalability_throughput_c1.svg'];
  assert.match(tput, /Tiles\/s/, 'throughput names its unit (tiles/s)');
  assert.match(tput, /higher is better/, 'throughput direction');
  assert.match(byName['scalability_efficiency_c1.svg'], /higher is better/, 'efficiency direction');

  // Throughput is tiles/s, never pixels/s.
  assert.ok(!tput.toLowerCase().includes('pixels/s'), 'throughput is tiles/s, not pixels/s');
});

test('history chart titles state their direction', () => {
  const charts = buildHistoryCharts([
    snapshot('0.3.0', 1, 100 * 1024 * 1024),
    snapshot('0.3.1', 2, 120 * 1024 * 1024),
  ]);
  assert.ok(charts.length >= 2, 'a wall-time and a peak-RSS trend are produced');
  for (const { filename, svg } of charts) {
    assert.match(svg, /lower is better/, `${filename} states its direction`);
  }
});
