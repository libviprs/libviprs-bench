/**
 * SVG chart renderers for the libviprs benchmark report — the JS port of
 * causl-bench's `packages/bench/src/chart.ts` (the proven, article-grade SVG
 * code), adapted to libviprs's engines and JSON shapes.
 *
 * Ported pieces (same structure, helpers, and rendering approach as the
 * original — not a fresh reimplementation):
 *   - {@link renderHistoryTrend} from causl's `renderHistoryTrend`: a
 *     per-version trend whose x is the SNAPSHOT INDEX in the history
 *     timeline (a `versionByRun` map drives the x-axis tick labels). This is
 *     exactly the #20 alignment fix: a point's x comes from WHICH snapshot it
 *     belongs to, shared across engines, so a missing/late engine never
 *     shifts the others. The libviprs enhancement over the causl original:
 *     when an engine is MISSING from a snapshot that its peers occupy the
 *     polyline BREAKS (a gap) rather than drawing a straight line across the
 *     hole (#28/#29).
 *   - {@link renderScalabilityChart} from causl's `renderScalabilityCell` /
 *     `renderScalabilitySweep` (which already log-scaled x via `Math.log10`),
 *     extended to true LOG-LOG axes — log10 on BOTH the megapixel x-axis and
 *     the metric y-axis — with the log domain snapped to enclosing decades so
 *     the endpoints land on labeled decade ticks (#21/#34). A LINEAR mode is
 *     kept behind `opts.logScale = false`, and an optional `opts.xMin` zooms
 *     into the large-image regime (#43, the opt-in replacement for the removed
 *     Rust `--crop`).
 *   - {@link renderMetricGroupedBars} (+ {@link renderWallTimeBars} /
 *     {@link renderPeakMemoryBars} / {@link renderTrackedMemoryBars} /
 *     {@link renderThroughputBars} / {@link renderEfficiencyBars} /
 *     {@link renderResourceCostBars}) from causl's `renderMetricGroupedBars`
 *     family: one column group per benchmark config, one bar per engine, driven
 *     from benchmark_results.json — the port that retires the Rust plotters
 *     `generate_charts` (#42). All SIX charts the Rust emitter produced are
 *     ported (wall time, peak RSS, engine-tracked working set, throughput,
 *     efficiency, resource cost); wall time and peak RSS carry the 95%-CI
 *     whiskers the Rust charts drew, from `RunStats.wall_ms_ci95 / rss_mb_ci95`.
 *   - the shared helpers: {@link COLORS}, {@link ENGINE_ORDER},
 *     {@link formatNumber}, {@link svgPlaceholder}, and the SVG scaffolding /
 *     font conventions, so both libviprs charts read like the causl-bench
 *     article.
 *
 * Cross-stack note: the grouped-bar `chart_*.svg` comparison charts are now
 * rendered here too ({@link renderMetricGroupedBars} + its per-metric
 * wrappers), completing the JS migration and retiring the Rust plotters
 * `generate_charts` (#42). The engine palette below is the hex equivalent of
 * the RGB values those plotters emitters used, so the JS charts stay
 * colour-identical to the prior report artifacts.
 *
 * Determinism (the article's CI-snapshot contract): every renderer is a pure
 * function of its input — canonical {@link ENGINE_ORDER} iteration (never a
 * hash-map order), all number formatting through {@link formatNumber} /
 * {@link formatLogTick} / {@link fmtCoord} (locale-independent), no
 * timestamps, no random ids.
 */

/**
 * Single source of truth for engine identity: canonical draw order, stroke
 * colour, and the title-cased display label. {@link ENGINE_ORDER},
 * {@link COLORS}, and {@link ENGINE_LABELS} are all DERIVED from this so the
 * three can never drift apart. The libvips oracle leads, then the three
 * libviprs engines in pipeline order (monolithic → streaming → mapreduce),
 * mirroring the draw order the removed Rust plotters emitters used.
 *
 * Colours are the hex equivalents of the Rust `RGBColor` palette
 * (COLOR_VIPS / COLOR_MONO / COLOR_STREAM / COLOR_MR) so the JS charts are
 * colour-identical to the prior report artifacts.
 */
const ENGINES = [
  { key: 'libvips', label: 'libvips', color: '#9c27b0' }, // RGB(156, 39, 176) — purple
  { key: 'monolithic', label: 'Monolithic', color: '#4285f4' }, // RGB(66, 133, 244) — blue
  { key: 'streaming', label: 'Streaming', color: '#34a853' }, // RGB(52, 168, 83) — green
  { key: 'mapreduce', label: 'MapReduce', color: '#ea4335' }, // RGB(234, 67, 53) — red
];

/** Canonical engine order — every renderer iterates this for stable output. */
export const ENGINE_ORDER = Object.freeze(ENGINES.map((e) => e.key));

/** Engine → stroke colour (frozen so the palette contract can't be mutated). */
export const COLORS = Object.freeze(Object.fromEntries(ENGINES.map((e) => [e.key, e.color])));

/**
 * Engine → title-cased display label, wired into both legends so the series
 * names match the prior Rust artifacts ('Monolithic'/'Streaming'/'MapReduce').
 */
