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
 *     when an engine is MISSING from a snapshot the polyline BREAKS (a gap)
 *     rather than drawing a straight line across the hole (#28/#29).
 *   - {@link renderScalabilityChart} from causl's `renderScalabilityCell` /
 *     `renderScalabilitySweep` (which already log-scaled x via `Math.log10`),
 *     extended to true LOG-LOG axes — log10 on BOTH the megapixel x-axis and
 *     the metric y-axis — with log-spaced decade major ticks + 2..9 minor
 *     ticks (#21/#34). A LINEAR mode is kept behind `opts.logScale = false`.
 *   - the shared helpers: {@link COLORS}, {@link ENGINE_ORDER},
 *     {@link formatNumber}, {@link svgPlaceholder}, and the SVG scaffolding /
 *     font conventions, so both libviprs charts read like the causl-bench
 *     article.
 *
 * Determinism (the article's CI-snapshot contract): every renderer is a pure
 * function of its input — canonical {@link ENGINE_ORDER} iteration (never a
 * hash-map order), all number formatting through {@link formatNumber} /
 * {@link fmtCoord} (locale-independent), no timestamps, no random ids.
 */

/**
 * Canonical engine order — every renderer iterates this for stable output.
 * The libvips oracle leads, then the three libviprs engines in pipeline
 * order (monolithic → streaming → mapreduce). Mirrors the draw order the
 * removed Rust plotters emitters used, so legends stay familiar.
 */
export const ENGINE_ORDER = ['libvips', 'monolithic', 'streaming', 'mapreduce'];

/**
 * Engine → stroke colour. Hex equivalents of the `RGBColor` palette the
 * Rust emitters used (COLOR_VIPS / COLOR_MONO / COLOR_STREAM / COLOR_MR), so
 * the JS charts are colour-identical to the prior report artifacts.
 */
export const COLORS = {
  libvips: '#9c27b0', // RGB(156, 39, 176) — purple
  monolithic: '#4285f4', // RGB(66, 133, 244) — blue
  streaming: '#34a853', // RGB(52, 168, 83) — green
  mapreduce: '#ea4335', // RGB(234, 67, 53) — red
};

/**
 * Nice human labels for engines. Legends print the raw engine key by
 * default (matches the JSON `engine` field and keeps tests simple); this
 * map is exported for callers that want the title-cased display form.
 */
export const ENGINE_LABELS = {
  libvips: 'libvips',
  monolithic: 'Monolithic',
  streaming: 'Streaming',
  mapreduce: 'MapReduce',
};

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
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${height}" font-family="ui-sans-serif"><text x="${fmtCoord(width / 2)}" y="${fmtCoord(height / 2)}" text-anchor="middle" font-size="12" fill="#999">no data — ${label}</text></svg>`;
}

/* -------------------------------------------------------------------------- */
/* Axis scales + log-spaced ticks (#21 / #34).                                */
/* -------------------------------------------------------------------------- */

/**
 * A log10 axis: maps `[domainMin, domainMax]` (both > 0) onto
 * `[rangeMin, rangeMax]` in log space, so every decade occupies an EQUAL
 * pixel span. Returns a `(value) => pixel` function. A zero-width domain
 * collapses to `rangeMin`.
 */
export function log10Scale(domainMin, domainMax, rangeMin, rangeMax) {
  const lo = Math.log10(domainMin);
  const hi = Math.log10(domainMax);
  const span = hi - lo;
  return (v) => {
    if (span === 0) return rangeMin;
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
 * Split an engine's runIndex-sorted points into maximal runs of CONSECUTIVE
 * snapshot indices. A missing snapshot in the middle of an engine's history
 * ends the current segment and starts a new one, so the renderer draws a
 * gap rather than a line across the hole (#28/#29 — the enhancement over the
 * causl original, which drew a single polyline over all of an engine's
 * points regardless of gaps).
 */
function consecutiveSegments(series) {
  const segments = [];
  let cur = [];
  let prev = null;
  for (const p of series) {
    if (prev !== null && p.runIndex !== prev + 1) {
      segments.push(cur);
      cur = [];
    }
    cur.push(p);
    prev = p.runIndex;
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
  const minRun = allRuns[0] ?? 0;
  const maxRun = allRuns[allRuns.length - 1] ?? 0;
  const maxV = Math.max(...points.map((p) => p.value).filter(Number.isFinite), 0) || 1;

  const xFor = (r) => {
    if (maxRun === minRun) return padding;
    return padding + ((r - minRun) / (maxRun - minRun)) * (width - 2 * padding);
  };
  const yFor = (v) => height - padding - (v / maxV) * (height - 2 * padding);

  const lines = ENGINE_ORDER.map((engine) => {
    const series = points
      .filter((p) => p.engine === engine && Number.isFinite(p.value))
      .sort((a, b) => a.runIndex - b.runIndex);
    if (series.length === 0) return '';
    const color = COLORS[engine];
    // One polyline per consecutive-run segment → gaps break the line.
    const polylines = consecutiveSegments(series)
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
  }).join('');

  const tickLabels = allRuns
    .map((r) => {
      const x = xFor(r);
      const y = height - padding + 14;
      const v = versionByRun.get(r) ?? `r${r}`;
      return `<text x="${fmtCoord(x)}" y="${fmtCoord(y)}" text-anchor="middle" font-size="9" fill="#666">${v}</text>`;
    })
    .join('');

  const legend = ENGINE_ORDER.map((engine, i) => {
    const x = padding + i * 150;
    return `<g><rect x="${fmtCoord(x)}" y="${fmtCoord(height - 16)}" width="10" height="10" fill="${COLORS[engine]}"/><text x="${fmtCoord(x + 14)}" y="${fmtCoord(height - 7)}" font-size="10" fill="#333">${engine}</text></g>`;
  }).join('');

  const yMaxText = `${formatNumber(maxV)}${unitSuffix}`;
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${height}" font-family="ui-sans-serif"><text x="${fmtCoord(width / 2)}" y="20" text-anchor="middle" font-size="13" font-weight="600">${title}</text><text x="${fmtCoord(padding - 4)}" y="${fmtCoord(padding + 4)}" text-anchor="end" font-size="9" fill="#666">${yMaxText}</text>${lines}${tickLabels}${legend}</svg>`;
}

/* -------------------------------------------------------------------------- */
/* #21 — scalability (megapixels × metric, one polyline per engine, log-log). */
/* -------------------------------------------------------------------------- */

/**
 * Scalability line chart: x = image size in megapixels, y = one metric, one
 * polyline per engine. Defaults to true LOG-LOG axes (log10 on both) with
 * decade major ticks + 2..9 minor ticks; `opts.logScale = false` selects the
 * preserved linear mode (axes from 0, no decade ticks).
 *
 * @param {ReadonlyArray<{engine:string, megapixels:number, value:number}>} points
 * @param {{title?:string, xLabel?:string, yLabel?:string, unitSuffix?:string, width?:number, height?:number, logScale?:boolean}} opts
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

  // In log mode only strictly-positive (x, y) points are plottable.
  const usable = points.filter(
    (p) =>
      Number.isFinite(p.megapixels) &&
      Number.isFinite(p.value) &&
      (!logScale || (p.megapixels > 0 && p.value > 0)),
  );
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
    const xMin = Math.min(...xs);
    const xMax = Math.max(...xs);
    const yMin = Math.min(...ys);
    const yMax = Math.max(...ys);
    xFor = log10Scale(xMin, xMax, plotL, plotR);
    yFor = log10Scale(yMin, yMax, plotB, plotT); // inverted: larger value → higher
    xTicks = decadeTicks(xMin, xMax);
    yTicks = decadeTicks(yMin, yMax);
  } else {
    const xMax = Math.max(...xs) * 1.05 || 1;
    const yMax = Math.max(...ys) * 1.15 || 1;
    xFor = linearScale(0, xMax, plotL, plotR);
    yFor = linearScale(0, yMax, plotB, plotT);
    xTicks = { major: [...new Set(xs)].sort((a, b) => a - b), minor: [] };
    yTicks = { major: [0, yMax / 2, yMax], minor: [] };
  }

  // Axis frame.
  const frame = `<line x1="${fmtCoord(plotL)}" y1="${fmtCoord(plotB)}" x2="${fmtCoord(plotR)}" y2="${fmtCoord(plotB)}" stroke="#999" stroke-width="1"/><line x1="${fmtCoord(plotL)}" y1="${fmtCoord(plotT)}" x2="${fmtCoord(plotL)}" y2="${fmtCoord(plotB)}" stroke="#999" stroke-width="1"/>`;

  // X ticks: decade gridlines + labels (major), short marks (minor).
  const xMajor = xTicks.major
    .map((v) => {
      const x = xFor(v);
      return `<line x1="${fmtCoord(x)}" y1="${fmtCoord(plotT)}" x2="${fmtCoord(x)}" y2="${fmtCoord(plotB)}" stroke="#eee" stroke-width="1"/><text x="${fmtCoord(x)}" y="${fmtCoord(plotB + 16)}" text-anchor="middle" font-size="9" fill="#666">${formatNumber(v)}</text>`;
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
      return `<line x1="${fmtCoord(plotL)}" y1="${fmtCoord(y)}" x2="${fmtCoord(plotR)}" y2="${fmtCoord(y)}" stroke="#eee" stroke-width="1"/><text x="${fmtCoord(plotL - 6)}" y="${fmtCoord(y + 3)}" text-anchor="end" font-size="9" fill="#666">${formatNumber(v)}${unitSuffix}</text>`;
    })
    .join('');
  const yMinor = yTicks.minor
    .map((v) => {
      const y = yFor(v);
      return `<line x1="${fmtCoord(plotL)}" y1="${fmtCoord(y)}" x2="${fmtCoord(plotL + 4)}" y2="${fmtCoord(y)}" stroke="#ccc" stroke-width="0.5"/>`;
    })
    .join('');

  const seriesSvg = ENGINE_ORDER.map((engine) => {
    const series = usable
      .filter((p) => p.engine === engine)
      .sort((a, b) => a.megapixels - b.megapixels);
    if (series.length === 0) return '';
    const color = COLORS[engine];
    const pts = series.map((p) => `${fmtCoord(xFor(p.megapixels))},${fmtCoord(yFor(p.value))}`).join(' ');
    const polyline = `<polyline points="${pts}" fill="none" stroke="${color}" stroke-width="2"/>`;
    const dots = series
      .map(
        (p) =>
          `<circle cx="${fmtCoord(xFor(p.megapixels))}" cy="${fmtCoord(yFor(p.value))}" r="3" fill="${color}"/>`,
      )
      .join('');
    return polyline + dots;
  }).join('');

  const modeTag = logScale ? 'log-log' : 'linear';
  const legend = ENGINE_ORDER.filter((e) => usable.some((p) => p.engine === e))
    .map((engine, i) => {
      const x = plotL + i * 130;
      const y = height - 14;
      return `<g><rect x="${fmtCoord(x)}" y="${fmtCoord(y - 9)}" width="10" height="10" fill="${COLORS[engine]}"/><text x="${fmtCoord(x + 14)}" y="${fmtCoord(y)}" font-size="10" fill="#333">${engine}</text></g>`;
    })
    .join('');

  const axisLabels = `<text x="${fmtCoord((plotL + plotR) / 2)}" y="${fmtCoord(plotB + 30)}" text-anchor="middle" font-size="10" fill="#333">${xLabel}</text><text x="14" y="${fmtCoord((plotT + plotB) / 2)}" text-anchor="middle" font-size="10" fill="#333" transform="rotate(-90 14 ${fmtCoord((plotT + plotB) / 2)})">${yLabel}</text>`;

  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${height}" font-family="ui-sans-serif"><text x="${fmtCoord(width / 2)}" y="22" text-anchor="middle" font-size="13" font-weight="600">${title}</text><text x="${fmtCoord(width - 8)}" y="14" text-anchor="end" font-size="8" fill="#999">${modeTag}</text>${frame}${xMajor}${xMinor}${yMajor}${yMinor}${axisLabels}${seriesSvg}${legend}</svg>`;
}
