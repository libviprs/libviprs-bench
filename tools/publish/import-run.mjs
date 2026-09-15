#!/usr/bin/env node
// Import one archived libviprs-bench document into the history the page reads.
//
// This is causl's `pages/benchmarks/import-run.mjs` with the libviprs
// differences expressed as configuration rather than edited in. Causl's file
// opens by saying that none of the producer's gates are reachable from here,
// because a `combined.json` handed to a script is just a file, so the script
// re-checks what is checkable from the artefact itself and refuses the rest.
// That is exactly the position here, and `archive/storage/README.md` opens with
// the same sentence:
//
//   "Every number the libviprs benchmark page draws comes from a file in this
//    directory, and a file gets in here only if the run it describes can say
//    what produced it."
//
// What this file keeps from causl, unchanged in intent:
//
//   * the run must be ARCHIVED: its digests must appear against a run id in the
//     archive's `index.json`, because a document sitting in a working tree is
//     not citable;
//   * the tree must have been CLEAN;
//   * at least one cell must be `ok`, because an empty reading is a refusal and
//     not a result;
//   * a run measured mostly under contention is refused, on the cause
//     (`machineLoad.quiet`) rather than on the consequence;
//   * every reason is reported, never the first;
//   * an unknown flag is a usage error and not a silent no-op;
//   * re-importing the same run replaces its entry rather than appending a
//     second copy;
//   * one compact entry per line, so adding a run is one added line.
//
// What is new here, and why each one is real rather than theoretical:
//
//   * **the digests are recomputed, not trusted.** Causl matches the integrity
//     block against the archive index. That catches a swapped file and not an
//     edited one, so this side canonicalises the document and derives the four
//     digests itself. A number nobody can tie to a file is not evidence.
//   * **an emulated or unknown run is refused.** The published PMTiles numbers
//     this epic replaced were almost certainly Rosetta and nothing recorded it.
//     The probe's verdict is in the document; anything but exactly `false` is a
//     refusal, absent included, because an unobserved run is not a native one.
//   * **a non-publishable profile is refused.** `ci` proves the harness runs, is
//     never a measurement, and archives indistinguishably from `full`.
//   * **an invariant that moved within one commit is refused.** That is a
//     defect, not a delta: `filesystem_entries`, `tiles_produced` and
//     `output_bytes` reproduced byte for byte across every export this epic has
//     seen. Across commits the same change is a finding and is imported.
//   * **a debug build is refused.** Already refused at the archive door; this is
//     the door it must not get back in through.
//
// And what it carries through rather than drops: the replicate spread, because
// the page draws bands from it and calls a delta `noise` when it is covered;
// `confidence` and `lowConfidenceReasons` on every cell, because 106 of the 332
// measured cells in the first real capture were low-confidence for timer
// saturation; the `gated` flag, so an ungateable metric never gets a verdict
// chip; and the invariants, which are the epic's actual claim and are exact.
//
// Usage, where everything but the document defaults out of the config:
//   node tools/publish/import-run.mjs --document archive/storage/<runId>.json
//     [--archive  archive/<family>]                 producer.archiveRoot
//     [--history  tools/publish/history.json]       producer.historyPath
//     [--config   tools/contract/config.json]       then tools/publish/config.json
//     [--baseline archive/storage/baseline-<host8>.json]
//     [--dry-run]
//
// Exit codes: 0 imported · 1 refused · 2 usage.

import { createHash } from 'node:crypto';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { canonicalJson, computeDigests, CanonicalError } from './canonical-json.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, '..', '..');

const EXIT = { OK: 0, REFUSED: 1, USAGE: 2 };

// --- the invocation ----------------------------------------------------------

const argv = process.argv.slice(2);
const flag = (name) => {
  const i = argv.indexOf(`--${name}`);
  return i === -1 ? undefined : argv[i + 1];
};
const has = (name) => argv.includes(`--${name}`);

const KNOWN = new Set(['document', 'archive', 'history', 'config', 'baseline', 'dry-run', 'help']);
const unknown = argv
  .filter((a) => a.startsWith('--'))
  .map((a) => a.slice(2).split('=')[0])
  .filter((a) => !KNOWN.has(a));
if (unknown.length > 0) {
  console.error(`unknown flag(s): ${unknown.map((u) => `--${u}`).join(', ')}`);
  console.error(`known flags: ${[...KNOWN].map((k) => `--${k}`).join(' ')}`);
  process.exit(EXIT.USAGE);
}

if (has('help') || !flag('document')) {
  console.error(
    'usage: node tools/publish/import-run.mjs --document <document.json> [--archive <dir>]\n' +
      '                                        [--history <history.json>] [--config <config.json>]\n' +
      '                                        [--baseline <baseline.json>] [--dry-run]',
  );
  process.exit(EXIT.USAGE);
}

// K2.3 (#76) owns `tools/contract/config.json`; the copy beside this file is
// provisional and exists only so this lane's tests can run before that lands.
// Preferring the contract copy means the compose is a deletion and not an edit.
const CONFIG_CANDIDATES = [
  join(repoRoot, 'tools', 'contract', 'config.json'),
  join(here, 'config.json'),
];
const configPath = resolve(flag('config') ?? CONFIG_CANDIDATES.find((p) => existsSync(p)) ?? '');
if (!configPath || !existsSync(configPath)) {
  console.error(`refused: no config at ${configPath || CONFIG_CANDIDATES.join(' or ')}`);
  process.exit(EXIT.REFUSED);
}
const config = JSON.parse(readFileSync(configPath, 'utf8'));
const producer = config.producer ?? {};
const sections = config.sections ?? {};

/** A config path relative to the repository root unless it is already absolute. */
const fromRepo = (p) => (isAbsolute(p) ? p : join(repoRoot, p));

