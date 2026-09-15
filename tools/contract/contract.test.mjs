// The contract itself: the frozen copy, the anchors, the config, the era axes.
//
// Each test names the wrong implementation it goes red against.

import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdtempSync, cpSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parameterize, balancedEnd, CONFIG_PATHS } from './parameterize.mjs';
import { UPSTREAM_DEFAULTS, resolve, pluck, validate } from './lib/config.mjs';
import { blobSha1 } from './lib/blob-sha1.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const read = (p) => readFileSync(join(here, p), 'utf8');
const json = (p) => JSON.parse(read(p));

const FROZEN = read('upstream/dashboard.js');
const CONFIG = json('config.json');
const SCHEMA = json('config.schema.json');
const VOCAB = json('fixtures/producer-vocabulary.json');
const REV = read('UPSTREAM_REV').trim();

const sh = (args, opts = {}) =>
  execFileSync(args[0], args.slice(1), { cwd: here, encoding: 'utf8', ...opts });

// ---------------------------------------------------------------------------
// The frozen copy
// ---------------------------------------------------------------------------

// Goes red against: a manifest regenerated from whatever is on disk. The gate
// is only a check against upstream if the numbers in the manifest are the ones
// `git ls-tree` reported at the pin, so they are re-asserted here as literals.
test('the manifest carries the blob sha1s causl-org cd65c76 actually holds', () => {
  assert.equal(REV, 'cd65c76');
  const pinned = {
    'comparable-era.test.mjs': '72cdaf568cf881ae18a0c9f851661eab9d1c2248',
    'dashboard.css': 'b5a45369e15fef5f942a55d19d8281872f6fdd23',
    'dashboard.js': '2f3203769e3c49f64fc0747d8ef114ea31cee412',
    'history.json': 'e1b82917a2d788dcf1975e1cf88ab08a80b6ab95',
    'history.sample.json': '46bca7033b29d05c4c1cba51f787102ea8784284',
    'import-run.mjs': 'a3c7e9e1bed1e2243385bbd4689a03da62cc971b',
    'import-run.test.mjs': '40337c48db9bf3f302011187de1e9631852188d1',
    'regen-history-sample.mjs': '424f486bd99d09bdd9f547ed9728d8ba99f03bb6',
    'render-latest.mjs': '4bb85e06d22e9273da19451d204f79ad04238f56',
  };
  const manifest = read('UPSTREAM.manifest');
  for (const [name, want] of Object.entries(pinned)) {
    assert.ok(manifest.includes(`${want}  ${name}`), `the manifest does not pin ${name} at ${want}`);
    assert.equal(
      blobSha1(readFileSync(join(here, 'upstream', name))),
      want,
      `upstream/${name} is not what causl-org ${REV} holds`,
    );
  }
});

// Goes red against: a gate that reports green when it cannot actually check.
test('sync-contract.sh --check is green at the pin', () => {
  const out = sh(['bash', './sync-contract.sh', '--check']);
  assert.match(out, /in sync with causl-org pages\/benchmarks @ cd65c76/);
  assert.match(out, /manifest \(blob sha1 recorded at the pin\)/);
});

// Goes red against: a gate that only compares a file to itself. Every frozen
// file gets an edit, one at a time, and the gate has to notice each one.
test('sync-contract.sh --check is red against an edit to any frozen file', () => {
  const files = read('UPSTREAM.manifest')
    .split('---\n')[1]
    .trim()
    .split('\n')
    .map((l) => l.split('  ')[1]);
  assert.equal(files.length, 9);

  for (const name of files) {
    const scratch = mkdtempSync(join(tmpdir(), 'k23-sync-'));
    try {
      cpSync(here, scratch, { recursive: true });
      const target = join(scratch, 'upstream', name);
      // One appended byte. Nothing about the file's meaning changes, which is
      // the point: the gate is about bytes, not about meaning.
      writeFileSync(target, `${readFileSync(target, 'utf8')} `);
      let code = 0;
      try {
        execFileSync('bash', ['./sync-contract.sh', '--check'], { cwd: scratch, stdio: 'pipe' });
      } catch (e) {
        code = e.status;
      }
      assert.equal(code, 1, `sync-contract.sh did not go red on an edit to upstream/${name}`);
    } finally {
      rmSync(scratch, { recursive: true, force: true });
    }
  }
});