export const ENGINE_LABELS = Object.freeze(Object.fromEntries(ENGINES.map((e) => [e.key, e.label])));

/** Set membership test for the canonical engines (drives the fallback path). */
const CANONICAL = new Set(ENGINE_ORDER);

/**
 * Deterministic fallback strokes for engines that appear in the JSON but are
 * not canonical (e.g. a future engine). Assigned by the engine's sorted
 * position so output stays byte-stable.
 */
const FALLBACK_COLORS = ['#607d8b', '#795548', '#5c6bc0', '#00838f', '#c2185b', '#558b2f'];

/**
 * Canonical engines first, then any non-canonical engine SEEN IN THE DATA in
 * sorted order. Renderers iterate this (not {@link ENGINE_ORDER} directly) so
 * an engine present in the JSON but missing from the canonical list is drawn
 * with a fallback colour rather than silently dropped.
 */
function orderedEngines(points) {
  const present = new Set(points.map((p) => p.engine));
  const extras = [...present].filter((e) => !CANONICAL.has(e)).sort();
  return [...ENGINE_ORDER, ...extras];
}

/** Stroke colour for an engine, falling back for non-canonical engines. */
function colorFor(engine, ordered) {
  if (COLORS[engine]) return COLORS[engine];
  const extraIndex = ordered.indexOf(engine) - ENGINE_ORDER.length;
  const i = extraIndex < 0 ? 0 : extraIndex % FALLBACK_COLORS.length;
  return FALLBACK_COLORS[i];
}

/** Display label for an engine (title-cased for canonical, raw key otherwise). */
function labelFor(engine) {
  return ENGINE_LABELS[engine] ?? engine;
}

/**
 * XML-escape interpolated TEXT (titles, versions, engine names, axis labels)
 * so a value containing `<`, `>`, `&`, or `"` never produces malformed SVG.
 * Not applied to numeric coordinates (they go through {@link fmtCoord}).
 */
function escapeXml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

/** Reduce-based min/max — avoids `Math.min(...arr)` stack overflow on huge arrays. */
function minOf(arr, seed = Infinity) {
  let m = seed;
  for (const v of arr) if (v < m) m = v;
  return m;
}
function maxOf(arr, seed = -Infinity) {
  let m = seed;
  for (const v of arr) if (v > m) m = v;
  return m;
}

/**
 * Locale-stable number formatter for DATA LABELS (no Intl, no NaN leakage).
 * Ported verbatim from causl-bench: `n/a` for non-finite, integers for
 * magnitudes >= 100, otherwise up to two trimmed decimals.
 */
export function formatNumber(n) {
  if (!Number.isFinite(n)) return 'n/a';
  if (Math.abs(n) >= 100) return Math.round(n).toString();
  return Number.parseFloat(n.toFixed(2)).toString();
}

/**
 * Label formatter for LOG-axis decade ticks. Unlike {@link formatNumber}
 * (which rounds |v| < 100 to two decimals and would collapse 0.001 and 0.0001
 * both to '0', colliding two distinct decades), this preserves small and
 * large magnitudes — compact scientific (`1e-3`, `1e5`) outside
 * `[0.01, 1e5)`, a trimmed decimal within.
 */
export function formatLogTick(v) {
  if (!Number.isFinite(v)) return 'n/a';
  if (v === 0) return '0';
  const abs = Math.abs(v);
  if (abs < 0.01 || abs >= 1e5) return v.toExponential(0).replace('e+', 'e');
  return Number.parseFloat(v.toFixed(4)).toString();
}

/**
 * Coordinate formatter for SVG GEOMETRY. Unlike {@link formatNumber} (which
 * rounds >= 100 to integers, fine for labels) this keeps sub-pixel precision
 * so log-spaced decade ticks land at exactly-equal pixel spacing. Still
 * locale-independent and deterministic; non-finite coordinates collapse to
 * `0` rather than leaking `NaN`.
 */
function fmtCoord(n) {
  if (!Number.isFinite(n)) return '0';
  return Number.parseFloat(n.toFixed(5)).toString();
}

/** Empty-state SVG carrying the chart's label. Ported from causl-bench. */
export function svgPlaceholder(width, height, label) {
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${height}" font-family="ui-sans-serif"><text x="${fmtCoord(width / 2)}" y="${fmtCoord(height / 2)}" text-anchor="middle" font-size="12" fill="#999">no data — ${escapeXml(label)}</text></svg>`;
}

/* -------------------------------------------------------------------------- */
/* Axis scales + log-spaced ticks (#21 / #34).                                */
/* -------------------------------------------------------------------------- */

/**
 * A log10 axis: maps `[domainMin, domainMax]` (both > 0) onto
 * `[rangeMin, rangeMax]` in log space, so every decade occupies an EQUAL
 * pixel span. Returns a `(value) => pixel` function. A non-positive domain or
 * a zero-width domain collapses to `rangeMin` (never leaks `NaN`).
 */
export function log10Scale(domainMin, domainMax, rangeMin, rangeMax) {
  if (!(domainMin > 0) || !(domainMax > 0)) return () => rangeMin;
  const lo = Math.log10(domainMin);
  const hi = Math.log10(domainMax);
  const span = hi - lo;
  return (v) => {
    if (span === 0 || !(v > 0)) return rangeMin;
    return rangeMin + ((Math.log10(v) - lo) / span) * (rangeMax - rangeMin);
  };
}

