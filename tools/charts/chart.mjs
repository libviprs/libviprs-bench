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
 *     kept behind `opts.logScale = false`.
 *   - the shared helpers: {@link COLORS}, {@link ENGINE_ORDER},
 *     {@link formatNumber}, {@link svgPlaceholder}, and the SVG scaffolding /
 *     font conventions, so both libviprs charts read like the causl-bench
 *     article.
 *
 * Cross-stack note: the six grouped-bar `chart_*.svg` comparison charts are
 * still emitted in Rust (`libviprs_bench::generate_charts`, plotters). The
 * engine palette below is therefore intentionally MIRRORED from that Rust
 * `COLOR_VIPS/COLOR_MONO/COLOR_STREAM/COLOR_MR` set and must be kept in
 * lockstep with it until that half is ported too (see the tracking issue for
 * completing the JS migration and dropping plotters).
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

  // A point is plottable when both coordinates are finite and (in log mode)
  // strictly positive. Non-plottable points break the line, never bridge it.
  const plottable = (p) =>
    Number.isFinite(p.megapixels) &&
    Number.isFinite(p.value) &&
    (!logScale || (p.megapixels > 0 && p.value > 0));
  const usable = points.filter(plottable);
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

  const ordered = orderedEngines(points);
  const seriesSvg = ordered
    .map((engine) => {
      // Size-sorted points that can be POSITIONED on the x-axis (finite MP).
      const eng = points
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
  // Honest disclosure: how many input points fell out of the log plot.
  const omitted = points.length - usable.length;
  const omittedNote =
    logScale && omitted > 0
      ? `<text x="${fmtCoord(width - 8)}" y="26" text-anchor="end" font-size="8" fill="#b26a00">${omitted} pt${omitted === 1 ? '' : 's'} &lt;=0 omitted</text>`
      : '';

  // Legend keeps every engine with INPUT points (even none plottable), so a
  // benchmarked-but-unplottable engine is still disclosed.
  const legend = ordered
    .filter((engine) => points.some((p) => p.engine === engine))
    .map((engine, i) => {
      const x = plotL + i * 130;
      const y = height - 14;
      return `<g><rect x="${fmtCoord(x)}" y="${fmtCoord(y - 9)}" width="10" height="10" fill="${colorFor(engine, ordered)}"/><text x="${fmtCoord(x + 14)}" y="${fmtCoord(y)}" font-size="10" fill="#333">${escapeXml(labelFor(engine))}</text></g>`;
    })
    .join('');

  const axisLabels = `<text x="${fmtCoord((plotL + plotR) / 2)}" y="${fmtCoord(plotB + 30)}" text-anchor="middle" font-size="10" fill="#333">${escapeXml(xLabel)}</text><text x="14" y="${fmtCoord((plotT + plotB) / 2)}" text-anchor="middle" font-size="10" fill="#333" transform="rotate(-90 14 ${fmtCoord((plotT + plotB) / 2)})">${escapeXml(yLabel)}</text>`;

  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${height}" font-family="ui-sans-serif"><text x="${fmtCoord(width / 2)}" y="22" text-anchor="middle" font-size="13" font-weight="600">${escapeXml(title)}</text><text x="${fmtCoord(width - 8)}" y="14" text-anchor="end" font-size="8" fill="#999">${modeTag}</text>${omittedNote}${frame}${xMajor}${xMinor}${yMajor}${yMinor}${axisLabels}${seriesSvg}${legend}</svg>`;
}
