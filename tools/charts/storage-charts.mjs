#!/usr/bin/env node
/**
 * storage-charts.mjs — a directory tree against a PMTiles archive.
 *
 * The storage family builds a pyramid into each backend and then reads it back
 * twelve ways. It has been measured and archived since #65 and nothing drew it:
 * `render.mjs` reads the engines-family JSON shapes and never the storage
 * document, so 528 measured cells a run had no chart path at all.
 *
 * Every scenario goes through the same comparability contract the libvips
 * comparison uses, so `decode_root` (pmtiles only, a directory tree has no root
 * to decode) and `requests` (a declared count against an observed one) are held
 * out here exactly as they are there.
 *
 * Usage:
 *   node tools/charts/storage-charts.mjs <archive/storage/*.json> [--out-dir DIR]
 */

import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { renderGroupedBars } from '@spdrman/bencharts';
import { series, labelFor, betterOf } from './series.mjs';
import { loadContract, assessComparison, CONTRACT_PATH } from './comparability.mjs';

/**
 * Hang each cell's work count off the cell.
 *
 * The counts live in `invariants`, keyed by library, scale and source, so
 * without this the census has nothing to check and a held-out cell's reason
 * stops being evidence.
 */
export function attachWorkCounts(document) {
  const counts = new Map();
  for (const inv of document.invariants ?? []) {
    if (inv.name !== 'tiles_produced') continue;
    counts.set(`${inv.library}\u0000${inv.scale}\u0000${inv.source}`, inv.value);
  }
  return {
    ...document,
    cells: (document.cells ?? []).map((c) => {
      const hit = counts.get(`${c.backend}\u0000${c.scale}\u0000${c.source}`);
      return hit === undefined ? c : { ...c, tiles_produced: hit };
    }),
  };
}

const slug = (s) => String(s).replace(/[^A-Za-z0-9._-]+/g, '-');

/**
 * The half-width of a confidence interval, whatever shape the document carries.
 *
 * `ci95` is `[lo, hi]`, not a scalar. The first version of this file tested it
 * with `Number.isFinite`, which is false for an array, so NO storage chart ever
 * drew a whisker while the chart cheerfully carried a "95% CI" label. The test
 * fixture used a scalar, so the suite agreed with the code and neither agreed
 * with the data.
 *
 * The interval is not symmetric about the median, and a whisker is. I take the
 * longer arm, so the whisker always covers the interval rather than cutting it
 * short, and say so here because a symmetric drawing of an asymmetric interval
 * is a small lie either way and the honest choice is the conservative one.
 *
 * @param {unknown} ci95
 * @param {number} median
 * @returns {number|null}
 */
export function halfWidth(ci95, median) {
  if (Number.isFinite(ci95)) return /** @type {number} */ (ci95);
  if (!Array.isArray(ci95) || ci95.length !== 2) return null;
  const [lo, hi] = ci95;
  if (!Number.isFinite(lo) || !Number.isFinite(hi) || !Number.isFinite(median)) return null;
  const arm = Math.max(median - lo, hi - median);
  return arm > 0 ? arm : null;
}

/** The median of a list, for collapsing repeated placements of one cell. */
export function medianOf(values) {
  const v = [...values].sort((a, b) => a - b);
  if (v.length === 0) return null;
  const mid = v.length >> 1;
  return v.length % 2 ? v[mid] : (v[mid - 1] + v[mid]) / 2;
}

/**
 * Rows the harness itself does not stand behind.
 *
 * The document flags every one of these and the first version of this renderer
 * read none of them: 210 of 528 rows in the current archive are
 * `confidence: low`, 40 are `timerSaturated` and 44 are `oversubscribed`, and
 * all of them were drawn identically to a high-confidence row. A saturated
 * timer in particular is not a slow measurement, it is a measurement of the
 * clock, and racing it against a row that clears the floor compares two
 * different things.
 */
export function unfitToRace(cell) {
  const why = [];
  if (cell.timerSaturated) why.push('timer saturated');
  if (cell.oversubscribed) why.push('oversubscribed');
  if (cell.confidence === 'low') why.push(`low confidence: ${(cell.lowConfidenceReasons ?? []).join('; ') || 'unstated'}`);
  return why;
}

/**
 * @param {object} document a storage family report
 * @param {{ contract?: object }} [opts]
 */