/** Follow a dotted path into an object. */
const at = (root, path) => path.split('.').reduce((n, k) => (n == null ? undefined : n[k]), root);

const documentPath = resolve(flag('document'));
const historyPath = resolve(
  flag('history') ?? fromRepo(producer.historyPath ?? 'tools/publish/history.json'),
);
const baselinePath = flag('baseline') ? resolve(flag('baseline')) : null;

if (!existsSync(documentPath)) {
  console.error(`refused: ${documentPath} does not exist`);
  process.exit(EXIT.REFUSED);
}

const refusals = [];
const refuse = (why) => refusals.push(why);

let doc;
try {
  doc = JSON.parse(readFileSync(documentPath, 'utf8'));
} catch (e) {
  console.error(`refused: ${documentPath} is not JSON (${e.message})`);
  process.exit(EXIT.REFUSED);
}

// --- 1. the document is one of ours -----------------------------------------
//
// The schema version is per document family, so "version 1" means nothing
// without the family name beside it. Refusing an unknown family here is what
// stops an `engines` document being read with the `storage` reader's
// assumptions about what a cell holds.

const families = producer.families ?? [];
const familyPrefix = producer.familyIdPrefix ?? '';
const knownFamily = families.includes(doc.family);
// `libviprs-storage` names the document family and `storage` names the page's
// tab; the prefix rule keeps one of them from being a second list that drifts.
const family = knownFamily
  ? { name: doc.family, id: String(doc.family).replace(familyPrefix, '') }
  : null;
if (!family) {
  refuse(
    `family ${JSON.stringify(doc.family ?? null)} is not one this config knows: ` +
      `${families.join(', ') || 'none configured'}`,
  );
} else {
  // One version or a list of them. A list is not laxity: version 2 redefined
  // `replicate.spreadPct` from the gap between two measurements of the control
  // cell to a dispersion over every placement of it, and both are readable, but
  // they are different statistics. What keeps them apart is the `documentSchema`
  // era axis, which puts a version-1 run and a version-2 run on separate x-axes
  // rather than drawing one line through both (libviprs-bench #84). A version
  // this config does not name is still refused outright.
  const known = [producer.schemaVersion ?? 1].flat();
  if (!known.includes(doc.schemaVersion)) {
    refuse(
      `schemaVersion ${JSON.stringify(doc.schemaVersion ?? null)} is not a version this ` +
        `config reads for ${doc.family} (${known.join(', ')}); the numbering is per family ` +
        'and a reader that guesses reads a different document',
    );
  }
  // The runner is a constant per family, so it says the same thing twice and a
  // disagreement between the two means the document was assembled by hand.
  if (doc.runner !== doc.family) {
    refuse(
      `runner ${JSON.stringify(doc.runner ?? null)} does not match family ` +
        `${JSON.stringify(doc.family)}`,
    );
  }
}

// --- 2. the digests hold ----------------------------------------------------
//
// Causl matches the integrity block against the archive index, which catches a
// swapped file. It cannot catch an edited one, because an edited document
// carries whatever digests the editor left in it and the index was written
// before the edit. So the digests are recomputed here from the document's own
// bytes and then joined to the index, and both halves have to agree.

let statedIntegrity = null;
let recomputed = null;
if (!doc.integrity || typeof doc.integrity !== 'object') {
  refuse(
    'the document carries no `integrity` block, so there is nothing to verify and nothing ' +
      'to match against the archive',
  );
} else {
  statedIntegrity = doc.integrity;
  try {
    recomputed = computeDigests(doc);
    const moved = ['cells', 'runners', 'measurements', 'document'].filter(
      (block) => statedIntegrity[block] !== recomputed[block],
    );
    if (moved.length > 0) {
      refuse(
        `the document's digests do not verify: ${moved.join(', ')} moved. ` +
          moved
            .map((b) => `${b} states ${statedIntegrity[b] ?? 'nothing'} and digests ${recomputed[b]}`)
            .join('; '),
      );
    }
  } catch (e) {
    if (e instanceof CanonicalError) {
      refuse(`the document cannot be canonicalised, so its digests cannot be checked: ${e.message}`);
    } else {
      throw e;
    }
  }
}

// --- 3. the run is archived --------------------------------------------------

/** `2026-09-14T14:57:07.877Z` into `20260914T145707Z`. */
function compactTimestamp(iso) {
  const withoutFraction = iso.includes('.')
    ? `${iso.split('.')[0]}${iso.endsWith('Z') ? 'Z' : ''}`
    : iso;
  return [...withoutFraction].filter((c) => /[0-9a-zA-Z]/.test(c)).join('');
}

/** The environment bucket: the first eight hex of a sha256 over the six fields
 *  that decide whether two runs are comparable at all.
 *
 *  NUL-separated rather than concatenated, so an os of `linu` with an arch of
 *  `xaarch64` cannot land in the same bucket as `linux` with `aarch64`. The
 *  emulation verdict is stringified the way the producer's `Value::to_string`
 *  does, so `"unknown"` carries its quotes and an absent key is the literal
 *  `absent`: an unobserved run must not share a bucket with an observed one.
 */
function fingerprint(d, paths) {
  const parts = paths.map((path) => {
    const value = at(d, path);
    if (value === undefined) return 'absent';
    // The producer stringifies through `Value::to_string`, so a string keeps its
    // quotes and a bool does not. `"unknown"` and `unknown` must not collide:
    // an unobserved run does not share a bucket with an observed one.
    return typeof value === 'string' ? (value || 'unknown') : JSON.stringify(value);
  });
  return createHash('sha256').update(parts.join('\0'), 'utf8').digest('hex').slice(0, 8);
}