// Goes red against: a gate that ignores a file dropped into the frozen
// directory, which is the shape a libviprs-specific addition would take.
test('sync-contract.sh --check is red against an unpinned file in upstream/', () => {
  const scratch = mkdtempSync(join(tmpdir(), 'k23-sync-'));
  try {
    cpSync(here, scratch, { recursive: true });
    writeFileSync(join(scratch, 'upstream', 'libviprs-tweaks.js'), '// nope\n');
    let code = 0;
    let out = '';
    try {
      out = execFileSync('bash', ['./sync-contract.sh', '--check'], { cwd: scratch, encoding: 'utf8' });
    } catch (e) {
      code = e.status;
      out = e.stdout ?? '';
    }
    assert.equal(code, 1);
    assert.match(out, /UNPINNED/);
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
});

// ---------------------------------------------------------------------------
// The anchors
// ---------------------------------------------------------------------------

// Goes red against: a parameteriser that silently patches the wrong place when
// upstream moves a line. Every anchor is required to match exactly once, and a
// frozen file with the anchor removed must refuse rather than produce output.
test('an anchor that moved is a refusal, not a patch landing somewhere else', () => {
  assert.doesNotThrow(() => parameterize(FROZEN));
  const broken = FROZEN.replace('  const NOISY_PROXY = 0.50', '  const NOISY_PROXY_RENAMED = 0.50');
  assert.throws(() => parameterize(broken), /NOISY_PROXY.*matches 0 times|matches 0 times/s);
  // And a duplicated anchor is equally a refusal: patching the first of two is
  // a coin flip.
  const doubled = FROZEN.replace(
    '  const PASS_PCT = 0.05',
    '  const PASS_PCT = 0.05\n  const PASS_PCT = 0.05',
  );
  assert.throws(() => parameterize(doubled), /matches 2 times/);
});

// Goes red against: a generated file hand-edited, or left behind after a config
// or parameteriser change.
test('generated/dashboard.js is what the frozen file and the parameteriser produce', () => {
  const out = sh(['node', './parameterize.mjs', '--check']);
  assert.match(out, /is current \(18 edits\)/);
});

// Goes red against: a fallback table that drifted from the frozen file. Every
// upstream literal is re-read out of dashboard.js here, so the table cannot
// quietly stop being upstream's.
test('UPSTREAM_DEFAULTS still matches the literals in the frozen dashboard', () => {
  const lift = (decl, path) => {
    const at = FROZEN.indexOf(decl);
    assert.notEqual(at, -1, `${decl} is gone from the frozen dashboard`);
    const start = at + decl.length;
    // The same balanced-literal scan the parameteriser anchors on, so this test
    // reads exactly the span the parameteriser wraps and cannot agree with it
    // by accident. The literal is JS with comments in it, so `new Function` is
    // the honest reader; nothing evaluated here comes from outside the frozen
    // file.
    const literal = FROZEN.slice(start, balancedEnd(FROZEN, start));
    // eslint-disable-next-line no-new-func
    const value = new Function(`return (${literal})`)();
    assert.deepEqual(value, UPSTREAM_DEFAULTS[path], `${path} drifted from ${decl.trim()}`);
  };
  lift('  const LIBRARY_ORDER = ', 'series.order');
  lift('  const LIBRARY_LABEL = ', 'series.label');
  lift('  const LIBRARY_COLOR = ', 'series.color');
  lift('  const LIBRARY_DASH = ', 'series.dash');
  lift('  const VERDICT_COLOR = ', 'verdict.color');
  lift('  const DEFAULT_LIBRARIES = new Set(', 'defaults.libraries');
  lift('  const DEFAULT_SCALES = new Set(', 'defaults.scales');
  for (const [decl, path] of [
    ['  const REGRESSION_PCT = ', 'verdict.regressionPct'],
    ['  const IMPROVED_PCT = ', 'verdict.improvedPct'],
    ['  const PASS_PCT = ', 'verdict.passPct'],
    ['  const NOISY_PROXY = ', 'verdict.noisyProxy'],
  ]) {
    const at = FROZEN.indexOf(decl) + decl.length;
    const num = Number.parseFloat(FROZEN.slice(at, FROZEN.indexOf('\n', at)));
    assert.equal(num, UPSTREAM_DEFAULTS[path], `${path} drifted from ${decl.trim()}`);
  }
});

// ---------------------------------------------------------------------------
// The config
// ---------------------------------------------------------------------------

/** A key walk against the schema. Not a full validator: it catches the failure
 *  that actually happens, which is a config key nothing reads because it was
 *  never declared. */
function undeclaredKeys(value, node, path = '') {
  const bad = [];
  if (!node || typeof node !== 'object') return bad;
  if (node.type === 'object' && node.properties) {
    for (const [k, v] of Object.entries(value ?? {})) {
      if (k === '$schema') continue;
      if (!(k in node.properties)) {
        if (node.additionalProperties === false) bad.push(`${path}${k}`);
        continue;
      }
      bad.push(...undeclaredKeys(v, node.properties[k], `${path}${k}.`));
    }
  }
  return bad;
}

test('every key in config.json is declared in config.schema.json', () => {
  assert.deepEqual(undeclaredKeys(CONFIG, SCHEMA), []);
  for (const req of SCHEMA.required) assert.ok(req in CONFIG, `config.json is missing ${req}`);
});

// Goes red against: a schema that declares a path nothing reads, or a
// parameteriser that reads a path the schema never declared. Either way a
// consumer builds against a shape that does not exist.
test('every path the generated dashboard reads is declared in the schema', () => {
  for (const p of CONFIG_PATHS) {
    let node = SCHEMA;
    for (const seg of p.split('.')) {
      assert.ok(node.properties && seg in node.properties, `${p} is not declared in the schema`);
      node = node.properties[seg];
    }
  }
});

test('config.json passes the structural validator', () => {
  assert.deepEqual(validate(CONFIG, { upstreamRev: REV }), []);
});

// Goes red against: a config written against one revision and a frozen copy
// pinned at another.
test('a config pinned at a different revision is refused', () => {
  const wrong = JSON.parse(JSON.stringify(CONFIG));
  wrong.contract.upstreamRev = 'deadbee';
  const problems = validate(wrong, { upstreamRev: REV });
  assert.equal(problems.length, 1);
  assert.match(problems[0], /not automatically valid against another/);
});

// ---------------------------------------------------------------------------
// The config against a real document
// ---------------------------------------------------------------------------

// Goes red against: a config naming a series, scenario, metric, outcome or
// invariant the producer does not emit. The vocabulary is extracted from the
// archived run 20260914T145707Z; nothing here is invented.
test('config.json names only things the producer actually emits', () => {
  assert.equal(
    VOCAB.derivedFrom.documentDigest,
    'sha256:6a712af369d3229bb359a13bc3b6ece1bd3bff02257f540f85c0e0f1b0cf49d3',
    'the vocabulary is no longer the archived run the config cites',
  );
  assert.equal(CONFIG.contract.provenance, `${VOCAB.derivedFrom.file.replace('.json', '')}`);

  assert.deepEqual([...CONFIG.series.order].sort(), VOCAB.backends);
  for (const id of CONFIG.defaults.libraries) assert.ok(VOCAB.backends.includes(id));

  assert.ok(VOCAB.cellNames.includes(CONFIG.defaults.scales[0]), 'defaults.scales names no real cell');

  assert.ok(VOCAB.cellFields.includes(CONFIG.producer.seriesFrom));
  for (const f of CONFIG.producer.rowIdentityFrom) {
    // A row identity naming a field no cell carries is the worst of the shapes
    // this file guards: it does not refuse and it does not throw, it makes every
    // row look like every other row, and the run gets a large confident
    // replicate count it never earned.
    assert.ok(
      VOCAB.cellFields.includes(f),
      `producer.rowIdentityFrom names ${f}, which no cell has`,
    );
  }
  for (const f of [CONFIG.sections.scaleFrom, CONFIG.sections.unitFrom, CONFIG.sections.directionFrom]) {
    assert.ok(VOCAB.cellFields.includes(f), `sections names cell field ${f}, which does not exist`);
  }
  for (const f of CONFIG.sections.scenarioFrom) {
    assert.ok(VOCAB.cellFields.includes(f), `sections.scenarioFrom names ${f}, which does not exist`);
  }
  for (const f of CONFIG.samples.carry) {
    assert.ok(VOCAB.cellFields.includes(f), `samples.carry names ${f}, which no cell has`);
  }

  const declared = [
    ...CONFIG.producer.outcomes.measured,
    ...CONFIG.producer.outcomes.structural,
    ...CONFIG.producer.outcomes.refusing,
  ].sort();
  assert.deepEqual(declared, VOCAB.outcomes, 'the outcome buckets do not cover exactly what the producer emits');

  assert.deepEqual([...CONFIG.producer.invariantNames].sort(), VOCAB.invariantNames);
  assert.ok(VOCAB.documentPaths.includes(CONFIG.producer.fsTypeFrom));
  assert.ok(VOCAB.documentPaths.includes(CONFIG.producer.replicateSpreadFrom));
  for (const p of CONFIG.producer.hostFingerprintFrom) {
    assert.ok(VOCAB.documentPaths.includes(p), `hostFingerprintFrom names ${p}, which the document has not got`);
  }
  assert.ok(VOCAB.families ?? true);
  assert.ok(CONFIG.producer.families.includes(VOCAB.derivedFrom.family));
});

// Goes red against: the comment claiming tile count is ambiguous. It is a fact
// about the real capture and it decides the section key, so it is asserted.
test('tile count is NOT unique across cells, which is why source is in the scenario', () => {
  const ambiguous = Object.entries(VOCAB.scaleToCells).filter(([, cells]) => cells.length > 1);
  assert.ok(
    ambiguous.length >= 2,
    'tile counts became unique, so sections.scaleFrom could be simplified and this note is stale',
  );
  assert.deepEqual(VOCAB.scaleToCells['21851'], ['8192x8192@64+gradient', '8192x8192@64+noise']);
  // With `source` in the scenario, every (scenario, scale) pair is one cell.
  const seen = new Map();
  for (const [scale, cells] of Object.entries(VOCAB.scaleToCells)) {
    for (const c of cells) {
      const src = c.split('+')[1];
      const k = `${src}|${scale}`;
      assert.ok(!seen.has(k), `${k} is still ambiguous: ${seen.get(k)} and ${c}`);
      seen.set(k, c);
    }
  }
});

// Goes red against: the structural-failure claim in the config comment. Seven
// cells in the real capture are `failed` for a reason that is a fact about the
// backend, and upstream's importer refuses a whole run over any `failed` cell.
test('the real capture carries structurally-failed cells, so `failed` cannot be a refusal', () => {
  assert.ok(VOCAB.outcomes.includes('failed'));
  assert.ok(
    VOCAB.structuralFailureReasons.some((r) => r.startsWith('failed: a directory tree has no root directory')),
    'the structural failure this bucket exists for is gone; re-check producer.outcomes',
  );
  assert.ok(CONFIG.producer.outcomes.structural.includes('failed'));
  assert.deepEqual(CONFIG.producer.outcomes.refusing, []);
});

// Goes red against: the observed-numbers block drifting from the run it cites.
test('verdict.observed matches the run it cites, number for number', () => {
  const o = CONFIG.verdict.observed;
  assert.equal(o.medianSpreadPct, VOCAB.replicate.medianSpreadPct);
  assert.equal(o.maxSpreadPct, VOCAB.replicate.maxSpreadPct);
  assert.equal(o.maxSpreadKey, VOCAB.replicate.maxSpreadKey);
  assert.equal(o.spreadMetrics, VOCAB.counts.spreadMetrics);
  assert.equal(o.cellsOk, VOCAB.counts.ok);
  assert.equal(o.cellsLowConfidence, VOCAB.counts.lowConfidence);
  assert.equal(o.cellsTimerSaturated, VOCAB.counts.timerSaturated);
  assert.equal(o.timerTickNs, VOCAB.measurement.timerTickNs);
  assert.equal(o.covLowConfidence, VOCAB.measurement.covLowConfidence);
  // The 4.1 us floor is the timer tick times the minimum ticks per sample, not
  // a number somebody typed.
  assert.equal(o.timerFloorUs, (VOCAB.measurement.timerTickNs * VOCAB.measurement.minTicksPerSample) / 1000);
});

// ---------------------------------------------------------------------------
// The era axes
// ---------------------------------------------------------------------------

/** Lift `__eraSignature` out of the generated bundle the way upstream's own
 *  test lifts `comparableEraStart`: it is a pure function and importing the
 *  IIFE would need a DOM. */
function liftEraSignature(config) {
  const src = read('generated/dashboard.js');
  const m = src.match(/function __eraSignature\(entry\)\s*\{[\s\S]*?\n {2}\}/);
  assert.ok(m, '__eraSignature is no longer findable in generated/dashboard.js');
  const cfg = src.match(/function __cfg\(path, fallback\)\s*\{[\s\S]*?\n {2}\}/);
  assert.ok(cfg, '__cfg is no longer findable');
  // eslint-disable-next-line no-new-func
  return new Function(
    '__CONFIG',
    `${cfg[0]}; ${m[0]}; return __eraSignature`,
  )(config ?? null);
}

const entry = (libs, extra = {}) => ({ samples: libs.map((library) => ({ library })), ...extra });

// Goes red against: an era rewrite that changed upstream's answer. With the
// default single axis the signature must partition the shipped history exactly
// the way `libsOf`/`sameLibs` did, and the whole shipped feed is the test data.
test('with the default axis the era signature agrees with upstream on every shipped run', () => {
  const sig = liftEraSignature(null);
  const history = json('upstream/history.json');
  const libsOf = (e) => [...new Set((e.samples ?? []).map((s) => s.library))].sort();
  for (const e of history) {
    const expected = libsOf(e);
    assert.equal(
      sig(e),
      expected.length === 0 ? null : `series=${expected.join('')}`,
      `the signature for ${e.runId ?? e.capturedAt} is not its library set`,
    );
  }
  // And the empty-entry rule upstream's own test pins.
  assert.equal(sig({ samples: [] }), null);
  assert.equal(sig(entry(['a', 'b'])), sig(entry(['b', 'a'])), 'the set is order-sensitive');
});

// Goes red against: era axes that ignore the host, which is the whole reason
// libviprs needs more than the library set. Two runs on different machines must
// not share an x-axis.
test('a run on a different host or filesystem starts a new era', () => {
  const sig = liftEraSignature(CONFIG);
  const base = entry(['pmtiles', 'directory'], { host: { fingerprint: 'linux|aarch64|m1|8', fsType: 'ext4' } });
  const sameMachine = entry(['pmtiles', 'directory'], { host: { fingerprint: 'linux|aarch64|m1|8', fsType: 'ext4' } });
  const otherMachine = entry(['pmtiles', 'directory'], { host: { fingerprint: 'linux|x86_64|xeon|32', fsType: 'ext4' } });
  const otherFs = entry(['pmtiles', 'directory'], { host: { fingerprint: 'linux|aarch64|m1|8', fsType: 'tmpfs' } });

  assert.equal(sig(base), sig(sameMachine));
  assert.notEqual(sig(base), sig(otherMachine), 'a different host did not start a new era');
  assert.notEqual(sig(base), sig(otherFs), 'a different filesystem did not start a new era');

  // The control: upstream's single axis cannot tell any of these apart, which
  // is exactly the gap the extra axes close.
  const upstreamSig = liftEraSignature(null);
  assert.equal(upstreamSig(base), upstreamSig(otherMachine));
  assert.equal(upstreamSig(base), upstreamSig(otherFs));
});

// Goes red against: era axes that ignore which estimator produced the noise
// floor. Document version 1 published `replicate.spreadPct` as the gap between
// two measurements of the control cell; version 2 publishes a dispersion over
// every placement under the same key. Both are real, neither is wrong, and one
// line drawn through the two is a trend in an estimator rather than in the code
// (libviprs-bench #84).
test('a run whose replicate floor came from a different estimator starts a new era', () => {
  const sig = liftEraSignature(CONFIG);
  const host = { fingerprint: 'linux|aarch64|m1|8', fsType: 'ext4' };
  const twoPoint = entry(['pmtiles', 'directory'], { host, schemaVersion: 1 });
  const dispersion = entry(['pmtiles', 'directory'], { host, schemaVersion: 2 });
  const alsoDispersion = entry(['pmtiles', 'directory'], { host, schemaVersion: 2 });

  assert.equal(sig(dispersion), sig(alsoDispersion));
  assert.notEqual(
    sig(twoPoint),
    sig(dispersion),
    'a two-point floor and a six-placement floor landed on one x-axis',
  );
  // And the axis is declared, so this is not an accident of the fingerprint.
  assert.ok(
    CONFIG.era.axes.some((axis) => axis.from === 'schemaVersion'),
    'nothing in the era axes reads the document schema version',
  );
  // The control: upstream's single axis cannot tell the two eras apart, which
  // is exactly the gap this axis closes.
  const upstreamSig = liftEraSignature(null);
  assert.equal(upstreamSig(twoPoint), upstreamSig(dispersion));
});

// Goes red against: a fingerprint whose inputs are not on the document. The
// era axis reads `host.fingerprint`, and the importer builds that from the
// paths in `producer.hostFingerprintFrom`.
test('the host fingerprint is buildable from the paths the config names', () => {
  const doc = { provenance: { os: 'linux', arch: 'aarch64', cpuModel: 'm1', ncpu: 8, inContainer: true, emulated: false } };
  const parts = CONFIG.producer.hostFingerprintFrom.map((p) => pluck(doc, p));
  for (let i = 0; i < parts.length; i += 1) {
    assert.equal(parts[i].length, 1, `${CONFIG.producer.hostFingerprintFrom[i]} did not resolve to one value`);
  }
  assert.equal(parts.map((v) => v[0]).join('|'), 'linux|aarch64|m1|8|true|false');
  // `emulated: false` is part of it on purpose: an emulated run and a native
  // run on the same box are not the same experiment.
  assert.ok(CONFIG.producer.hostFingerprintFrom.includes('provenance.emulated'));
});

test('resolve falls back rather than throwing on a half-written config', () => {
  assert.equal(resolve(null, 'a.b', 'fb'), 'fb');
  assert.equal(resolve({ a: null }, 'a.b', 'fb'), 'fb');
  assert.equal(resolve({ a: { b: null } }, 'a.b', 'fb'), 'fb');
  assert.equal(resolve({ a: { b: 0 } }, 'a.b', 'fb'), 0, 'a falsy value is a value');
  assert.equal(resolve({ a: { b: false } }, 'a.b', true), false);
});
