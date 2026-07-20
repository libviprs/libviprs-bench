#!/usr/bin/env node
/**
 * render.mjs — turn the libviprs benchmark harness JSON into the SVG files
 * the report/article embed. Consumes the JSON the Rust harness already
 * writes (nothing else changed on the data path):
 *
 *   report/benchmark_history.json   (Vec<BenchmarkSnapshot>)  → chart_history_*.svg
 *   report/benchmark_results.json   (Vec<RunMetrics>)         → chart_*.svg (grouped bars)
 *   report/scalability_results.json (Vec<ScalabilityPoint>)   → scalability_*.svg
 *
 * The SVG generation used to live in the Rust `report` / `scalability`
 * binaries (plotters); it now lives here entirely, reusing the proven
 * causl-bench chart code (see chart.mjs) — the grouped-bar comparison charts
 * were the last plotters user and are now rendered here too (#42), so the Rust
 * side emits JSON only and the plotters dependency is gone. `run-bench.sh`
 * invokes this after the harness produces the JSON, so charts regenerate on
 * every run.
 *
 * Output is deterministic (no timestamps, no rng) — the same JSON always
 * yields byte-identical SVGs. Missing input is skipped, not fatal: a single
 * bench invocation writes only some of the JSON files.
 *
 * Usage:
 *   node tools/charts/render.mjs [--report-dir DIR] [--out-dir DIR]
 *                                [--history FILE] [--results FILE]
 *                                [--scalability FILE] [--linear]
 *                                [--zoom <minMP>]
 */