const ARCHIVE_BUCKET_FROM = producer.archiveBucketFrom ?? [
  'provenance.os',
  'provenance.arch',
  'provenance.cpuModel',
  'provenance.node.rustc',
  'provenance.filesystem.fsType',
  'provenance.emulated',
];
const HOST_FINGERPRINT_FROM = producer.hostFingerprintFrom ?? ARCHIVE_BUCKET_FROM;

/** The bucket the producer files a run under: the last segment of every run id. */
const host8 = (d) => fingerprint(d, ARCHIVE_BUCKET_FROM);

/** The run id the document derives, never the one an index states. */
function runIdFor(d) {
  const startedAt = typeof d.startedAt === 'string' ? d.startedAt : null;
  const commit = d.provenance?.library?.commit;
  if (!startedAt || !commit) return null;
  return `${compactTimestamp(startedAt)}-${commit}-${host8(d)}`;
}

const derivedRunId = runIdFor(doc);
// The document's own `runId` is NOT what this keys on, and today it could not
// be: the producer writes the field and has never populated it, so every
// archived document carries `runId: null` (fixed on lane/k2.2-engines-document).
// Keying on it would give one null-keyed entry per run and an idempotency that
// works because everything collides. The id is derived from the run's own
// evidence and joined to the archive index instead, which is what makes
// re-importing replace rather than append. When the producer does start writing
// it, a stated id that disagrees with the derived one is a defect and says so.
if (typeof doc.runId === 'string' && doc.runId.length > 0 && doc.runId !== derivedRunId) {
  refuse(
    `the document states runId ${JSON.stringify(doc.runId)} and its own evidence derives ` +
      `${JSON.stringify(derivedRunId)}; the id is a function of the run, so a disagreement ` +
      'means one of the two was written by hand',
  );
}
if (!derivedRunId) {
  refuse(
    'the document has no `startedAt` or no `provenance.library.commit`, so it cannot derive ' +
      'the run id the archive files it under',
  );
}

// Two families archive now, each under `archive/<family>/`, so the default
// cannot be resolved before the document has said which family it is.
// `archive/<family without its prefix>`, which is `archive::dir_for_family` on
// the producer side. One directory and one index per family, never a shared
// one: two families derive their run ids from the same fields, so a `storage`
// and an `engines` sweep started in the same second against the same commit on
// the same host derive the same id, and in one directory the second would look
// like a collision.
const archiveDir = resolve(
  flag('archive') ??
    fromRepo(
      producer.archiveDirByFamily?.[doc.family] ??
        join(producer.archiveRoot ?? 'archive', family?.id ?? 'unknown'),
    ),
);

const indexPath = join(archiveDir, producer.documentIndex ?? 'index.json');
let archivedRow = null;
if (!existsSync(indexPath)) {
  refuse(
    `the archive at ${archiveDir} has no index.json, so this run is not citable, and ` +
      'archive/storage/README.md forbids publishing from a run that is not archived',
  );
} else if (derivedRunId) {
  let rows = [];
  try {
    rows = JSON.parse(readFileSync(indexPath, 'utf8'));
  } catch (e) {
    refuse(`the archive index at ${indexPath} is not JSON (${e.message})`);
  }
  if (!Array.isArray(rows)) rows = [];
  archivedRow = rows.find((r) => r?.runId === derivedRunId) ?? null;
  if (!archivedRow) {
    refuse(
      `no row in ${indexPath} carries the run id ${derivedRunId} that this document derives: ` +
        `the archive index holds ${rows.length} run(s) and none of them is this one. A sealed ` +
        'document in a working tree is not a citable artefact; archive it with ' +
        '`storage-aggregate --archive`.',
    );
  } else if (statedIntegrity && archivedRow.documentDigest !== statedIntegrity.document) {
    refuse(
      `the archive index records documentDigest ${archivedRow.documentDigest} for run id ` +
        `${derivedRunId} and this document carries ${statedIntegrity.document}. One of the two ` +
        'has been edited since the run was filed.',
    );
  }
}

// --- 4. the run may be published ---------------------------------------------

const prov = doc.provenance ?? {};

// The probe's verdict, read strictly. `true`, `"unknown"`, `null` and absent are
// all refusals: an unobserved run is not a native one, and "absent" is the exact
// state the published PMTiles numbers this epic replaced are in.
if (prov.emulated !== false) {
  const verdict = prov.emulated === undefined ? 'absent' : JSON.stringify(prov.emulated);
  refuse(
    `provenance.emulated is ${verdict} and only an observed \`false\` may be published. ` +
      'The numbers this page replaces were taken in a container that almost certainly ' +
      'translated every instruction, and nothing in the artefact recorded it.',
  );
} else if (!Array.isArray(prov.emulationEvidence) || prov.emulationEvidence.length === 0) {
  refuse(
    'provenance.emulated is false with no `emulationEvidence`, which is an assertion rather ' +
      'than an observation',
  );
}

const profile = doc.profile;
const publishable = producer.publishableProfiles ?? [];
if (!publishable.includes(profile)) {
  refuse(
    `profile ${JSON.stringify(profile ?? null)} is not publishable (${publishable.join(', ')}). ` +
      'A `ci` sweep proves the harness runs, takes three reps of a cut-down cell list, and ' +
      'archives indistinguishably from a calibrated one, so it is refused here rather than ' +
      'left to sit in the same era as a real sweep.',
  );
}
const resolvedProfile = prov.invocation?.resolved?.profile;
if (resolvedProfile !== undefined && resolvedProfile !== profile) {
  refuse(
    `the document says profile ${JSON.stringify(profile)} and its invocation resolved ` +
      `${JSON.stringify(resolvedProfile)}; a relabelled sweep is not a sweep`,
  );
}

