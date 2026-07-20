#!/usr/bin/env node
/**
 * render.mjs — turn the libviprs benchmark harness JSON into the SVG files
 * the report/article embed. Consumes the JSON the Rust harness already
 * writes (nothing else changed on the data path):
 *
 *   report/benchmark_history.json   (Vec<BenchmarkSnapshot>)  → chart_history_*.svg
 *   report/scalability_results.json (Vec<ScalabilityPoint>)   → scalability_*.svg
 *
 * The SVG generation used to live in the Rust `report` / `scalability`
 * binaries (plotters); it now lives here, reusing the proven causl-bench
 * chart code (see chart.mjs). `run-bench.sh` invokes this after the harness
 * produces the JSON, so charts regenerate on every run.
 *
 * Output is deterministic (no timestamps, no rng) — the same JSON always
 * yields byte-identical SVGs. Missing input is skipped, not fatal: a single
 * bench invocation writes only one of the two JSON files.
 *
 * Usage:
 *   node tools/charts/render.mjs [--report-dir DIR] [--out-dir DIR]
 *                                [--history FILE] [--scalability FILE]
 *                                [--linear]
 */

import { readFileSync, writeFileSync, existsSync, mkdirSync } from 'node:fs';
import { join, dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { renderHistoryTrend, renderScalabilityChart } from './chart.mjs';

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

/** Distinct config keys `{w}x{h}_c{conc}`, deterministically ordered. */
function configsOf(snapshots) {
  const seen = new Map();
  for (const snap of snapshots) {
    for (const run of snap.runs ?? []) {
      const key = `${run.width}x${run.height}_c${run.concurrency}`;
      if (!seen.has(key)) {
        seen.set(key, { width: run.width, height: run.height, concurrency: run.concurrency, key });
      }
    }
  }
  return [...seen.values()].sort(
    (a, b) => a.width - b.width || a.height - b.height || a.concurrency - b.concurrency,
  );
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
  const metrics = [
    { suffix: 'time', label: 'Wall Time', unitSuffix: 'ms', valueOf: (r) => durationMs(r.wall_time) },
    {
      suffix: 'memory',
      label: 'Peak RSS',
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
        title: `${metric.label} History — ${config.width}x${config.height} c${config.concurrency}`,
        unitSuffix: metric.unitSuffix,
      });
      out.push({ filename: `chart_history_${config.key}_${metric.suffix}.svg`, svg });
    }
  }
  return out;
}

/**
 * Scalability SVGs from a scalability_results point array: five metric charts
 * (wall time, peak RSS, throughput, efficiency, resource cost) per distinct
 * thread budget. Log-log by default; `linear` selects the linear axes.
 */
export function buildScalabilityCharts(points, { linear = false } = {}) {
  const out = [];
  if (!Array.isArray(points) || points.length === 0) return out;
  const metrics = [
    { suffix: 'wall_time', title: 'Wall Time Scalability', yLabel: 'Time (ms)', unitSuffix: 'ms', valueOf: (p) => p.wall_time_ms },
    // `peak_rss_mb ?? peak_memory_mb` mirrors the Rust
    // `#[serde(alias = "peak_memory_mb")]` so pre-#153 scalability JSON that
    // still uses the old field name is read, not silently dropped.
    { suffix: 'peak_memory', title: 'Peak RSS Scalability', yLabel: 'Peak RSS (MB)', unitSuffix: 'MB', valueOf: (p) => p.peak_rss_mb ?? p.peak_memory_mb },
    { suffix: 'throughput', title: 'Throughput Scalability', yLabel: 'Tiles/s', unitSuffix: '', valueOf: (p) => p.tiles_per_second },
    { suffix: 'efficiency', title: 'Memory Efficiency — Tiles/s per RSS-MB', yLabel: 'Tiles/s/RSS-MB', unitSuffix: '', valueOf: (p) => p.tiles_per_second_per_mb },
    { suffix: 'resource_cost', title: 'Resource Cost — RSS-MB·s per Tile', yLabel: 'RSS-MB·s/tile', unitSuffix: '', valueOf: (p) => p.resource_cost },
  ];
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
        title: `${metric.title} — synthetic gradient (${threadCap(conc)})`,
        xLabel: 'Image size (megapixels)',
        yLabel: metric.yLabel,
        unitSuffix: metric.unitSuffix,
        logScale: !linear,
      });
      out.push({ filename: `scalability_${metric.suffix}_c${conc}.svg`, svg });
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
  if (!Array.isArray(data) || data.length === 0) return true;
  const run = data.find((s) => Array.isArray(s?.runs) && s.runs.length > 0)?.runs?.[0];
  if (!run) return true; // snapshots without runs — not a field-shape signal
  return 'engine' in run && ('wall_time' in run || 'peak_rss_bytes' in run) && 'width' in run;
}