/**
 * A linear axis (the preserved LINEAR mode). Maps `[domainMin, domainMax]`
 * proportionally onto `[rangeMin, rangeMax]`.
 */
export function linearScale(domainMin, domainMax, rangeMin, rangeMax) {
  const span = domainMax - domainMin;
  return (v) => {
    if (span === 0) return rangeMin;
    return rangeMin + ((v - domainMin) / span) * (rangeMax - rangeMin);
  };
}

/**
 * Snap `[min, max]` (both > 0) out to the enclosing powers of ten, e.g.
 * `[0.18, 11.8] → [0.1, 100]`. Driving both {@link log10Scale} and
 * {@link decadeTicks} off these bounds guarantees the data endpoints land on
 * LABELED decade ticks (never a bare sub-decade axis) and gives headroom so
 * the extreme dots aren't stranded on the frame edge — the conventional
 * "nice bounds" behaviour for a log axis. A single-value domain expands
 * symmetrically to one decade either side.
 */
export function enclosingDecades(min, max) {
  if (!(min > 0) || !(max > 0)) return [min, max];
  const lo = 10 ** Math.floor(Math.log10(min) + 1e-9);
  const hi = 10 ** Math.ceil(Math.log10(max) - 1e-9);
  if (hi <= lo) return [lo / 10, lo * 10];
  return [lo, hi];
}

/**
 * Log-spaced axis ticks over `[min, max]` (both > 0): `major` at each power
 * of ten within range, `minor` at the 2..9 multiples of each decade. The
 * `± epsilon` guards keep `Math.log10` float noise (e.g. `log10(0.1)` a hair
 * below −1) from spawning a phantom out-of-range decade.
 */
export function decadeTicks(min, max) {
  const major = [];
  const minor = [];
  if (!(min > 0) || !(max > 0)) return { major, minor };
  const dLo = Math.floor(Math.log10(min) + 1e-9);
  const dHi = Math.ceil(Math.log10(max) - 1e-9);
  const inRange = (v) => v >= min * (1 - 1e-9) && v <= max * (1 + 1e-9);
  for (let d = dLo; d <= dHi; d++) {
    const base = 10 ** d;
    if (inRange(base)) major.push(base);
    for (let k = 2; k <= 9; k++) {
      const v = k * base;
      if (inRange(v)) minor.push(v);
    }
  }
  return { major, minor };
}

/* -------------------------------------------------------------------------- */
/* #20 — history trend (per-version line chart, one polyline per engine).     */
/* -------------------------------------------------------------------------- */

/**
 * Split an engine's runIndex-sorted points into maximal runs of ADJACENT
 * snapshots, where adjacency is measured against `orderOf` — the position of
 * a runIndex within the snapshots THIS chart actually draws (its peers'
 * occupied indices), NOT `runIndex + 1` arithmetic. So the line breaks only
 * when an engine is missing at a snapshot its peers occupy (#28/#29), and a
 * snapshot that NO engine of this config occupies (a whole gap in the
 * timeline) does not spuriously shatter every engine into single points.
 */
function consecutiveSegments(series, orderOf) {
  const segments = [];
  let cur = [];
  let prevOrd = null;
  for (const p of series) {
    const ord = orderOf(p.runIndex);
    if (prevOrd !== null && ord !== prevOrd + 1) {
      segments.push(cur);
      cur = [];
    }
    cur.push(p);
    prevOrd = ord;
  }
  if (cur.length) segments.push(cur);
  return segments;
}

/**
 * Per-version trend chart for one (metric × config) tuple. `points` are
 * `{ runIndex, version, engine, value }` records — one per (engine × run).
 * The x-axis is the snapshot index (0..N-1) with the version as the tick
 * label; the y-axis is the metric value, one polyline per engine. A missing
 * engine at a snapshot breaks that engine's line (#20).
 *
 * @param {ReadonlyArray<{runIndex:number, version:string, engine:string, value:number}>} points
 * @param {{title?:string, unitSuffix?:string, width?:number, height?:number}} opts
 * @returns {string} deterministic SVG
 */