for (const [label, value] of [
  ['provenance.dirty', prov.dirty],
  ['provenance.library.dirty', prov.library?.dirty],
]) {
  if (value !== false) {
    refuse(
      `${label} is ${value === undefined ? 'absent' : JSON.stringify(value)}; a missing flag ` +
        'is not a clean tree and a dirty one cannot say what produced the numbers',
    );
  }
}

const node = prov.node ?? {};
if (node.debugAssertions !== false) {
  refuse(
    `provenance.node.debugAssertions is ${
      node.debugAssertions === undefined ? 'absent' : JSON.stringify(node.debugAssertions)
    }; a debug build is refused at the archive door and must not get back in here`,
  );
}
if (node.buildProfile !== 'release') {
  refuse(
    `provenance.node.buildProfile is ${JSON.stringify(node.buildProfile ?? null)} and not ` +
      '`release`; a debug build measures the assertions as well as the work',
  );
}
for (const needle of producer.refuse?.perturbingRustflags ?? []) {
  if (typeof node.rustflags === 'string' && node.rustflags.includes(needle)) {
    refuse(`RUSTFLAGS carries ${needle}, which changes the code that was timed`);
  }
}
if (!prov.filesystem?.fsType || !prov.filesystem?.scratchDir) {
  refuse(
    'provenance.filesystem has no fsType or no scratchDir, so there is nothing to attach the ' +
      'filesystem era axis to',
  );
}

// --- 5. the cells --------------------------------------------------------------

const cells = Array.isArray(doc.cells) ? doc.cells : [];

// Three buckets rather than causl's two. `measured` is a number the page may
// draw. `structural` is an honest not-a-number: a directory tree has no root
// directory to decode, and four generate cells refused themselves because
// allocated_bytes moved between reps of one pyramid. `refusing` is the set whose
// presence refuses the whole run, and it is empty, because inheriting causl's
// "no cell may be failed" rule refuses this real capture for containing an
// impossibility it correctly reported. What survives is the run-level form: at
// least one cell must have been measured.
const OUTCOMES = producer.outcomes ?? {};
const MEASURED = new Set(OUTCOMES.measured ?? ['ok']);
const REFUSING = new Set(OUTCOMES.refusing ?? []);
const STRUCTURAL = new Set(OUTCOMES.structural ?? []);

const okCells = cells.filter((c) => MEASURED.has(c.outcome));

const unclassified = cells.filter(
  (c) => !MEASURED.has(c.outcome) && !REFUSING.has(c.outcome) && !STRUCTURAL.has(c.outcome),
);
if (unclassified.length > 0) {
  const kinds = [...new Set(unclassified.map((c) => JSON.stringify(c.outcome ?? null)))];
  refuse(
    `${unclassified.length} cell(s) carry an outcome this config does not classify ` +
      `(${kinds.join(', ')}). A new outcome the importer has never seen must be a refusal ` +
      'rather than a silent skip, or the day the producer adds one the page quietly loses ' +
      'every cell that has it.',
  );
}

const refusingCells = cells.filter((c) => REFUSING.has(c.outcome));
if (refusingCells.length > 0) {
  refuse(
    `${refusingCells.length} cell(s) carry an outcome this config refuses on: ` +
      refusingCells.slice(0, 5).map((c) => `${c.backend}/${c.key}@${c.scale} (${c.outcome})`).join(', '),
  );
}

if (okCells.length === 0) {
  refuse(
    `the sweep published ${cells.length} cell(s) and none of them is \`ok\`, so there is no ` +
      'measurement here to publish. An empty reading is a refusal, not a result.',
  );
}

// Inherited from causl and, today, unreachable: `--allow-dirty` stamps
// `dirty: true` on every cell so a reader quoting one cell knows, and the
// producer has never written the field (found in K2.2). The check stays because
// the flag is meant to work and the aggregator refuses a dirty run that is not
// stamped, so the day it does the page must not be the last to hear.
const dirtyCells = cells.filter((c) => c.dirty === true);
if (dirtyCells.length > 0) {
  refuse(
    `${dirtyCells.length} cell(s) are stamped \`dirty\` and were measured against a tree with ` +
      'uncommitted changes in it',
  );
}

// The field is `attested` on `lane/k2.2-engines-document` and `storageAttested`
// in every document archived before it: widening the cell shape to a second
// family is what turned a field name into a family name. Both names are read, in
// the order the config lists them, and a cell carrying NEITHER is a different
// refusal from a cell carrying `false`. That distinction is the whole point of
// naming them here: read only the new name and every old document comes back
// `undefined`, which reads as "unattested" and refuses the entire archive, and
// read only the old one and every new document does the same. Either way the
// importer looks like a gate doing its job while it is really answering a
// question nobody asked.
const ATTESTED_FROM = producer.attestedFrom ?? ['attested', 'storageAttested'];
const attestationOf = (cell) => {
  for (const field of ATTESTED_FROM) {
    if (cell[field] !== undefined) return { field, value: cell[field] };
  }
  return { field: null, value: undefined };
};

const unnamed = okCells.filter((c) => attestationOf(c).field === null);
if (unnamed.length > 0) {
  refuse(
    `${unnamed.length} measured cell(s) carry none of the attestation fields this config ` +
      `names (${ATTESTED_FROM.join(', ')}), so nothing here has looked at whether they were ` +
      'observed. This is a document of a shape the importer has not been taught, not a run ' +
      'that failed attestation.',
  );
}

const unattested = okCells.filter((c) => {
  const { field, value } = attestationOf(c);
  return field !== null && value !== true;
});
if (unattested.length > 0) {
  refuse(
    `${unattested.length} cell(s) claim \`ok\` without \`attested: true\`: ` +
      unattested.slice(0, 5).map((c) => `${c.backend}/${c.key}@${c.scale}`).join(', ') +
      (unattested.length > 5 ? ', …' : '') +
      '. Attestation is observed in the measuring process, never asserted, and a number with ' +
      'no witness is not a measurement of the thing it is labelled with.',
  );
}