/** As {@link historyShapeOk}, for the scalability `ScalabilityPoint` shape. */
export function scalabilityShapeOk(data) {
  if (!Array.isArray(data) || data.length === 0) return true;
  const p = data[0];
  return !!p && 'engine' in p && 'megapixels' in p && 'wall_time_ms' in p;
}

/**
 * Read whichever JSON files are present and write their SVGs to `outDir`.
 * Returns the absolute paths written, sorted.
 *
 * @param {{reportDir?:string, outDir?:string, historyPath?:string, scalabilityPath?:string, linear?:boolean}} opts
 */
export function renderAll(opts = {}) {
  const reportDir = opts.reportDir ?? DEFAULT_REPORT_DIR;
  const outDir = opts.outDir ?? reportDir;
  const historyPath = opts.historyPath ?? join(reportDir, 'benchmark_history.json');
  const scalabilityPath = opts.scalabilityPath ?? join(reportDir, 'scalability_results.json');

  const charts = [
    ...buildHistoryCharts(readJson(historyPath) ?? []),
    ...buildScalabilityCharts(readJson(scalabilityPath) ?? [], { linear: opts.linear ?? false }),
  ];
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
  scalability_results.json (Vec<ScalabilityPoint>)   -> scalability_*.svg

Options:
  --report-dir DIR    Dir holding the input JSON and receiving the SVGs
                      (default: ../../report relative to tools/charts).
  --out-dir DIR       Write SVGs here instead of the report dir.
  --history FILE      Override the history JSON path.
  --scalability FILE  Override the scalability JSON path.
  --linear            Linear scalability axes (default: log-log).
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
      case '--scalability': opts.scalabilityPath = takeValue(a, i); i++; break;
      case '--linear': opts.linear = true; break;
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
  const reportDir = opts.reportDir ?? DEFAULT_REPORT_DIR;
  const historyPath = opts.historyPath ?? join(reportDir, 'benchmark_history.json');
  const scalabilityPath = opts.scalabilityPath ?? join(reportDir, 'scalability_results.json');

  const written = renderAll(opts);
  if (written.length > 0) {
    for (const p of written) process.stdout.write(`wrote ${p}\n`);
    process.stdout.write(`render.mjs: ${written.length} chart(s) written.\n`);
    return;
  }

  // Nothing written. Tell three cases apart:
  //   (1) no input files          → benign, nothing to do
  //   (2) inputs present, but a record shape we don't recognise → LOUD:
  //       likely producer/consumer field-shape drift; exit non-zero so
  //       run-bench.sh surfaces it.
  //   (3) inputs present + well-shaped but too little data (e.g. a single
  //       snapshot has no trend) → informational, exit 0.
  const present = [historyPath, scalabilityPath].filter((p) => existsSync(p));
  if (present.length === 0) {
    process.stdout.write('render.mjs: no benchmark JSON found — nothing to render.\n');
    return;
  }
  const drift = [];
  if (existsSync(historyPath) && !historyShapeOk(readJson(historyPath))) drift.push(historyPath);
  if (existsSync(scalabilityPath) && !scalabilityShapeOk(readJson(scalabilityPath))) {
    drift.push(scalabilityPath);
  }
  if (drift.length > 0) {
    process.stderr.write(
      `render.mjs: parsed input but produced 0 charts — field-shape mismatch between ` +
        `the Rust harness JSON and the chart extractors? Check: ${drift.join(', ')}\n`,
    );
    process.exit(1);
  }
  process.stdout.write(
    'render.mjs: inputs present but produced no charts ' +
      '(a history trend needs >= 2 snapshots; scalability needs >= 1 point).\n',
  );
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main();
}
