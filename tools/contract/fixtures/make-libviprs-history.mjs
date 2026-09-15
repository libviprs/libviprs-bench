#!/usr/bin/env node
// Build `history.libviprs.json` from a real archived libviprs-storage document.
//
//   node fixtures/make-libviprs-history.mjs <archived-run.json> > fixtures/history.libviprs.json
//
// The fixture exists so the two libviprs verdict behaviours can be rendered and
// read rather than argued about. It has two entries:
//
//   run 1  every field taken from the archived document named on the command
//          line. Real numbers, real series ids, real replicate spreads.
//   run 2  the SAME cells with medians moved by hand, each by an amount chosen
//          to land on one verdict kind. This run is CONSTRUCTED. It is a
//          renderer fixture and nothing else: it is not a measurement, it must
//          never be imported into a published history, and every entry in it
//          carries `constructed: true` so that a reader and a gate can both see
//          so.
//
// The reason for constructing the second run rather than waiting for a second
// capture is that the behaviours under test are about the DELTA between two
// runs, and there is exactly one archived run today. Pinning the fixture to the
// real one for run 1 keeps the series ids, the scenario names, the units and
// the spreads honest, which is the part a stand-in would get wrong.

import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';

const src = process.argv[2];
if (!src) {
  console.error('usage: make-libviprs-history.mjs <archived-run.json>');
  process.exit(2);
}
const raw = readFileSync(src);
const doc = JSON.parse(raw.toString('utf8'));

const spreads = doc.replicate?.spreadPct ?? {};
const fingerprint = [
  doc.provenance.os,
  doc.provenance.arch,
  doc.provenance.cpuModel,
  doc.provenance.ncpu,
  doc.provenance.inContainer,
  doc.provenance.emulated,
].join('|');

/** The cells the fixture keeps: one per verdict kind we need to see, plus the
 *  headline one. Keyed `<backend>.<key>` @ scale. */
const WANTED = [
  // The metric whose replicate spread is 53.3%. A 20% move on it is what an
  // idle machine produces, and upstream's 10% line would call that a
  // regression. This is the cell the noise rule exists for.
  { backend: 'directory', key: 'read_concurrent@4.lookups_per_s', source: 'gradient', deltaPct: 20, want: 'noise' },
  // Same metric, a move far outside the spread: still a regression.
  { backend: 'pmtiles', key: 'read_concurrent@4.lookups_per_s', source: 'gradient', deltaPct: 80, want: 'regressed' },
  // A tight metric moved less than its spread.
  { backend: 'pmtiles', key: 'read_random.p50', source: 'gradient', deltaPct: 1, want: 'noise' },
  // A tight metric moved well past it.
  { backend: 'directory', key: 'read_random.p50', source: 'gradient', deltaPct: 25, want: 'regressed' },
  // Measured, not gated: no verdict chip whatever the delta says.
  { backend: 'pmtiles', key: 'generate.wall', source: 'gradient', deltaPct: 40, want: 'ungated', gated: false },
  // No measured spread, so nothing can tell a real move from noise.
  { backend: 'directory', key: 'generate.wall', source: 'gradient', deltaPct: 40, want: 'unknown', noSpread: true },
];

const find = (backend, key, source) =>
  doc.cells.find((c) => c.backend === backend && c.key === key && c.source === source && c.outcome === 'ok');

function sampleOf(w, { run }) {
  const cell = find(w.backend, w.key, w.source);
  if (!cell) throw new Error(`no ok cell for ${w.backend}/${w.key}/${w.source} in ${src}`);
  const spreadKey = `${w.backend}.${w.key}`;
  const spread = w.noSpread ? null : (spreads[spreadKey] ?? null);
  if (!w.noSpread && spread === null) throw new Error(`no replicate spread recorded for ${spreadKey}`);
  const factor = run === 1 ? 1 : 1 + w.deltaPct / 100;
  return {
    library: w.backend,
    // `sections.scenarioFrom` / `scaleFrom` in config.json: the importer joins
    // key and source into the scenario and keeps the tile count as the scale.
    scenario: `${w.key} · ${w.source}`,
    scale: cell.scale,
    cell: cell.cell,
    key: w.key,
    source: w.source,
    unit: cell.unit,
    direction: cell.direction,
    medianMs: Number((cell.median * factor).toFixed(6)),
    p95Ms: Number((cell.p95OfSamples * factor).toFixed(6)),
    reps: cell.reps,
    cov: cell.cov,
    confidence: cell.confidence,
    lowConfidenceReasons: cell.lowConfidenceReasons,
    timerSaturated: cell.timerSaturated,
    replicateSpreadPct: spread,
    // No cell in the archived capture carries a `gated` field at all, so run 1
    // carries null exactly as the importer would: "nobody said" is not the same
    // fact as "not gated", and only the literal false triggers the ungated
    // behaviour. A constructed `gated: false` appears in run 2 only, where
    // everything is constructed and says so.
    gated: run === 2 && 'gated' in w ? w.gated : null,
    expectedVerdict: run === 1 ? null : w.want,
  };
}

const base = {
  source: 'libviprs-bench',
  host: {
    os: doc.provenance.os,
    arch: doc.provenance.arch,
    cpuModel: doc.provenance.cpuModel,
    ncpu: doc.provenance.ncpu,
    inContainer: doc.provenance.inContainer,
    emulated: doc.provenance.emulated,
    fingerprint,
    fsType: doc.provenance.filesystem.fsType,
  },
  libraries: {
    pmtiles: { package: 'libviprs', version: doc.provenance.library.version },
    directory: { package: 'libviprs', version: doc.provenance.library.version },
  },
};

const history = [
  {
    ...base,
    capturedAt: doc.startedAt,
    version: doc.integrity.document,
    runId: doc.integrity.document,
    constructed: false,
    derivedFrom: {
      file: src.split('/').pop(),
      documentDigest: doc.integrity.document,
      sha256OfFile: `sha256:${createHash('sha256').update(raw).digest('hex')}`,
    },
    samples: WANTED.map((w) => sampleOf(w, { run: 1 })),
    skipped: doc.cells
      .filter((c) => c.outcome !== 'ok' && c.source === 'gradient' && c.cell === '2048x2048@256+gradient')
      .map((c) => ({
        library: c.backend,
        scenario: `${c.key} · ${c.source}`,
        scale: c.scale,
        status: 'SKIP',
        outcome: c.outcome,
        reason: c.reason,
      })),
  },
  {
    ...base,
    capturedAt: '2026-09-15T14:57:07.877Z',
    version: 'CONSTRUCTED-FIXTURE-RUN-2',
    runId: 'CONSTRUCTED-FIXTURE-RUN-2',
    constructed: true,
    constructedWhy:
      'Renderer fixture. The medians here were moved by hand from run 1 by the percentages in ' +
      'fixtures/make-libviprs-history.mjs so that each verdict kind is reachable. These are not ' +
      'measurements and must never be imported into a published history.',
    samples: WANTED.map((w) => sampleOf(w, { run: 2 })),
    skipped: [],
  },
];

process.stdout.write(`${JSON.stringify(history, null, 2)}\n`);