// The fields this reader needs from a measured cell, named by the config rather
// than assumed. The `engines` family is being built in another lane and its
// document may not have this cell shape; without this check `scenarioOf` would
// quietly shorten a section name and `direction` would come out null, which is a
// page that is wrong rather than a page that is missing.
const REQUIRED_CELL_FIELDS = [
  ...new Set([
    producer.seriesFrom ?? 'backend',
    ...(sections.scenarioFrom ?? ['key']),
    sections.scaleFrom ?? 'scale',
    sections.unitFrom ?? 'unit',
    sections.directionFrom ?? 'direction',
  ]),
];
for (const field of REQUIRED_CELL_FIELDS) {
  const missing = okCells.filter((c) => c[field] === undefined || c[field] === null);
  if (missing.length > 0) {
    refuse(
      `${missing.length} measured cell(s) carry no \`${field}\`, which this config names as ` +
        'part of the series, the section or its axis. A document of another shape must be ' +
        'refused rather than read with this one\'s assumptions.',
    );
  }
}

const unexplained = cells.filter(
  (c) => !MEASURED.has(c.outcome) && (typeof c.reason !== 'string' || c.reason.length === 0),
);
if (unexplained.length > 0) {
  refuse(`${unexplained.length} non-ok cell(s) carry no reason`);
}

// The contamination check, kept from causl and kept on the cause rather than the
// consequence. `machineLoad.quiet` is an observation the runner took while it
// measured the cell; "how many cells stayed high-confidence" is a consequence,
// and picking a cut-off on a consequence means inventing a number. A simple
// majority is the line: past it, the typical cell of the run was measured while
// other work was on the CPU, and no number of repetitions removes competing work.
if (producer.refuse?.majorityNoisyCells !== false) {
  const withLoad = okCells.filter((c) => typeof c.machineLoad?.quiet === 'boolean');
  const noisy = withLoad.filter((c) => c.machineLoad.quiet === false);
  if (withLoad.length > 0 && noisy.length > withLoad.length / 2) {
    const loads = noisy
      .map((c) => c.machineLoad.loadAvg1m)
      .filter(Number.isFinite)
      .sort((a, b) => a - b);
    const median = loads.length ? loads[Math.floor(loads.length / 2)] : null;
    refuse(
      `${noisy.length} of ${withLoad.length} measured cells record \`machineLoad.quiet: false\`, ` +
        'so the typical cell of this run was measured while other work was on the CPU' +
        (median === null ? '' : ` (median load average ${median})`) +
        '. Re-measure on an idle machine; repetitions do not remove competing work.',
    );
  }
}

// --- 6. the invariants, and one that moved inside a commit ---------------------

/** The invariants, as an equality table.
 *
 *  These are the epic's actual claim and they are exact: one archive entry
 *  against 22127 filesystem entries for the same pyramid. They are carried as
 *  rows with an `exact` flag and never folded into `samples`, because a page
 *  that charts them draws a flat line with a band it never had.
 */
const EXACT = new Set(producer.invariantsExactWithinCommit ?? []);
const KNOWN_INVARIANTS = new Set(producer.invariantNames ?? []);
const strayInvariants = [
  ...new Set((doc.invariants ?? []).map((r) => r.name).filter((n) => !KNOWN_INVARIANTS.has(n))),
];
if (strayInvariants.length > 0) {
  // A new invariant nobody classified is neither exact nor filesystem-dependent,
  // so it would be carried and never compared: a claim on the page that no
  // refusal can ever contradict.
  refuse(
    `invariant(s) ${strayInvariants.join(', ')} are not in the config's list, so nothing ` +
      'here knows whether they are exact within a commit',
  );
}
const invariants = (doc.invariants ?? []).map((row) => ({
  library: row.library,
  scale: row.scale,
  source: row.source,
  name: row.name,
  value: row.value,
  unit: row.unit,
  exact: EXACT.has(row.name),
}));

const modelled = (doc.modelled ?? []).map((row) => ({ ...row }));

let history = [];
if (!existsSync(historyPath)) {
  refuse(`no history at ${historyPath}`);
} else {
  try {
    history = JSON.parse(readFileSync(historyPath, 'utf8'));
  } catch (e) {
    refuse(`${historyPath} is not JSON (${e.message})`);
  }
  if (!Array.isArray(history)) {
    refuse(`${historyPath} is not a JSON array`);
    history = [];
  }
}

const libraryCommit = prov.library?.commit ?? null;
const harnessCommit = prov.commit ?? null;
const fsType = prov.filesystem?.fsType ?? null;
const mountSource = prov.filesystem?.mountSource ?? null;
const FS_DEPENDENT = new Set(producer.invariantsFilesystemDependent ?? []);

const invariantKey = (row) => `${row.library}/${row.source}@${row.scale}.${row.name}`;

/** Invariants that moved against a run of the same commit already in history.
 *
 *  Same commit means both trees: the harness generates the source image, so a
 *  harness-only change can move `output_bytes` honestly and refusing it would
 *  block publication with no way to clear it. Across commits the same difference
 *  is a finding the page renders as a step with the commit that moved it, which
 *  is why this is a comparison and not a rule against change.
 */
