#!/usr/bin/env node
// Extract the axis vocabulary of a real archived libviprs run.
//
//   node fixtures/make-producer-vocabulary.mjs <archived-run.json> > fixtures/producer-vocabulary.json
//
// `config.json` names series, scenarios, metrics, outcomes, invariants and a
// set of dotted field paths. The acceptance criterion for all of that is that
// it names only things the producer actually emits, and the only way to hold
// that true a year from now is to check it against the document in a test.
//
// The document is 812 KB of numbers and lives outside this repository, so what
// gets committed is its VOCABULARY: every distinct value on every axis the
// config names, plus the digest of the document it came from. No measurement is
// copied, and nothing here can be mistaken for one.

import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';

const src = process.argv[2];
if (!src) {
  console.error('usage: make-producer-vocabulary.mjs <archived-run.json>');
  process.exit(2);
}
const raw = readFileSync(src);
const doc = JSON.parse(raw.toString('utf8'));
const uniq = (f) => [...new Set(doc.cells.map(f))].sort();

/** Dotted paths that exist on the document, so `config.json`'s field paths can
 *  be checked rather than trusted. Recorded as paths, never values: a CPU model
 *  and a scratch directory are not vocabulary. */
function paths(node, prefix = '', out = [], depth = 0) {
  if (depth > 3 || node === null || typeof node !== 'object' || Array.isArray(node)) return out;
  for (const [k, v] of Object.entries(node)) {
    const p = prefix ? `${prefix}.${k}` : k;
    out.push(p);
    paths(v, p, out, depth + 1);
  }
  return out;
}

const vocabulary = {
  derivedFrom: {
    file: src.split('/').pop(),
    runId: `${doc.startedAt.replace(/[-:]/g, '').replace(/\.\d+Z$/, 'Z')}`,
    documentDigest: doc.integrity.document,
    sha256OfFile: `sha256:${createHash('sha256').update(raw).digest('hex')}`,
    schemaVersion: doc.schemaVersion,
    family: doc.family,
    runner: doc.runner,
    profile: doc.profile,
  },
  counts: {
    cells: doc.cells.length,
    ok: doc.cells.filter((c) => c.outcome === 'ok').length,
    lowConfidence: doc.cells.filter((c) => c.outcome === 'ok' && c.confidence === 'low').length,
    timerSaturated: doc.cells.filter((c) => c.outcome === 'ok' && c.timerSaturated).length,
    invariants: doc.invariants.length,
    modelled: doc.modelled.length,
    spreadMetrics: Object.keys(doc.replicate?.spreadPct ?? {}).length,
  },
  backends: uniq((c) => c.backend),
  scenarios: uniq((c) => c.scenario),
  metrics: uniq((c) => c.metric),
  keys: uniq((c) => c.key),
  cellNames: uniq((c) => c.cell),
  sources: uniq((c) => c.source),
  scales: uniq((c) => c.scale),
  units: uniq((c) => c.unit),
  directions: uniq((c) => c.direction),
  outcomes: uniq((c) => c.outcome),
  confidence: uniq((c) => c.confidence),
  cellFields: [...new Set(doc.cells.flatMap((c) => Object.keys(c)))].sort(),
  invariantNames: [...new Set(doc.invariants.map((i) => i.name))].sort(),
  modelledNames: [...new Set(doc.modelled.map((i) => i.name))].sort(),
  replicateSpreadKeys: Object.keys(doc.replicate?.spreadPct ?? {}).sort(),
  documentPaths: paths(doc).sort(),
  // Tile count is not unique across cells, which is why `sections.scaleFrom`
  // is only sound with `source` in the scenario. Recorded so a test can assert
  // it rather than a comment claiming it.
  scaleToCells: Object.fromEntries(
    [...new Set(doc.cells.map((c) => c.scale))]
      .sort((a, b) => a - b)
      .map((s) => [s, [...new Set(doc.cells.filter((c) => c.scale === s).map((c) => c.cell))].sort()]),
  ),
  structuralFailureReasons: [
    ...new Set(doc.cells.filter((c) => c.outcome !== 'ok').map((c) => `${c.outcome}: ${c.reason}`)),
  ].sort(),
  replicate: {
    cell: doc.replicate?.cell ?? null,
    replicateReps: doc.replicate?.replicateReps ?? null,
    medianSpreadPct: (() => {
      const v = Object.values(doc.replicate?.spreadPct ?? {}).sort((a, b) => a - b);
      return v.length ? v[Math.floor(v.length / 2)] : null;
    })(),
    maxSpreadPct: Math.max(...Object.values(doc.replicate?.spreadPct ?? { x: 0 })),
    maxSpreadKey:
      Object.entries(doc.replicate?.spreadPct ?? {}).sort((a, b) => b[1] - a[1])[0]?.[0] ?? null,
  },
  measurement: {
    timerTickNs: doc.measurement?.timerTickNs ?? null,
    minTicksPerSample: doc.measurement?.minTicksPerSample ?? null,
    covLowConfidence: doc.measurement?.covLowConfidence ?? null,
  },
};

process.stdout.write(`${JSON.stringify(vocabulary, null, 2)}\n`);