export function renderHistoryTrend(points, opts = {}) {
  const width = opts.width ?? 720;
  const height = opts.height ?? 260;
  const padding = 50;
  const title = opts.title ?? '';
  const unitSuffix = opts.unitSuffix ?? '';
  if (points.length === 0) return svgPlaceholder(width, height, title || 'history');

  const allRuns = [...new Set(points.map((p) => p.runIndex))].sort((a, b) => a - b);
  const versionByRun = new Map();
  for (const p of points) versionByRun.set(p.runIndex, p.version);
  // Position of each drawn snapshot in the timeline — the adjacency basis for
  // the gap logic (so a wholly-absent snapshot doesn't break every line).
  const runPos = new Map(allRuns.map((r, i) => [r, i]));
  const orderOf = (r) => runPos.get(r);
  const minRun = allRuns[0] ?? 0;
  const maxRun = allRuns[allRuns.length - 1] ?? 0;
  const maxV = maxOf(points.map((p) => p.value).filter(Number.isFinite), 0) || 1;

  const xFor = (r) => {
    if (maxRun === minRun) return padding;
    return padding + ((r - minRun) / (maxRun - minRun)) * (width - 2 * padding);
  };
  const yFor = (v) => height - padding - (v / maxV) * (height - 2 * padding);

  const ordered = orderedEngines(points);
  const lines = ordered
    .map((engine) => {
      const series = points
        .filter((p) => p.engine === engine && Number.isFinite(p.value))
        .sort((a, b) => a.runIndex - b.runIndex);
      if (series.length === 0) return '';
      const color = colorFor(engine, ordered);
      // One polyline per adjacent-snapshot segment → gaps break the line.
      const polylines = consecutiveSegments(series, orderOf)
        .map((seg) => {
          const pts = seg
            .map((p) => `${fmtCoord(xFor(p.runIndex))},${fmtCoord(yFor(p.value))}`)
            .join(' ');
          return `<polyline points="${pts}" fill="none" stroke="${color}" stroke-width="2"/>`;
        })
        .join('');
      // A dot per snapshot makes the alignment legible and keeps an isolated
      // (single-snapshot) point visible even when it forms no line.
      const dots = series
        .map(
          (p) =>
            `<circle cx="${fmtCoord(xFor(p.runIndex))}" cy="${fmtCoord(yFor(p.value))}" r="3" fill="${color}"/>`,
        )
        .join('');
      return polylines + dots;
    })
    .join('');

  const tickLabels = allRuns
    .map((r) => {
      const x = xFor(r);
      const y = height - padding + 14;
      const v = versionByRun.get(r) ?? `r${r}`;
      return `<text x="${fmtCoord(x)}" y="${fmtCoord(y)}" text-anchor="middle" font-size="9" fill="#666">${escapeXml(v)}</text>`;
    })
    .join('');

  // Legend: only engines actually present in this config's data (shared
  // convention with renderScalabilityChart), title-cased display labels.
  const legend = ordered
    .filter((engine) => points.some((p) => p.engine === engine))
    .map((engine, i) => {
      const x = padding + i * 150;
      return `<g><rect x="${fmtCoord(x)}" y="${fmtCoord(height - 16)}" width="10" height="10" fill="${colorFor(engine, ordered)}"/><text x="${fmtCoord(x + 14)}" y="${fmtCoord(height - 7)}" font-size="10" fill="#333">${escapeXml(labelFor(engine))}</text></g>`;
    })
    .join('');

  const yMaxText = `${formatNumber(maxV)}${escapeXml(unitSuffix)}`;
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${height}" font-family="ui-sans-serif"><text x="${fmtCoord(width / 2)}" y="20" text-anchor="middle" font-size="13" font-weight="600">${escapeXml(title)}</text><text x="${fmtCoord(padding - 4)}" y="${fmtCoord(padding + 4)}" text-anchor="end" font-size="9" fill="#666">${yMaxText}</text>${lines}${tickLabels}${legend}</svg>`;
}

/* -------------------------------------------------------------------------- */
/* #21 — scalability (megapixels × metric, one polyline per engine, log-log). */
/* -------------------------------------------------------------------------- */

/**
 * Scalability line chart: x = image size in megapixels, y = one metric, one
 * polyline per engine. Defaults to true LOG-LOG axes (log10 on both, domain
 * snapped to enclosing decades) with decade major ticks + 2..9 minor ticks;
 * `opts.logScale = false` selects the preserved linear mode (axes from 0, no
 * decade ticks).
 *
 * Named `renderScalabilityChart` (not causl's `renderScalabilitySweep`)
 * because libviprs emits one SVG per (metric × concurrency) — the
 * `scalability_<metric>_c<n>.svg` shape the old Rust produced — rather than
 * causl's composite small-multiples grid.
 *
 * In log mode only strictly-positive `(x, y)` points are plottable; a
 * non-positive / non-finite point BREAKS the polyline at that size (never
 * bridged, mirroring the history gap logic) and is counted in an "omitted"
 * annotation. An engine with input points but none plottable still keeps its
 * legend swatch so the reader can tell it was benchmarked.
 *
 * @param {ReadonlyArray<{engine:string, megapixels:number, value:number}>} points
 * @param {{title?:string, xLabel?:string, yLabel?:string, unitSuffix?:string, width?:number, height?:number, logScale?:boolean, xMin?:number}} opts
 * @returns {string} deterministic SVG
 */