const moved = [];
for (const prior of history) {
  if (prior.runId === derivedRunId) continue;
  if (prior.commit !== libraryCommit || prior.harnessCommit !== harnessCommit) continue;
  const before = new Map((prior.invariants ?? []).map((r) => [invariantKey(r), r]));
  for (const row of invariants) {
    const was = before.get(invariantKey(row));
    if (!was) continue;
    const comparable = EXACT.has(row.name)
      ? true
      : FS_DEPENDENT.has(row.name) &&
        prior.filesystem?.fsType === fsType &&
        prior.filesystem?.mountSource === mountSource;
    if (!comparable) continue;
    if (was.value !== row.value) {
      moved.push(
        `${invariantKey(row)} was ${JSON.stringify(was.value)} in ${prior.runId} and is ` +
          `${JSON.stringify(row.value)} here`,
      );
    }
  }
}
if (moved.length > 0) {
  refuse(
    `${moved.length} invariant(s) moved within one commit, which is a defect rather than a ` +
      'delta: these reproduce byte for byte across every export this epic has seen, and a ' +
      'change at a fixed commit means something is wrong rather than something is slower. ' +
      moved.slice(0, 5).join('; ') +
      (moved.length > 5 ? `; and ${moved.length - 5} more` : ''),
  );
}


// --- the refusal gate ---------------------------------------------------------
//
// Before the entry is built, not after. A document that has already been refused
// can be malformed in ways the reader below does not survive, and a crash there
// prints a stack trace where the list of reasons should be: the run is refused
// either way, and the operator loses the one thing that tells them how many
// re-runs this is going to take.

if (refusals.length > 0) {
  console.error('REFUSED. This run may not be published:\n');
  for (const r of refusals) console.error(`  · ${r}\n`);
  process.exit(EXIT.REFUSED);
}

// --- 7. the entry -------------------------------------------------------------

const TO_MS = { ns: 1e-6, us: 0.001, ms: 1, s: 1000 };
const THROUGHPUT_UNITS = new Set(['1/s']);
const UNGATEABLE = producer.ungateableMetrics ?? [];
const DECLARED_SUFFIX = producer.declaredSuffix ?? '_declared';
const SERIES_FROM = producer.seriesFrom ?? 'backend';
const RUNNER_TO_SERIES = producer.runnerToSeries ?? {};

const SCENARIO_FROM = sections.scenarioFrom ?? ['key'];
const SCENARIO_JOIN = sections.scenarioJoin ?? ' · ';
const SCALE_FROM = sections.scaleFrom ?? 'scale';
const UNIT_FROM = sections.unitFrom ?? 'unit';
const DIRECTION_FROM = sections.directionFrom ?? 'direction';

/** The series id: the backend, renamed where the config says so.
 *
 *  A runner id names a thing the producer runs and a series id names a line on
 *  the chart; conflating them is how causl ended up with one engine drawn as two
 *  lines. Here they agree for every backend, so the map is empty and exists so
 *  that the day they stop agreeing there is somewhere to say it.
 */
function seriesId(cell) {
  const raw = String(cell[SERIES_FROM] ?? '');
  return RUNNER_TO_SERIES[raw] ?? raw;
}

/** The section a cell belongs to.
 *
 *  `key` and `source` joined, because the tile count is NOT unique across cells:
 *  21851 is both `8192x8192@64+gradient` and `8192x8192@64+noise` in one run, and
 *  16369 is two `4096x6256@46` cells. A section keyed on tile count alone draws
 *  gradient and noise as one line and nothing on the page says so.
 */
function scenarioOf(cell) {
  return SCENARIO_FROM.map((field) => cell[field]).filter((v) => v !== undefined && v !== null)
    .join(SCENARIO_JOIN);
}

/** The replicate spread for a cell's metric, as a percentage, or null.
 *
 *  Keyed `<backend>.<key>` in the document, so it is a property of the backend
 *  and the metric rather than of the series: the control cell is measured on the
 *  gradient source only. The floor is therefore applied to the noise rows too
 *  and `replicateSpreadCell` says which cell it came from, so the page can name
 *  it rather than imply it was measured everywhere.
 */
function replicateSpread(cell) {
  const spread = at(doc, producer.replicateSpreadFrom ?? 'replicate.spreadPct') ?? {};
  const value = spread[`${cell.backend}.${cell.key}`];
  return Number.isFinite(value) ? value : null;
}

/** The fitted tolerance for a cell, and why there is none when there is none.
 *
 *  There is no constant here and there is not going to be one. A tolerance comes
 *  from a baseline fitted over repeat sweeps of one commit on one host and one
 *  filesystem, or it does not exist and the cell is published as measured and not
 *  gated. Causl's ten percent is a guess, and its own file documents three ways a
 *  guessed threshold went wrong.
 */
let baseline = null;
let baselineRejection = null;
if (baselinePath) {
  if (!existsSync(baselinePath)) {
    refuse(`no baseline at ${baselinePath}`);
  } else {
    const candidate = JSON.parse(readFileSync(baselinePath, 'utf8'));
    const thisHost = host8(doc);
    const thisFs = prov.filesystem?.fsType ?? null;
    if (candidate.host8 !== thisHost) {
      baselineRejection = `the baseline was fitted on host ${candidate.host8} and this run is on ${thisHost}`;
    } else if (candidate.fsType !== thisFs) {
      baselineRejection = `the baseline was fitted on ${candidate.fsType} and this run is on ${thisFs}`;
    } else {
      baseline = candidate;
    }
  }
}

const GATED_FROM = producer.gatedFrom ?? null;
const GATED_WHEN_UNCALIBRATED = producer.gatedWhenUncalibrated ?? null;

