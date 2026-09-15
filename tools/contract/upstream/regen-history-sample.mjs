#!/usr/bin/env node
// Regenerate history.sample.json from history.json.
//
// Purpose:
//   history.sample.json is a preload/fallback stub used by dashboard.js
//   when a mirror serves a 404 (or stale) history.json. Because the
//   stub must match the live shape, this script keeps it in sync by
//   slicing the latest entries off the source-of-truth history.json.
//
// Rule:
//   stub = the last 3 HistoryEntries, PROJECTED to the fields
//          dashboard.js reads, written one entry per line.
//
//   The last 3 entries cover every render path currently used by the
//   dashboard (OK samples across all libraries, skipped[] rows,
//   typed-skip rows, bytes-distribution fields). Three entries is
//   enough to exercise the time-series renderer without bloating the
//   stub.
//
// ## Why the stub is projected and not a verbatim slice
//
// It used to be a verbatim slice, which was fine while an entry held
// ~130 samples of five fields each. The causl-bench sweeps now carry
// ~360 samples with intervals, dispersion, confidence reasons,
// attestation and hold-out reasons attached, and a verbatim slice of
// three of those came to 754 KB, pretty-printed, and named in a
// `<link rel="preload">` in index.html, so every visitor downloaded it
// on the critical path whether or not the fallback was ever needed. A
// fallback that costs more than the file it stands in for is not a
// fallback.
//
// So the stub carries exactly what dashboard.js reads, and nothing else:
//
//   entry    capturedAt, version, samples[], skipped[]
//   sample   library, scenario, scale, medianMs, p95Ms,
//            boundaryCrossings, boundaryBytes*  (when present)
//   skipped  library, scenario, scale, status, reason, detail
//
// What the stub drops (intervals, CoV, confidence, recomputes, engine
// attestation, rankability) is all in history.json, which the
// dashboard promotes to as soon as it resolves and which the page links
// directly. Nothing is lost; it is only off the preload path.
//
// Usage:
//   node pages/benchmarks/regen-history-sample.mjs
//
// CI:
//   Run this on every change to pages/benchmarks/history.json so the
//   stub never drifts again (see issue #14).

import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const srcPath = join(here, 'history.json');
const dstPath = join(here, 'history.sample.json');

const SAMPLE_SIZE = 3;

/** The fields dashboard.js reads off a sample. Optional ones are copied only
 *  when present, so a projected row is indistinguishable from a verbatim one
 *  on every code path in the renderer. */
const SAMPLE_FIELDS = [
  'library',
  'scenario',
  'scale',
  'medianMs',
  'p95Ms',
  'boundaryCrossings',
  'boundaryBytes',
  'boundaryBytesMin',
  'boundaryBytesMedian',
  'boundaryBytesMax',
];

/** …and off a skipped row. `status` plus `reason` drive the typed-chip
 *  taxonomy; `detail` is its elaboration. */
const SKIPPED_FIELDS = ['library', 'scenario', 'scale', 'status', 'reason', 'detail'];

function project(row, fields) {
  const out = {};
  for (const f of fields) if (row[f] !== undefined) out[f] = row[f];
  return out;
}

const src = JSON.parse(readFileSync(srcPath, 'utf8'));
if (!Array.isArray(src)) {
  throw new Error(`expected ${srcPath} to be a JSON array, got ${typeof src}`);
}
if (src.length === 0) {
  throw new Error(`${srcPath} is empty; refusing to write an empty stub`);
}

const stub = src.slice(-SAMPLE_SIZE).map((entry) => ({
  capturedAt: entry.capturedAt,
  version: entry.version,
  samples: (entry.samples ?? []).map((s) => project(s, SAMPLE_FIELDS)),
  skipped: (entry.skipped ?? []).map((s) => project(s, SKIPPED_FIELDS)),
}));

// One entry per line, matching history.json: compact enough for a preload,
// still exactly one added line per run in a diff.
writeFileSync(dstPath, `[\n${stub.map((e) => JSON.stringify(e)).join(',\n')}\n]\n`);

const kb = readFileSync(dstPath).length / 1024;
console.log(
  `wrote ${dstPath}: ${stub.length} entr${stub.length === 1 ? 'y' : 'ies'} ` +
    `(from ${src.length} in history.json), ${kb.toFixed(1)} KB`,
);

// The stub is on the preload path, so its size is a cost every visitor pays
// before the dashboard needs it. Refuse rather than let it creep back.
const BUDGET_KB = 256;
if (kb > BUDGET_KB) {
  console.error(
    `\nREFUSED: the stub is ${kb.toFixed(1)} KB, over the ${BUDGET_KB} KB preload budget.\n` +
      '  index.html names it in <link rel="preload">, so this is paid on every page load.\n' +
      '  Narrow SAMPLE_FIELDS, or lower SAMPLE_SIZE.\n',
  );
  process.exit(1);
}