export function renderScalabilityChart(points, opts = {}) {
  const width = opts.width ?? 700;
  const height = opts.height ?? 450;
  const padding = 64;
  const logScale = opts.logScale ?? true;
  const title = opts.title ?? '';
  const xLabel = opts.xLabel ?? '';
  const yLabel = opts.yLabel ?? '';
  const unitSuffix = opts.unitSuffix ?? '';

  if (points.length === 0) return svgPlaceholder(width, height, title || 'scalability');

  // #43 large-image zoom: an optional xMin (minimum megapixels) restricts the
  // chart to the large-image regime, re-scaling the axes to just the retained
  // points — the opt-in replacement for the removed Rust `--crop`. The default
  // (no xMin) keeps the full-range sweep, so the log-log full-range chart stays
  // the default view. Everything downstream operates on the windowed set, so
  // the domain snaps to the retained points and points below xMin simply drop
  // out of view (they are not counted as "omitted", which is reserved for the
  // <=0 / non-finite log-mode drops).
  const xMin = Number.isFinite(opts.xMin) ? opts.xMin : null;
  const windowed =
    xMin === null
      ? points
      : points.filter((p) => Number.isFinite(p.megapixels) && p.megapixels >= xMin);
  if (windowed.length === 0) return svgPlaceholder(width, height, title || 'scalability');

  // A point is plottable when both coordinates are finite and (in log mode)
  // strictly positive. Non-plottable points break the line, never bridge it.
  const plottable = (p) =>
    Number.isFinite(p.megapixels) &&
    Number.isFinite(p.value) &&
    (!logScale || (p.megapixels > 0 && p.value > 0));
  const usable = windowed.filter(plottable);
  if (usable.length === 0) return svgPlaceholder(width, height, title || 'scalability');

  const xs = usable.map((p) => p.megapixels);
  const ys = usable.map((p) => p.value);
  const plotL = padding;
  const plotR = width - padding;
  const plotT = 40;
  const plotB = height - padding;

  let xFor;
  let yFor;
  let xTicks;
  let yTicks;
  if (logScale) {
    // Snap to enclosing decades so endpoints land on labeled ticks and a
    // sub-decade span (e.g. efficiency 3..4) is never a bare, label-free axis.
    const [xLo, xHi] = enclosingDecades(minOf(xs), maxOf(xs));
    const [yLo, yHi] = enclosingDecades(minOf(ys), maxOf(ys));
    xFor = log10Scale(xLo, xHi, plotL, plotR);
    yFor = log10Scale(yLo, yHi, plotB, plotT); // inverted: larger value → higher
    xTicks = decadeTicks(xLo, xHi);
    yTicks = decadeTicks(yLo, yHi);
  } else {
    const xMax = maxOf(xs) * 1.05 || 1;
    const yMax = maxOf(ys) * 1.15 || 1;
    xFor = linearScale(0, xMax, plotL, plotR);
    yFor = linearScale(0, yMax, plotB, plotT);
    xTicks = { major: [...new Set(xs)].sort((a, b) => a - b), minor: [] };
    yTicks = { major: [0, yMax / 2, yMax], minor: [] };
  }

  // Decade labels want a small-magnitude-aware formatter; linear data labels
  // want formatNumber's rounding.
  const tickFmt = logScale ? formatLogTick : formatNumber;

  // Axis frame.
  const frame = `<line x1="${fmtCoord(plotL)}" y1="${fmtCoord(plotB)}" x2="${fmtCoord(plotR)}" y2="${fmtCoord(plotB)}" stroke="#999" stroke-width="1"/><line x1="${fmtCoord(plotL)}" y1="${fmtCoord(plotT)}" x2="${fmtCoord(plotL)}" y2="${fmtCoord(plotB)}" stroke="#999" stroke-width="1"/>`;

  // X ticks: decade gridlines + labels (major), short marks (minor).
  const xMajor = xTicks.major
    .map((v) => {
      const x = xFor(v);
      return `<line x1="${fmtCoord(x)}" y1="${fmtCoord(plotT)}" x2="${fmtCoord(x)}" y2="${fmtCoord(plotB)}" stroke="#eee" stroke-width="1"/><text x="${fmtCoord(x)}" y="${fmtCoord(plotB + 16)}" text-anchor="middle" font-size="9" fill="#666">${tickFmt(v)}</text>`;
    })
    .join('');
  const xMinor = xTicks.minor
    .map((v) => {
      const x = xFor(v);
      return `<line x1="${fmtCoord(x)}" y1="${fmtCoord(plotB - 4)}" x2="${fmtCoord(x)}" y2="${fmtCoord(plotB)}" stroke="#ccc" stroke-width="0.5"/>`;
    })
    .join('');
  const yMajor = yTicks.major
    .map((v) => {
      const y = yFor(v);
      return `<line x1="${fmtCoord(plotL)}" y1="${fmtCoord(y)}" x2="${fmtCoord(plotR)}" y2="${fmtCoord(y)}" stroke="#eee" stroke-width="1"/><text x="${fmtCoord(plotL - 6)}" y="${fmtCoord(y + 3)}" text-anchor="end" font-size="9" fill="#666">${tickFmt(v)}${escapeXml(unitSuffix)}</text>`;
    })
    .join('');
  const yMinor = yTicks.minor
    .map((v) => {
      const y = yFor(v);
      return `<line x1="${fmtCoord(plotL)}" y1="${fmtCoord(y)}" x2="${fmtCoord(plotL + 4)}" y2="${fmtCoord(y)}" stroke="#ccc" stroke-width="0.5"/>`;
    })
    .join('');

  const ordered = orderedEngines(windowed);
  const seriesSvg = ordered
    .map((engine) => {
      // Size-sorted points that can be POSITIONED on the x-axis (finite MP).
      const eng = windowed
        .filter((p) => p.engine === engine && Number.isFinite(p.megapixels))
        .sort((a, b) => a.megapixels - b.megapixels);
      if (eng.length === 0) return '';
      const color = colorFor(engine, ordered);
      // Break the line into segments of consecutive plottable points; a
      // dropped size (<=0 / non-finite value in log mode) ends the segment so
      // the line never interpolates across it.
      const segments = [];
      let cur = [];
      for (const p of eng) {
        if (plottable(p)) {
          cur.push(p);
        } else if (cur.length) {
          segments.push(cur);
          cur = [];
        }
      }
      if (cur.length) segments.push(cur);
      const polylines = segments
        .map((seg) => {
          const pts = seg
            .map((p) => `${fmtCoord(xFor(p.megapixels))},${fmtCoord(yFor(p.value))}`)
            .join(' ');
          return `<polyline points="${pts}" fill="none" stroke="${color}" stroke-width="2"/>`;
        })
        .join('');
      const dots = segments
        .flat()
        .map(
          (p) =>
            `<circle cx="${fmtCoord(xFor(p.megapixels))}" cy="${fmtCoord(yFor(p.value))}" r="3" fill="${color}"/>`,
        )
        .join('');
      return polylines + dots;
    })
    .join('');

  const modeTag = logScale ? 'log-log' : 'linear';
  // Honest disclosure: how many IN-WINDOW points fell out of the log plot
  // (the <=0 / non-finite drops; points below xMin are out of view, not
  // "omitted").
  const omitted = windowed.length - usable.length;
  const omittedNote =
    logScale && omitted > 0
      ? `<text x="${fmtCoord(width - 8)}" y="26" text-anchor="end" font-size="8" fill="#b26a00">${omitted} pt${omitted === 1 ? '' : 's'} &lt;=0 omitted</text>`
      : '';

  // Legend keeps every engine with INPUT points in the window (even none
  // plottable), so a benchmarked-but-unplottable engine is still disclosed.
  const legend = ordered
    .filter((engine) => windowed.some((p) => p.engine === engine))
    .map((engine, i) => {
      const x = plotL + i * 130;
      const y = height - 14;
      return `<g><rect x="${fmtCoord(x)}" y="${fmtCoord(y - 9)}" width="10" height="10" fill="${colorFor(engine, ordered)}"/><text x="${fmtCoord(x + 14)}" y="${fmtCoord(y)}" font-size="10" fill="#333">${escapeXml(labelFor(engine))}</text></g>`;
    })
    .join('');

  const axisLabels = `<text x="${fmtCoord((plotL + plotR) / 2)}" y="${fmtCoord(plotB + 30)}" text-anchor="middle" font-size="10" fill="#333">${escapeXml(xLabel)}</text><text x="14" y="${fmtCoord((plotT + plotB) / 2)}" text-anchor="middle" font-size="10" fill="#333" transform="rotate(-90 14 ${fmtCoord((plotT + plotB) / 2)})">${escapeXml(yLabel)}</text>`;

  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${height}" font-family="ui-sans-serif"><text x="${fmtCoord(width / 2)}" y="22" text-anchor="middle" font-size="13" font-weight="600">${escapeXml(title)}</text><text x="${fmtCoord(width - 8)}" y="14" text-anchor="end" font-size="8" fill="#999">${modeTag}</text>${omittedNote}${frame}${xMajor}${xMinor}${yMajor}${yMinor}${axisLabels}${seriesSvg}${legend}</svg>`;
}