function gating(cell, library) {
  const metric = cell.metric;
  // A field on the cell wins if the producer ever grows one. It does not have
  // one today, which is why `gatedFrom` is null and the rules below decide.
  if (GATED_FROM !== null && at(cell, GATED_FROM) !== undefined) {
    const declaredGate = at(cell, GATED_FROM);
    return { gated: declaredGate, tolerancePct: null, ungateableReason: null };
  }
  if (UNGATEABLE.includes(metric)) {
    return {
      gated: GATED_WHEN_UNCALIBRATED,
      tolerancePct: null,
      ungateableReason: `\`${metric}\` is ungateable on this suite: the free replicate pair ` +
        'already moves it further than any regression it would be asked to see',
    };
  }
  if (cell.key.endsWith(DECLARED_SUFFIX)) {
    return {
      gated: GATED_WHEN_UNCALIBRATED,
      tolerancePct: null,
      ungateableReason: 'declared, not measured, so there is nothing to grade',
    };
  }
  if (!baseline) {
    return {
      gated: GATED_WHEN_UNCALIBRATED,
      tolerancePct: null,
      ungateableReason: baselineRejection
        ? `no usable baseline: ${baselineRejection}`
        : 'no calibration has been fitted for this host and filesystem yet, so this cell is ' +
          'published as measured and not gated',
    };
  }
  const fitted = baseline.cells?.[`${library}/${cell.key}@${cell[SCALE_FROM]}`];
  if (!fitted || fitted.gateable === false || !Number.isFinite(fitted.tolerancePct)) {
    return {
      gated: GATED_WHEN_UNCALIBRATED,
      tolerancePct: null,
      ungateableReason: fitted
        ? 'the calibration named this cell ungateable: its envelope exceeds the regression it ' +
          'would be asked to see'
        : 'the calibration fitted no tolerance for this cell',
    };
  }
  return { gated: true, tolerancePct: fitted.tolerancePct, ungateableReason: null };
}

function sampleOf(cell) {
  const library = seriesId(cell);
  const unit = cell[UNIT_FROM] ?? null;
  const toMs = TO_MS[unit];
  const isThroughput = THROUGHPUT_UNITS.has(unit);
  const inv = cell.invariants ?? {};
  return {
    family: family?.id ?? null,
    library,
    backend: cell.backend,
    // `key` and `source` travel as fields as well as inside the scenario,
    // because the dashboard's chart point is fixed-shape and a verdict rule that
    // reads a field nobody carried reads undefined, which looks exactly like a
    // rule that is switched off.
    key: cell.key,
    source: cell.source,
    scenario: scenarioOf(cell),
    scale: cell[SCALE_FROM] ?? null,
    cell: cell.cell,
    // The document's own value under its own name, so nothing downstream has to
    // guess what a number means, plus the two legacy fields the page charts.
    // `medianMs` is null on a rate rather than filled with a different quantity
    // under a name that says milliseconds.
    median: cell.median ?? null,
    unit,
    direction: cell[DIRECTION_FROM] ?? null,
    medianMs: Number.isFinite(cell.median) && toMs !== undefined ? cell.median * toMs : null,
    p95Ms:
      Number.isFinite(cell.p95OfSamples) && toMs !== undefined ? cell.p95OfSamples * toMs : null,
    p95OfSamples: cell.p95OfSamples ?? null,
    throughput: isThroughput ? (cell.median ?? null) : null,
    // The across-rep dispersion the page draws bands from.
    reps: cell.reps ?? null,
    minReps: cell.minReps ?? null,
    min: cell.min ?? null,
    max: cell.max ?? null,
    iqr: cell.iqr ?? null,
    cov: cell.cov ?? null,
    ci95: cell.ci95 ?? null,
    ciHalfWidthPct: cell.ciHalfWidthPct ?? null,
    tail: cell.tail ?? null,
    // Confidence travels with its reasons or the page can grey a cell out
    // without being able to say why.
    confidence: cell.confidence ?? null,
    lowConfidenceReasons: cell.lowConfidenceReasons ?? [],
    timerSaturated: cell.timerSaturated ?? null,
    steadyState: cell.steadyState ?? null,
    machineLoad: cell.machineLoad ?? null,
    isolation: cell.isolation ?? null,
    // The verdict inputs.
    ...gating(cell, library),
    replicateSpreadPct: replicateSpread(cell),
    replicateSpreadCell: replicateSpread(cell) === null ? null : (doc.replicate?.cell ?? null),
    declared: typeof cell.key === 'string' && cell.key.endsWith(DECLARED_SUFFIX),
    deterministic: false,
    attested: attestationOf(cell).value ?? null,
    // Honest to the field names for once: RSS on generate, the counting
    // allocator's peak on read scenarios, null where neither was measured.
    peakRssMb: inv.peakRssMb ?? null,
    peakHeapMb: Number.isFinite(inv.heapPeakBytes) ? inv.heapPeakBytes / 1048576 : null,
  };
}

const samples = [];
const replicates = [];
const skipped = [];
const seen = new Set();

for (const cell of cells) {
  const library = seriesId(cell);
  if (!MEASURED.has(cell.outcome)) {
    skipped.push({
      family: family?.id ?? null,
      library,
      backend: cell.backend,
      key: cell.key,
      source: cell.source,
      scenario: scenarioOf(cell),
      scale: cell[SCALE_FROM] ?? null,
      cell: cell.cell,
      status: 'SKIP',
      // `structural` and not just `not ok`: a scenario that cannot apply to a
      // backend is a different fact from a cell that refused itself because an
      // invariant moved between reps, and the page says which.
      kind: STRUCTURAL.has(cell.outcome) ? 'structural' : 'skipped',
      outcome: cell.outcome ?? null,
      reason: cell.reason ?? null,
      // NOT `attested`. On a measured row a false attestation means the number
      // came from something other than what it is labelled with, which is a
      // withdrawal. A cell that produced no number has no timed work to attest,
      // and giving the two the same field value would let a filter count one as
      // the other.
      attested: attestationOf(cell).value ?? null,
    });
    continue;
  }
  const key = `${library}/${scenarioOf(cell)}@${cell[SCALE_FROM]}`;
  // The replicate control measures one cell first and last, so that cell is in
  // the document twice. Publishing both would draw one run as two points and
  // make the control itself look like a regression; the second pass is kept
  // beside the samples, and the spread it was taken for is already in
  // `replicate.spreadPct`.
  if (seen.has(key)) {
    replicates.push(sampleOf(cell));
  } else {
    seen.add(key);
    samples.push(sampleOf(cell));
  }
}

