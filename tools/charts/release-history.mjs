#!/usr/bin/env node
/**
 * release-history.mjs — is libviprs getting faster or slower across releases?
 *
 * Every archived report carries `provenance.library.version`, so the answer is
 * already in `archive/` and nobody was drawing it. One step per release, one
 * line per technology, the same cell throughout.
 *
 * Two rules make the line mean something:
 *
 *   The cell is held fixed. A trend that silently changes which measurement it
 *   is plotting is not a trend, so the cell charted is the one present in the
 *   most releases, and a release missing it leaves a gap rather than a zero.
 *
 *   One release is not a trend. Fewer than two and nothing is drawn, with the
 *   reason reported, because a single point drawn as a line is a chart that
 *   looks like a measurement of change and is not.
 *
 * Usage:
 *   node tools/charts/release-history.mjs [--archive DIR] [--out-dir DIR]
 */

import { readFileSync, readdirSync, writeFileSync, mkdirSync, existsSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { renderTrend } from '@spdrman/bencharts';
import { series, labelFor, betterOf } from './series.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = join(HERE, '..', '..');

/** One entry per version, most recent run of it, oldest release first. */
export function releasesOf(reports) {
  const byVersion = new Map();
  for (const report of reports) {
    const version = report?.provenance?.library?.version;
    if (!version) continue;
    const seen = byVersion.get(version);
    if (!seen || String(report.startedAt) > String(seen.startedAt)) {
      byVersion.set(version, {
        version,
        commit: report.provenance.library.commit,
        startedAt: report.startedAt,
        report,
      });
    }
  }
  return [...byVersion.values()]
    .sort((a, b) => String(a.startedAt).localeCompare(String(b.startedAt)))
    .map((r, step) => ({ ...r, step }));
}

const okCells = (report, scenario, metric) =>
  (report.cells ?? []).filter((c) =>
    c.scenario === scenario && c.metric === metric && c.outcome === 'ok' && Number.isFinite(c.median));

/**
 * The points for one (scenario, metric), across releases.
 *
 * Choosing the cell is the load-bearing part: it is the one present in the most
 * releases, with the largest scale breaking a tie, so the line compares one
 * measurement to itself rather than drifting between cells as the suite grows.
 */
export function seriesForMetric(reports, { scenario, metric }) {
  const releases = releasesOf(reports);
  const presence = new Map();
  const scaleOf = new Map();
  for (const release of releases) {
    for (const c of new Set(okCells(release.report, scenario, metric).map((c) => c.cell))) {
      presence.set(c, (presence.get(c) ?? 0) + 1);
    }
    for (const c of okCells(release.report, scenario, metric)) {
      scaleOf.set(c.cell, Math.max(scaleOf.get(c.cell) ?? 0, Number(c.scale) || 0));
    }
  }
  const candidates = [...presence.entries()].sort((a, b) =>
    b[1] - a[1] || (scaleOf.get(b[0]) ?? 0) - (scaleOf.get(a[0]) ?? 0) || String(a[0]).localeCompare(String(b[0])));
  const cell = candidates.length ? candidates[0][0] : null;
  if (cell === null) return { points: [], cell: null, releases, unit: null, direction: null };

  const technologies = new Set();
  let unit = null;
  let direction = null;
  for (const release of releases) {
    for (const c of okCells(release.report, scenario, metric)) {
      if (c.cell !== cell) continue;
      technologies.add(c.backend);
      unit ??= c.unit;
      direction ??= c.direction;
    }
  }

  const points = [];
  for (const release of releases) {
    const matching = new Map(
      okCells(release.report, scenario, metric).filter((c) => c.cell === cell).map((c) => [c.backend, c.median]),
    );
    for (const technology of [...technologies].sort()) {
      points.push({
        step: release.step,
        series: technology,
        // Absent is NaN, never 0: a release that did not run this cell has no
        // measurement, and drawing it as zero would report an improvement to
        // nothing.
        value: matching.has(technology) ? matching.get(technology) : Number.NaN,
        label: release.version,
      });
    }
  }
  return { points, cell, releases, unit, direction };
}

/** Every (family, scenario, metric) the archive can support a trend for. */
export function buildReleaseCharts(reports, opts = {}) {
  const charts = [];
  const skipped = [];
  const byFamily = new Map();
  for (const report of reports) {
    if (!byFamily.has(report.family)) byFamily.set(report.family, []);
    byFamily.get(report.family).push(report);
  }

  for (const [family, familyReports] of byFamily) {
    const releases = releasesOf(familyReports);
    if (releases.length < 2) {
      skipped.push({
        family,
        reason: `${family}: the archive holds one release (${releases.map((r) => r.version).join(', ') || 'none'}), `
          + 'and a trend needs at least two. Nothing drawn: a single point drawn as a line reports a change '
          + 'that was never measured.',
      });
      continue;
    }
    const pairs = new Set();
    for (const report of familyReports) {
      for (const c of report.cells ?? []) {
        if (c.outcome === 'ok' && c.scenario && c.metric) pairs.add(`${c.scenario}\u0000${c.metric}`);
      }
    }
    for (const pair of [...pairs].sort()) {
      const [scenario, metric] = pair.split('\u0000');
      const { points, cell, unit, direction } = seriesForMetric(familyReports, { scenario, metric });
      if (points.length === 0) continue;
      const drawable = points.filter((p) => Number.isFinite(p.value));
      if (drawable.length < 2) {
        skipped.push({ family, scenario, metric, reason: `${family} ${scenario} ${metric}: fewer than two measured points` });
        continue;
      }
      const svg = renderTrend(points, {
        series,
        title: `${labelFor(metric)} across releases · ${scenario} · ${cell}`,
        unit: unit ?? undefined,
        better: betterOf(direction),
      });
      charts.push({ filename: `release_${slug(family)}_${slug(scenario)}_${slug(metric)}.svg`, svg, family, scenario, metric });
    }
  }
  return { charts, skipped };
}

const slug = (s) => String(s).replace(/[^A-Za-z0-9._-]+/g, '-');

export function loadArchive(archiveDir = join(REPO, 'archive')) {
  const reports = [];
  if (!existsSync(archiveDir)) return reports;
  for (const family of readdirSync(archiveDir)) {
    const dir = join(archiveDir, family);
    let entries;
    try { entries = readdirSync(dir); } catch { continue; }
    for (const file of entries) {
      if (!file.endsWith('.json') || file === 'index.json') continue;
      reports.push(JSON.parse(readFileSync(join(dir, file), 'utf8')));
    }
  }
  return reports;
}

if (process.argv[1] && import.meta.url === `file://${process.argv[1]}`) {
  const arg = (name, fallback) => {
    const hit = process.argv.find((a) => a.startsWith(`--${name}=`));
    return hit ? hit.split('=').slice(1).join('=') : fallback;
  };
  const reports = loadArchive(arg('archive', join(REPO, 'archive')));
  const outDir = arg('out-dir', join(REPO, 'report', 'releases'));
  const { charts, skipped } = buildReleaseCharts(reports);
  for (const s of skipped) console.log(`skipped ${s.reason}`);
  if (charts.length === 0) {
    console.log('no release trends drawn');
  } else {
    mkdirSync(outDir, { recursive: true });
    for (const { filename, svg } of charts) writeFileSync(join(outDir, filename), svg);
    console.log(`${charts.length} release trends written to ${outDir}`);
  }
}