/* -------------------------------------------------------------------------- */
/* #42 — grouped-bar comparison charts (the plotters port).                   */
/*                                                                            */
/* The JS port of causl-bench's renderMetricGroupedBars + its per-metric      */
/* wrappers (renderWallTimeBars / renderPeakMemoryBars / renderTrackedMemoryBars */
/* / renderThroughputBars / renderEfficiencyBars / renderResourceCostBars),   */
/* adapted to libviprs: each CONFIG (`{w}x{h}_c{conc}`) is a column group and  */
/* each group holds one bar per engine in canonical ENGINE_ORDER. These       */
/* replace the Rust plotters `generate_charts` chart_*.svg emitters; render.mjs */
/* feeds them from benchmark_results.json (Vec<RunMetrics>). The canvas is     */
/* content-sized (grows with config × engine count, as the old plotters        */
/* `chart_w = 160 + n*(max_bars*35+50)` did) so many-config sweeps stay legible */
/* instead of squishing bars to nothing; wall-time / peak-RSS bars carry the   */
/* 95%-CI whiskers the Rust charts drew.                                       */
/* -------------------------------------------------------------------------- */

/**
 * Grouped-bar chart of one metric across every benchmark config — one column
 * group per config, one bar per engine. `rows` are
 * `{ config, engine, value, error? }` records; the caller (render.mjs) feeds
 * them in the config order it wants along the x-axis, and the groups preserve
 * that first-appearance order. An optional `error` is a symmetric half-width
 * (e.g. 95% CI) rendered as a whisker over the bar — the JS equivalent of the
 * Rust chart's CI whiskers (fed only for wall time / peak RSS, `0`/absent for
 * the ratio metrics, exactly as the Rust emitter did).
 *
 * The bar slots and the legend cover the engines actually present in the data,
 * in canonical {@link ENGINE_ORDER} (plus any non-canonical engine, drawn with
 * a fallback colour), so the engine set is stable across every group. A cell
 * that is truly ABSENT (no row for that `config, engine`) keeps its aligned
 * zero-height slot but draws NO value label, so "not benchmarked" never reads
 * as a misleading "0" (a measured zero still labels "0"). Deterministic:
 * canonical iteration, all formatting through the shared helpers, no
 * timestamps / rng.
 *
 * The canvas is content-sized: `width` grows with config × engine count (like
 * the retired Rust `chart_w = 160 + n*(max_bars*35+50)`) so a large sweep can
 * never squeeze bars to a negative width the way a fixed canvas did past ~48
 * configs. A caller may still pin `opts.width`.
 *
 * @param {ReadonlyArray<{config:string, engine:string, value:number, error?:number}>} rows
 * @param {{title?:string, unitSuffix?:string, width?:number, height?:number}} opts
 * @returns {string} deterministic SVG
 */