// --- the answer ---------------------------------------------------------------

const libraries = {};
for (const sample of samples) {
  libraries[sample.library] ??= {
    package: prov.library?.name ?? null,
    version: prov.library?.version ?? null,
    commit: libraryCommit,
    backend: sample.backend,
    source: sample.source,
    attested: true,
  };
  libraries[sample.library].attested &&= sample.attested === true;
}

const entry = {
  runId: derivedRunId,
  // The label the x axis carries: the libviprs version and the short commit, so
  // a reader can tell two runs of one version apart without reading the run id.
  version: `${prov.library?.version ?? 'unknown'}+${(libraryCommit ?? '').slice(0, 7)}`,
  capturedAt: doc.finishedAt ?? doc.startedAt ?? null,
  startedAt: doc.startedAt ?? null,
  finishedAt: doc.finishedAt ?? null,
  source: 'libviprs-bench',
  family: family.id,
  familyName: family.name,
  runner: doc.runner,
  profile,
  schemaVersion: doc.schemaVersion,
  commit: libraryCommit,
  harnessCommit,
  integrity: statedIntegrity,
  archiveFile: archivedRow?.file ?? null,
  // The era axes. A run on another machine or another filesystem starts a new
  // era instead of drawing one line across two experiments, so the page needs
  // these as fields rather than as prose it has to parse.
  host: {
    os: prov.os ?? null,
    arch: prov.arch ?? null,
    cpuModel: prov.cpuModel ?? null,
    ncpu: prov.ncpu ?? null,
    inContainer: prov.inContainer ?? null,
    rustc: node.rustc ?? null,
    cargo: node.cargo ?? null,
    buildProfile: node.buildProfile ?? null,
    // The page's era axis reads this one; the archive files the run under the
    // other. They are different lists over the same provenance and the entry
    // carries both rather than letting one stand in for the other.
    fingerprint: fingerprint(doc, HOST_FINGERPRINT_FROM),
    archiveBucket: host8(doc),
    fsType: at(doc, producer.fsTypeFrom ?? 'provenance.filesystem.fsType') ?? null,
  },
  filesystem: prov.filesystem ?? null,
  emulated: prov.emulated,
  emulationEvidence: prov.emulationEvidence ?? [],
  loadAverage: prov.loadAverage ?? null,
  lockfileHash: prov.lockfileHash ?? null,
  measurement: doc.measurement ?? null,
  libraries,
  // The noise floor the page draws bands from and calls a delta `noise` against.
  replicate: doc.replicate ?? null,
  invariants,
  modelled,
  samples,
  replicates,
  skipped,
};

const before = history.length;
const kept = history.filter((h) => h.runId !== entry.runId);
kept.push(entry);
kept.sort((a, b) => String(a.capturedAt).localeCompare(String(b.capturedAt)));

const lowConfidence = samples.filter((s) => s.confidence === 'low').length;
const gated = samples.filter((s) => s.gated).length;

console.log(`run       ${entry.runId}`);
console.log(`family    ${entry.family} (${entry.runner}), profile ${entry.profile}`);
console.log(`captured  ${entry.capturedAt}`);
console.log(
  `host      ${entry.host.cpuModel} · ${entry.host.os}/${entry.host.arch} · ${entry.host.rustc}`,
);
console.log(
  `native    emulated=${JSON.stringify(entry.emulated)} on ${entry.emulationEvidence.length} source(s)`,
);
console.log(`series    ${Object.keys(libraries).join(' ')}`);
console.log(
  `samples   ${samples.length} measured · ${replicates.length} replicate pass · ` +
    `${skipped.length} not-ok`,
);
console.log(
  `confidence ${lowConfidence} of ${samples.length} low, ` +
    `${gated} gated against a fitted tolerance`,
);
console.log(`invariants ${invariants.length} rows · modelled ${modelled.length} rows`);
console.log(`history   ${before} → ${kept.length} entries`);

if (lowConfidence > 0 && gated === 0) {
  console.error(
    `\nNOTE: nothing in this run is gated. ${lowConfidence} of ${samples.length} cells are ` +
      'low-confidence and no calibration has been fitted for this host, so the page publishes ' +
      'every cell as measured and not gated, with no verdict chips.\n',
  );
}

if (has('dry-run')) {
  console.log('\n--dry-run: nothing written');
  process.exit(EXIT.OK);
}

// One entry per line. The file grows by one run at a time and a pretty-printed
// array turns that into a fifty thousand line diff, while a single compact array
// turns it into an unreviewable "1 line changed".
let line;
try {
  line = kept.map((e) => JSON.stringify(e));
} catch (e) {
  console.error(`refused: the entry cannot be serialised (${e.message})`);
  process.exit(EXIT.REFUSED);
}
// Canonicalising here is not about the digest: it is the same range and
// finiteness check the producer passes, applied to what is about to be written,
// so a NaN cannot reach the page as `null`.
try {
  canonicalJson(entry);
} catch (e) {
  console.error(`refused: the entry carries a value the page cannot render: ${e.message}`);
  process.exit(EXIT.REFUSED);
}
writeFileSync(historyPath, `[\n${line.join(',\n')}\n]\n`);
console.log(`\nwrote ${historyPath}`);