export function buildStorageCharts(document, opts = {}) {
  const contract = opts.contract ?? loadContract(CONTRACT_PATH);
  const withWork = attachWorkCounts(document);
  const cells = (withWork.cells ?? []).filter((c) => c.outcome === 'ok' && Number.isFinite(c.median));

  const charts = [];
  const held = [];
  const violations = [];

  const byScenario = new Map();
  for (const c of cells) {
    if (!byScenario.has(c.scenario)) byScenario.set(c.scenario, []);
    byScenario.get(c.scenario).push(c);
  }

  for (const [scenario, members] of [...byScenario].sort((a, b) => a[0].localeCompare(b[0]))) {
    // The cell id is this family's config: a storage cell is one pyramid shape
    // at one source, which is what a backend is compared on.
    const assessment = assessComparison(members, contract, { configOf: (c) => c.cell });
    violations.push(...assessment.violations);
    const undeclared = assessment.violations.filter((v) => v.rule === 'every-scenario-is-declared');
    if (undeclared.length > 0) {
      throw new Error(`${scenario} is not declared in the comparability contract; add it with a status `
        + 'and, if it is not comparable, a reason. There is no default.');
    }
    const chartable = new Set(assessment.cells.filter((c) => c.chartable).map((c) => c.config));
    if (chartable.size === 0) {
      const first = assessment.cells[0];
      held.push({ scenario, status: first?.status ?? 'undeclared', reasons: first?.reasons ?? [] });
      continue;
    }

    const byMetric = new Map();
    for (const c of members) {
      if (!chartable.has(c.cell)) continue;
      if (!byMetric.has(c.metric)) byMetric.set(c.metric, []);
      byMetric.get(c.metric).push(c);
    }

    for (const [metric, metricCells] of [...byMetric].sort((a, b) => a[0].localeCompare(b[0]))) {
      const backends = [...new Set(metricCells.map((c) => c.backend))];
      if (backends.length < 2) {
        held.push({ scenario, metric, status: 'single-backend', reasons: [`only ${backends.join(', ')} reports it`] });
        continue;
      }
      // Every backend gets a slot in every cell it could have run, so one that
      // did not run reads as absent rather than vanishing from the chart.
      const groups = [...new Set(metricCells.map((c) => c.cell))].sort();
      // A Map keyed on cell and backend KEEPS THE LAST ROW AND DROPS THE REST.
      // The control cell is deliberately measured six times per sweep to show
      // drift, so that collapsed 22 rows to 12 and charted one arbitrary sixth
      // of the evidence. Its six placements span 10.67x to 16.89x on open.p50,
      // and which one you saw depended on iteration order.
      const placements = new Map();
      for (const c of metricCells) {
        const key = `${c.cell}\u0000${c.backend}`;
        if (!placements.has(key)) placements.set(key, []);
        placements.get(key).push(c);
      }
      const rows = [];
      for (const group of groups) {
        for (const backend of backends) {
          const hits = placements.get(`${group}\u0000${backend}`) ?? [];
          const fit = hits.filter((h) => unfitToRace(h).length === 0);
          if (fit.length === 0) {
            if (hits.length > 0) {
              held.push({
                scenario, metric, cell: group, backend,
                status: 'unfit-to-race',
                reasons: [...new Set(hits.flatMap(unfitToRace))],
              });
            }
            continue;
          }
          const value = medianOf(fit.map((h) => h.median));
          const row = { group, series: backend, value };
          // One placement gets its own interval; several get the spread across
          // them, which is the wider and more honest number.
          const arm = fit.length === 1
            ? halfWidth(fit[0].ci95, fit[0].median)
            : (Math.max(...fit.map((h) => h.median)) - Math.min(...fit.map((h) => h.median))) / 2;
          if (Number.isFinite(arm) && arm > 0) row.error = arm;
          rows.push(row);
        }
      }
      if (rows.length === 0) continue;
      const sample = metricCells[0];
      charts.push({
        filename: `storage_${slug(scenario)}_${slug(metric)}.svg`,
        scenario,
        metric,
        svg: renderGroupedBars(rows, {
          series,
          title: `${labelFor(metric)} · ${scenario}`,
          unit: sample.unit,
          better: betterOf(sample.direction),
          errorLabel: '95% CI or placement spread',
        }),
      });
    }
  }

  return { charts, held, violations };
}

if (process.argv[1] && import.meta.url === `file://${process.argv[1]}`) {
  const files = process.argv.slice(2).filter((a) => !a.startsWith('-'));
  const outArg = process.argv.find((a) => a.startsWith('--out-dir='));
  const outDir = outArg ? outArg.split('=').slice(1).join('=') : 'report/storage';
  let failed = 0;
  for (const file of files) {
    const { charts, held, violations } = buildStorageCharts(JSON.parse(readFileSync(file, 'utf8')));
    mkdirSync(outDir, { recursive: true });
    for (const { filename, svg } of charts) writeFileSync(join(outDir, filename), svg);
    console.log(`${file}: ${charts.length} charts written to ${outDir}`);
    for (const h of held) {
      console.log(`  held out  ${h.scenario}${h.metric ? `.${h.metric}` : ''}  ${h.status}  ${h.reasons.join(', ')}`);
    }
    for (const v of violations) {
      console.error(`  FAIL ${v.rule} ${v.message}`);
      failed++;
    }
  }
  process.exit(failed > 0 ? 1 : 0);
}