export function renderMetricGroupedBars(rows, opts = {}) {
  const height = opts.height ?? 320;
  const padding = 56;
  const legendH = 26;
  const title = opts.title ?? '';
  const unitSuffix = opts.unitSuffix ?? '';
  if (rows.length === 0) return svgPlaceholder(opts.width ?? 880, height, title || 'comparison');

  // Config groups in first-appearance order (render.mjs feeds them numerically
  // sorted). `ordered` keeps the FULL canonical order + extras so the palette
  // stays stable; `engines` is the subset actually present, driving the bar
  // slots and the legend (the shared convention with the line renderers).
  const configs = [];
  const seenCfg = new Set();
  for (const r of rows) {
    if (!seenCfg.has(r.config)) {
      seenCfg.add(r.config);
      configs.push(r.config);
    }
  }
  const ordered = orderedEngines(rows);
  const engines = ordered.filter((engine) => rows.some((r) => r.engine === engine));
  // Keep the whole row so `.has()` distinguishes an ABSENT cell from a measured
  // zero, and the optional `error` half-width rides along for the whisker.
  const byKey = new Map();
  for (const r of rows) byKey.set(`${r.config}|${r.engine}`, r);

  // Content-sized canvas: reserve a nominal per-engine slot inside each group so
  // groupW never falls below the point where bars go thin/negative. Below the
  // 880px floor the groups simply spread across the default width.
  const SLOT = 26; // nominal px per engine bar-slot within a group
  const GROUP_INNER_PAD = 24; // px reserved at the group's left+right edges
  const BAR_GAP = 6; // px between adjacent bars in a group
  const minGroupW = engines.length * SLOT + GROUP_INNER_PAD;
  const width = opts.width ?? Math.max(880, 2 * padding + configs.length * minGroupW);
  const groupW = (width - 2 * padding) / configs.length; // >= minGroupW by construction
  // Bar width from the available in-group span, clamped so it is never absurdly
  // wide (few configs) nor collapses to <= 0 (a pinned, too-small width).
  const rawBarW = (groupW - GROUP_INNER_PAD - (engines.length - 1) * BAR_GAP) / engines.length;
  const barW = Math.max(8, Math.min(40, rawBarW));
  // Below this bar width the font-9 value labels would collide, so suppress them
  // (the content-sized default keeps barW >= 20, so this only bites a caller who
  // pins an undersized width).
  const showLabels = barW >= 14;
  const clusterW = engines.length * barW + (engines.length - 1) * BAR_GAP;

  // Scale to the largest finite bar TOP (value + its whisker) so a tall CI cap
  // never clips out of the plot; a missing / NaN cell contributes no height.
  const maxV =
    maxOf(
      rows
        .filter((r) => Number.isFinite(r.value))
        .map((r) => r.value + (Number.isFinite(r.error) && r.error > 0 ? r.error : 0)),
      0,
    ) || 1;
  const plotH = height - 2 * padding - legendH;
  const baseY = height - padding - legendH; // the bar footing / x-baseline
  const pxFor = (v) => (Number.isFinite(v) && v > 0 ? (v / maxV) * plotH : 0);

  // A faint baseline at the bar footing gives the floating bars a magnitude
  // anchor (the Rust chart had a full y-axis + mesh; this keeps the flat
  // causl-bench aesthetic but stops the bars from floating unreferenced).
  const baseline = `<line x1="${fmtCoord(padding)}" y1="${fmtCoord(baseY)}" x2="${fmtCoord(width - padding)}" y2="${fmtCoord(baseY)}" stroke="#ddd" stroke-width="1"/>`;

  const groups = configs
    .map((config, gi) => {
      const gx = padding + gi * groupW;
      const startX = gx + (groupW - clusterW) / 2; // centre the cluster in the group
      const bars = engines
        .map((engine, ei) => {
          const row = byKey.get(`${config}|${engine}`);
          const present = row !== undefined;
          const v = present ? row.value : Number.NaN;
          const h = pxFor(v);
          const x = startX + ei * (barW + BAR_GAP);
          const y = baseY - h;
          const err = present && Number.isFinite(row.error) && row.error > 0 ? row.error : 0;
          // 95% CI whisker (only when a real error rides on a plotted bar).
          let whisker = '';
          let topY = y;
          if (err > 0 && Number.isFinite(v) && v > 0) {
            const yHi = baseY - pxFor(v + err);
            const yLo = baseY - pxFor(Math.max(0, v - err));
            const cx = x + barW / 2;
            const cap = Math.max(2, barW * 0.25);
            whisker =
              `<line x1="${fmtCoord(cx)}" y1="${fmtCoord(yHi)}" x2="${fmtCoord(cx)}" y2="${fmtCoord(yLo)}" stroke="#333" stroke-width="1"/>` +
              `<line x1="${fmtCoord(cx - cap)}" y1="${fmtCoord(yHi)}" x2="${fmtCoord(cx + cap)}" y2="${fmtCoord(yHi)}" stroke="#333" stroke-width="1"/>` +
              `<line x1="${fmtCoord(cx - cap)}" y1="${fmtCoord(yLo)}" x2="${fmtCoord(cx + cap)}" y2="${fmtCoord(yLo)}" stroke="#333" stroke-width="1"/>`;
            topY = Math.min(topY, yHi);
          }
          // Absent cell → no label (empty slot). Present cell → value label
          // above the bar/whisker when the bar is wide enough to carry it.
          const label =
            present && showLabels
              ? `<text x="${fmtCoord(x + barW / 2)}" y="${fmtCoord(topY - 3)}" text-anchor="middle" font-size="9" fill="#444">${formatNumber(v)}${escapeXml(unitSuffix)}</text>`
              : '';
          return `<g><rect x="${fmtCoord(x)}" y="${fmtCoord(y)}" width="${fmtCoord(barW)}" height="${fmtCoord(h)}" fill="${colorFor(engine, ordered)}"/>${whisker}${label}</g>`;
        })
        .join('');
      const sx = gx + groupW / 2;
      const sy = baseY + 14;
      return `<g>${bars}<text x="${fmtCoord(sx)}" y="${fmtCoord(sy)}" text-anchor="middle" font-size="11" fill="#222" font-weight="500">${escapeXml(config)}</text></g>`;
    })
    .join('');

  const legendY = height - 12;
  const legend = engines
    .map((engine, i) => {
      const x = padding + i * 150;
      return `<g><rect x="${fmtCoord(x)}" y="${fmtCoord(legendY - 10)}" width="10" height="10" fill="${colorFor(engine, ordered)}"/><text x="${fmtCoord(x + 14)}" y="${fmtCoord(legendY - 1)}" font-size="10" fill="#333">${escapeXml(labelFor(engine))}</text></g>`;
    })
    .join('');

  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${height}" font-family="ui-sans-serif"><text x="${fmtCoord(width / 2)}" y="22" text-anchor="middle" font-size="13" font-weight="600">${escapeXml(title)}</text>${baseline}${groups}${legend}</svg>`;
}