import { readFileSync, writeFileSync, existsSync, mkdirSync } from 'node:fs';
import { join, dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import {
  renderHistoryTrend,
  renderScalabilityChart,
  renderWallTimeBars,
  renderPeakMemoryBars,
  renderTrackedMemoryBars,
  renderThroughputBars,
  renderEfficiencyBars,
  renderResourceCostBars,
} from './chart.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
/** Crate-root `report/` dir, relative to tools/charts/. */
const DEFAULT_REPORT_DIR = resolve(HERE, '../../report');

/* -------------------------------------------------------------------------- */
/* Data extraction                                                            */
/* -------------------------------------------------------------------------- */

/** serde `Duration` → milliseconds. */
function durationMs(d) {
  if (!d || typeof d !== 'object') return Number.NaN;
  return (d.secs ?? 0) * 1000 + (d.nanos ?? 0) / 1e6;
}

const BYTES_PER_MB = 1024 * 1024;

/* RunMetrics → metric value. These MIRROR the Rust `RunMetrics` methods in
 * src/lib.rs (impl RunMetrics, ~L281-344) one-for-one — wall_time_ms,
 * peak_rss_mb, tracked_memory_mb, tiles_per_second, tiles_per_second_per_mb,
 * resource_cost_per_tile — so the grouped-bar comparison charts read
 * benchmark_results.json to the SAME numbers the removed plotters
 * `generate_charts` drew and the text comparison_table.txt still prints. Keep
 * the two in lockstep: a change to a Rust method here must change its twin
 * (golden.test.mjs asserts the derived values against hand-computed oracles).
 * RSS is the cross-engine-comparable memory basis; efficiency / resource-cost
 * are 0 when it is unavailable, matching the Rust methods' `mb > 0` guard. */
function runWallSecs(run) {
  return durationMs(run.wall_time) / 1000;
}
function runWallTimeMs(run) {
  return durationMs(run.wall_time);
}
// `peak_rss_bytes` is `#[serde(default)] = 0`, where 0 means UNKNOWN. Unlike the
// HISTORY peak-memory extractor (which maps 0 → NaN to break a misleading
// flat-zero trend line), the comparison path returns 0 here to MIRROR the Rust
// `peak_rss_mb()` (which returns 0.0, feeding a 0-height bar and 0 efficiency /
// cost) so the chart numbers match the text table. A fresh benchmark_results.json
// — the only input this path consumes — always carries a real RSS, so the
// sentinel does not arise in practice; the divergence from the history extractor
// is deliberate (Rust parity over the history path's line-break heuristic).
function runPeakRssMb(run) {
  return (run.peak_rss_bytes ?? 0) / BYTES_PER_MB;
}
function runTrackedMemoryMb(run) {
  return (run.tracked_memory_bytes ?? 0) / BYTES_PER_MB;
}
function runTilesPerSecond(run) {
  const s = runWallSecs(run);
  return s > 0 ? (run.tiles_produced ?? 0) / s : 0;
}
function runTilesPerSecondPerMb(run) {
  const mb = runPeakRssMb(run);
  return mb > 0 ? runTilesPerSecond(run) / mb : 0;
}
function runResourceCostPerTile(run) {
  const mb = runPeakRssMb(run);
  const s = runWallSecs(run);
  const tiles = run.tiles_produced ?? 0;
  return tiles > 0 ? (mb * s) / tiles : 0;
}

/** Distinct config keys `{w}x{h}_c{conc}` over a flat run array, deterministically ordered. */
function configsOfRuns(runs) {
  const seen = new Map();
  for (const run of runs) {
    const key = `${run.width}x${run.height}_c${run.concurrency}`;
    if (!seen.has(key)) {
      seen.set(key, { width: run.width, height: run.height, concurrency: run.concurrency, key });
    }
  }
  return [...seen.values()].sort(
    (a, b) => a.width - b.width || a.height - b.height || a.concurrency - b.concurrency,
  );
}

/** Distinct config keys across a snapshot array (their nested runs), ordered. */
function configsOf(snapshots) {
  return configsOfRuns(snapshots.flatMap((snap) => snap.runs ?? []));
}

/**
 * Build history-trend points for one config and one metric. `runIndex` is
 * the snapshot's position in the timeline (the #20 alignment axis), so a
 * config absent from some snapshots simply has no point at those indices.
 */
function historyPointsFor(snapshots, config, valueOf) {
  const points = [];
  snapshots.forEach((snap, runIndex) => {
    for (const run of snap.runs ?? []) {
      if (run.width !== config.width || run.height !== config.height || run.concurrency !== config.concurrency) {
        continue;
      }
      const value = valueOf(run);
      if (!Number.isFinite(value)) continue;
      points.push({ runIndex, version: snap.version ?? `r${runIndex}`, engine: run.engine, value });
    }
  });
  return points;
}

/** Human thread-budget label matching the removed Rust emitter. */
function threadCap(conc) {
  if (conc === 0) return 'auto threads';
  return `${conc} thread${conc === 1 ? '' : 's'}`;
}

/* -------------------------------------------------------------------------- */
/* Chart builders (pure — { filename, svg } records)                          */
/* -------------------------------------------------------------------------- */

/**
 * History-trend SVGs from a benchmark_history snapshot array: a wall-time and
 * a peak-RSS chart per (w×h, concurrency) config, but only where a trend
 * exists (data in >= 2 snapshots).
 */
export function buildHistoryCharts(snapshots) {
  const out = [];
  if (!Array.isArray(snapshots) || snapshots.length < 2) return out;
  // Both trended metrics are lower-is-better; the title states it so a downward
  // trend line reads unambiguously as an improvement.
  const metrics = [
    { suffix: 'time', label: 'Wall Time', direction: 'lower is better', unitSuffix: 'ms', valueOf: (r) => durationMs(r.wall_time) },
    {
      suffix: 'memory',
      label: 'Peak RSS',
      direction: 'lower is better',
      unitSuffix: 'MB',
      // `peak_rss_bytes` is `#[serde(default)] = 0`, where 0 means UNKNOWN
      // (legacy history predating the field), not a real zero-memory run.
      // Return NaN for 0/absent so the point is skipped and the gap logic
      // breaks the line rather than drawing a misleading flat-zero trend.
      valueOf: (r) => (r.peak_rss_bytes > 0 ? r.peak_rss_bytes / BYTES_PER_MB : Number.NaN),
    },
  ];
  for (const config of configsOf(snapshots)) {
    for (const metric of metrics) {
      const points = historyPointsFor(snapshots, config, metric.valueOf);
      const runs = new Set(points.map((p) => p.runIndex));
      if (runs.size < 2) continue; // need >= 2 snapshots for a trend
      const svg = renderHistoryTrend(points, {
        title: `${metric.label} History — ${config.width}x${config.height} c${config.concurrency} (${metric.direction})`,
        unitSuffix: metric.unitSuffix,
      });
      out.push({ filename: `chart_history_${config.key}_${metric.suffix}.svg`, svg });
    }
  }
  return out;
}

/**
 * Grouped-bar COMPARISON SVGs from a benchmark_results snapshot (a flat
 * `Vec<RunMetrics>`): one chart per metric — wall time, peak RSS, engine-tracked
 * working set, throughput, memory efficiency, resource cost — each a column
 * group per config with a bar per engine. This is the JS replacement for the
 * Rust plotters `generate_charts` chart_*.svg emitters (#42), reaching full
 * parity with the SIX charts it drew; the metric extractors mirror the Rust
 * `RunMetrics` methods so the numbers match the retired charts. Wall time and
 * peak RSS also carry the 95%-CI whiskers the Rust charts drew, sourced from
 * `RunStats.wall_ms_ci95 / rss_mb_ci95` (the ratio metrics have no CI, as in
 * the Rust emitter).
 */
export function buildComparisonCharts(results) {
  const out = [];
  if (!Array.isArray(results) || results.length === 0) return out;
  // `errorOf` is the 95%-CI half-width for the two metrics the Rust charts
  // whiskered; the ratio metrics leave it undefined (no whisker).
  const metrics = [
    { suffix: 'wall_time', render: renderWallTimeBars, valueOf: runWallTimeMs, errorOf: (r) => r.stats?.wall_ms_ci95 },
    { suffix: 'peak_memory', render: renderPeakMemoryBars, valueOf: runPeakRssMb, errorOf: (r) => r.stats?.rss_mb_ci95 },
    { suffix: 'tracked_memory', render: renderTrackedMemoryBars, valueOf: runTrackedMemoryMb },
    { suffix: 'throughput', render: renderThroughputBars, valueOf: runTilesPerSecond },
    { suffix: 'efficiency', render: renderEfficiencyBars, valueOf: runTilesPerSecondPerMb },
    { suffix: 'resource_cost', render: renderResourceCostBars, valueOf: runResourceCostPerTile },
  ];
  // Config groups run along the x-axis in the shared numeric config order.
  // Bucket the runs by config ONCE (not once per metric) so the row assembly is
  // a single pass over each config's members instead of re-scanning all runs.
  const configs = configsOfRuns(results);
  const byConfig = new Map(configs.map((c) => [c.key, []]));
  for (const run of results) {
    byConfig.get(`${run.width}x${run.height}_c${run.concurrency}`)?.push(run);
  }
  for (const metric of metrics) {
    const rows = [];
    for (const config of configs) {
      for (const run of byConfig.get(config.key)) {
        const row = { config: config.key, engine: run.engine, value: metric.valueOf(run) };
        const err = metric.errorOf?.(run);
        if (Number.isFinite(err) && err > 0) row.error = err;
        rows.push(row);
      }
    }
    out.push({ filename: `chart_${metric.suffix}.svg`, svg: metric.render(rows) });
  }
  return out;
}

/**
 * Scalability SVGs from a scalability_results point array: five metric charts
 * (wall time, peak RSS, throughput, efficiency, resource cost) per distinct
 * thread budget. Log-log by default; `linear` selects the linear axes. A finite
 * `xMin` zooms into the large-image regime (>= xMin MP) and tags the output
 * filenames with a `_zoom` suffix so it never overwrites the full-range set
 * (#43).
 */
export function buildScalabilityCharts(points, { linear = false, xMin = null } = {}) {
  const out = [];
  if (!Array.isArray(points) || points.length === 0) return out;
  // Each title names the metric's unit and its better-direction (higher/lower),
  // matching the grouped-bar comparison charts, so a reader never has to guess
  // whether a rising line is good or bad. The primary throughput metric is
  // TILES/s (pyramid tiles), never pixels/s.
  const metrics = [
    { suffix: 'wall_time', title: 'Wall Time Scalability (lower is better)', yLabel: 'Time (ms)', unitSuffix: 'ms', valueOf: (p) => p.wall_time_ms },
    // `peak_rss_mb ?? peak_memory_mb` mirrors the Rust
    // `#[serde(alias = "peak_memory_mb")]` so pre-#153 scalability JSON that
    // still uses the old field name is read, not silently dropped.
    { suffix: 'peak_memory', title: 'Peak RSS Scalability (lower is better)', yLabel: 'Peak RSS (MB)', unitSuffix: 'MB', valueOf: (p) => p.peak_rss_mb ?? p.peak_memory_mb },
    { suffix: 'throughput', title: 'Throughput Scalability — Tiles/s (higher is better)', yLabel: 'Tiles/s', unitSuffix: '', valueOf: (p) => p.tiles_per_second },
    { suffix: 'efficiency', title: 'Memory Efficiency — Tiles/s per RSS-MB (higher is better)', yLabel: 'Tiles/s/RSS-MB', unitSuffix: '', valueOf: (p) => p.tiles_per_second_per_mb },
    { suffix: 'resource_cost', title: 'Resource Cost — RSS-MB·s per Tile (lower is better)', yLabel: 'RSS-MB·s/tile', unitSuffix: '', valueOf: (p) => p.resource_cost },
  ];
  const zoomed = Number.isFinite(xMin);
  const suffix = zoomed ? '_zoom' : '';
  const concs = [...new Set(points.map((p) => p.concurrency ?? 0))].sort((a, b) => a - b);
  for (const conc of concs) {
    const subset = points.filter((p) => (p.concurrency ?? 0) === conc);
    for (const metric of metrics) {
      const chartPoints = subset.map((p) => ({
        engine: p.engine,
        megapixels: p.megapixels,
        value: metric.valueOf(p),
      }));
      const svg = renderScalabilityChart(chartPoints, {
        title: `${metric.title} — synthetic gradient (${threadCap(conc)}${zoomed ? `, >= ${xMin} MP` : ''})`,
        xLabel: 'Image size (megapixels)',
        yLabel: metric.yLabel,
        unitSuffix: metric.unitSuffix,
        logScale: !linear,
        xMin: zoomed ? xMin : undefined,
      });
      out.push({ filename: `scalability_${metric.suffix}_c${conc}${suffix}.svg`, svg });
    }
  }
  return out;
}

/* -------------------------------------------------------------------------- */
/* Disk orchestration                                                         */
/* -------------------------------------------------------------------------- */

function readJson(path) {
  if (!path || !existsSync(path)) return null;
  try {
    return JSON.parse(readFileSync(path, 'utf8'));
  } catch (e) {
    process.stderr.write(`render.mjs: skipping unreadable ${path}: ${e.message}\n`);
    return null;
  }
}

/**
 * Probe the load-bearing fields the history extractors read. Returns `true`
 * for absent/empty data (that is not a shape signal) and for well-shaped
 * runs; `false` only when non-empty records are present but a run lacks the
 * fields we depend on — i.e. the Rust `RunMetrics` shape drifted from what
 * this consumer reads. Used to tell "field-shape mismatch" apart from
 * "legitimately no charts yet" (a single snapshot has no trend).
 */
export function historyShapeOk(data) {
  if (data == null) return true; // absent — not a shape signal
  if (!Array.isArray(data)) return false; // present but not the JSON array serde emits → drift
  if (data.length === 0) return true; // empty — not a shape signal
  const run = data.find((s) => Array.isArray(s?.runs) && s.runs.length > 0)?.runs?.[0];
  if (!run) return true; // snapshots without runs — not a field-shape signal
  return 'engine' in run && ('wall_time' in run || 'peak_rss_bytes' in run) && 'width' in run;
}

/** As {@link historyShapeOk}, for the scalability `ScalabilityPoint` shape. */
export function scalabilityShapeOk(data) {
  if (data == null) return true; // absent — not a shape signal
  if (!Array.isArray(data)) return false; // present but not a JSON array → drift
  if (data.length === 0) return true; // empty — not a shape signal
  const p = data[0];
  return !!p && 'engine' in p && 'megapixels' in p && 'wall_time_ms' in p;
}

/**
 * As {@link historyShapeOk}, for the grouped-bar `RunMetrics` shape read out of
 * benchmark_results.json. Checks the grouping keys, the metric-source fields,
 * AND the nested Duration `{ secs, nanos }` — so a renamed/removed field
 * (top-level or inside the Duration) FAILS LOUD (`false`) rather than silently
 * charting `n/a`/zero. This is the JS CONSUMER half of the #44 producer/consumer
 * drift guard (the producer half lives in tests/chart_shape_drift.rs).
 */
export function resultsShapeOk(data) {
  if (data == null) return true; // absent — not a shape signal
  if (!Array.isArray(data)) return false; // present but not the JSON array serde emits → drift
  if (data.length === 0) return true; // empty — not a shape signal
  const r = data[0];
  if (!r || typeof r !== 'object') return false;
  const wt = r.wall_time;
  return (
    'engine' in r &&
    'width' in r &&
    'height' in r &&
    'concurrency' in r &&
    'peak_rss_bytes' in r &&
    'tiles_produced' in r &&
    !!wt &&
    typeof wt === 'object' &&
    'secs' in wt &&
    'nanos' in wt
  );
}

/**
 * The three input JSON paths + the report dir, defaulted off `reportDir`. One
 * place so `renderAll()` and `main()` can never disagree on a default filename
 * (they consumed the same three joins independently before).
 */
function resolveInputPaths(opts = {}) {
  const reportDir = opts.reportDir ?? DEFAULT_REPORT_DIR;
  return {
    reportDir,
    historyPath: opts.historyPath ?? join(reportDir, 'benchmark_history.json'),
    resultsPath: opts.resultsPath ?? join(reportDir, 'benchmark_results.json'),
    scalabilityPath: opts.scalabilityPath ?? join(reportDir, 'scalability_results.json'),
  };
}

/**
 * Read whichever JSON files are present and write their SVGs to `outDir`.
 * Returns the absolute paths written, sorted.
 *
 * @param {{reportDir?:string, outDir?:string, historyPath?:string, resultsPath?:string, scalabilityPath?:string, linear?:boolean, zoom?:number}} opts
 */
export function renderAll(opts = {}) {
  const { reportDir, historyPath, resultsPath, scalabilityPath } = resolveInputPaths(opts);
  const outDir = opts.outDir ?? reportDir;

  const scalability = readJson(scalabilityPath) ?? [];
  const linear = opts.linear ?? false;
  const charts = [
    ...buildHistoryCharts(readJson(historyPath) ?? []),
    ...buildComparisonCharts(readJson(resultsPath) ?? []),
    ...buildScalabilityCharts(scalability, { linear }),
  ];
  // #43: an opt-in --zoom adds large-image `_zoom` scalability variants ON TOP
  // of the default full-range charts (which stay the default view). The
  // user-facing `--zoom`/`opts.zoom` verb maps here to the more precise `xMin`
  // axis floor (minimum megapixels) that buildScalabilityCharts /
  // renderScalabilityChart speak — one concept, deliberately renamed at this
  // boundary from the CLI's action word to the axis parameter it drives.
  if (Number.isFinite(opts.zoom)) {
    charts.push(...buildScalabilityCharts(scalability, { linear, xMin: opts.zoom }));
  }
  if (charts.length === 0) return [];

  mkdirSync(outDir, { recursive: true });
  const written = [];
  for (const { filename, svg } of charts) {
    const full = join(outDir, filename);
    writeFileSync(full, svg);
    written.push(full);
  }
  return written.sort();
}

/* -------------------------------------------------------------------------- */
/* CLI                                                                        */
/* -------------------------------------------------------------------------- */

const HELP = `Usage: render.mjs [options]

Renders the benchmark SVGs from the harness JSON in the report dir:
  benchmark_history.json   (Vec<BenchmarkSnapshot>)  -> chart_history_*.svg
  benchmark_results.json   (Vec<RunMetrics>)         -> chart_*.svg (grouped bars)
  scalability_results.json (Vec<ScalabilityPoint>)   -> scalability_*.svg

Options:
  --report-dir DIR    Dir holding the input JSON and receiving the SVGs
                      (default: ../../report relative to tools/charts).
  --out-dir DIR       Write SVGs here instead of the report dir.
  --history FILE      Override the history JSON path.
  --results FILE      Override the results JSON path (grouped-bar charts).
  --scalability FILE  Override the scalability JSON path.
  --linear            Linear scalability axes (default: log-log).
  --zoom <minMP>      ALSO emit large-image scalability_*_zoom.svg restricted
                      to sizes >= <minMP> megapixels (full-range stays default).
  -h, --help          Show this help.

Missing input files are skipped, not fatal. Output is deterministic.
`;

function parseArgs(argv) {
  const opts = {};
  // A value-taking flag must be followed by a real value, not the next flag
  // or end-of-args — otherwise `--report-dir --linear` would silently swallow
  // the flag as the path and fall back to the default dir.
  const takeValue = (flag, i) => {
    const v = argv[i + 1];
    if (v === undefined || v.startsWith('-')) {
      process.stderr.write(`render.mjs: ${flag} needs a value\n`);
      process.exit(2);
    }
    return v;
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    switch (a) {
      case '--report-dir': opts.reportDir = takeValue(a, i); i++; break;
      case '--out-dir': opts.outDir = takeValue(a, i); i++; break;
      case '--history': opts.historyPath = takeValue(a, i); i++; break;
      case '--results': opts.resultsPath = takeValue(a, i); i++; break;
      case '--scalability': opts.scalabilityPath = takeValue(a, i); i++; break;
      case '--linear': opts.linear = true; break;
      case '--zoom': {
        const v = takeValue(a, i);
        i++;
        const n = Number(v);
        if (!Number.isFinite(n) || n <= 0) {
          process.stderr.write('render.mjs: --zoom needs a positive megapixel value\n');
          process.exit(2);
        }
        opts.zoom = n;
        break;
      }
      case '-h':
      case '--help':
        process.stdout.write(HELP);
        process.exit(0);
        break;
      default:
        process.stderr.write(`render.mjs: unknown argument ${a}\n`);
        process.exit(2);
    }
  }
  return opts;
}

function main() {
  const opts = parseArgs(process.argv.slice(2));
  const { historyPath, resultsPath, scalabilityPath } = resolveInputPaths(opts);

  // Shape-drift gate — runs on EVERY present input, BEFORE any chart is written.
  // This is deliberately not gated on "0 charts produced": a PARTIAL field
  // rename (e.g. peak_rss_bytes dropped while wall_time survives) still renders
  // some charts, so a 0-chart gate would never fire and the peak_memory /
  // efficiency / resource_cost charts would silently draw all-zero bars. Any
  // present, parseable input whose record shape the extractors don't recognise
  // is producer/consumer drift: fail loud (exit non-zero) so run-bench.sh
  // surfaces it, and skip rendering the misleading zero charts entirely.
  const drift = [];
  if (existsSync(historyPath) && !historyShapeOk(readJson(historyPath))) drift.push(historyPath);
  if (existsSync(resultsPath) && !resultsShapeOk(readJson(resultsPath))) drift.push(resultsPath);
  if (existsSync(scalabilityPath) && !scalabilityShapeOk(readJson(scalabilityPath))) {
    drift.push(scalabilityPath);
  }
  if (drift.length > 0) {
    process.stderr.write(
      `render.mjs: input present but its record shape does not match the chart extractors — ` +
        `producer/consumer field-shape drift between the Rust harness JSON and render.mjs. ` +
        `Not rendering (would silently chart n/a/zero). Check: ${drift.join(', ')}\n`,
    );
    process.exit(1);
  }

  const written = renderAll(opts);
  if (written.length > 0) {
    for (const p of written) process.stdout.write(`wrote ${p}\n`);
    process.stdout.write(`render.mjs: ${written.length} chart(s) written.\n`);
    return;
  }

  // Nothing written and no drift. Tell the two benign cases apart:
  //   (1) no input files → nothing to do;
  //   (2) inputs present + well-shaped but too little data (e.g. a single
  //       snapshot has no trend) → informational, exit 0.
  const present = [historyPath, resultsPath, scalabilityPath].filter((p) => existsSync(p));
  if (present.length === 0) {
    process.stdout.write('render.mjs: no benchmark JSON found — nothing to render.\n');
    return;
  }
  process.stdout.write(
    'render.mjs: inputs present but produced no charts ' +
      '(a history trend needs >= 2 snapshots; grouped bars need >= 1 run; scalability needs >= 1 point).\n',
  );
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main();
}