/*
 * Per-metric wrappers. Each takes `(rows, opts)` — the `(data, opts)` convention
 * the rest of the renderer family follows — so a caller can still set
 * width/height while the metric title + unit stay fixed. render.mjs feeds them
 * `{config, engine, value, error?}` rows built from benchmark_results.json.
 */

/** Wall time per config (ms, median). Lower is better. Whiskers = 95% CI. */
export function renderWallTimeBars(rows, opts = {}) {
  return renderMetricGroupedBars(rows, {
    title: 'Wall Time (lower is better; whiskers = 95% CI)',
    unitSuffix: 'ms',
    ...opts,
  });
}

/** Peak RSS per config (MB) — the cross-engine-comparable memory basis. Lower is better. Whiskers = 95% CI. */
export function renderPeakMemoryBars(rows, opts = {}) {
  return renderMetricGroupedBars(rows, {
    title: 'Peak RSS (lower is better; whiskers = 95% CI)',
    unitSuffix: 'MB',
    ...opts,
  });
}

/** Engine-tracked working set per config (MB) — a libviprs-only per-run figure (libvips reports 0). Lower is better. */
export function renderTrackedMemoryBars(rows, opts = {}) {
  return renderMetricGroupedBars(rows, {
    title: 'Engine-Tracked Working Set — libviprs engines (lower is better)',
    unitSuffix: 'MB',
    ...opts,
  });
}

/** Raw throughput per config (tiles/second). Higher is better. */
export function renderThroughputBars(rows, opts = {}) {
  return renderMetricGroupedBars(rows, {
    title: 'Raw Throughput — Tiles/s (higher is better)',
    unitSuffix: '',
    ...opts,
  });
}

/** Memory efficiency per config (tiles/second per RSS-MB). Higher is better. */
export function renderEfficiencyBars(rows, opts = {}) {
  return renderMetricGroupedBars(rows, {
    title: 'Memory Efficiency — Tiles/s per RSS-MB (higher is better)',
    unitSuffix: '',
    ...opts,
  });
}

/** Resource cost per config (RSS-MB·s per tile). Lower is better. */
export function renderResourceCostBars(rows, opts = {}) {
  return renderMetricGroupedBars(rows, {
    title: 'Resource Cost — RSS-MB·s per Tile (lower is better)',
    unitSuffix: '',
    ...opts,
  });
}
