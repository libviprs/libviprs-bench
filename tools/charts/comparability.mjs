#!/usr/bin/env node
/**
 * comparability.mjs — which cells may be charted across technologies, and why.
 *
 * The method is causl-bench's `runners/_contract/EQUIVALENCE.md`, applied to
 * this repo. A benchmark that compares two runners which did not do the same
 * work is not a benchmark, and this is the gate that refuses those cells.
 *
 * Four rules, each named so a failure says which one it broke:
 *
 *   every-scenario-is-declared        there is no default status
 *   declared-work-is-within-tolerance comparable is refused past the bound
 *   comparable-cells-prove-equivalence a comparable cell needs its evidence
 *   held-out-cells-keep-their-census  held out is not exempt from checking
 *
 * Where this repo is better off than the method it borrows: causl declares an
 * expected recompute count and checks against it, while every run here reports
 * `tiles_produced`, `levels_processed` and either a PSNR or a digest, so work
 * equivalence is measured first and the declaration only says what to make of
 * the measurement.
 *
 * Usage:
 *   node tools/charts/comparability.mjs <results.json>...   # exits non-zero on a violation
 */

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
export const CONTRACT_PATH = join(HERE, 'comparability.json');

const STATUSES = new Set(['comparable', 'non-comparable', 'unknown']);
const flat = (v) => (Array.isArray(v) ? v.join(' ') : v);

/**
 * Validate a contract. Everything that can be wrong here is wrong loudly,
 * because a contract that parses but means nothing is worse than none.
 */
export function loadContract(raw) {
  const contract = typeof raw === 'string' ? JSON.parse(readFileSync(raw, 'utf8')) : raw;
  const tolerance = contract.comparabilityTolerance;
  if (!Number.isFinite(tolerance) || tolerance < 1) {
    throw new Error('comparabilityTolerance must be a number no less than 1');
  }
  const scenarios = contract.scenarios ?? {};
  for (const [name, decl] of Object.entries(scenarios)) {
    if (!decl || typeof decl !== 'object') throw new Error(`${name}: declaration must be an object`);
    if (!STATUSES.has(decl.status)) {
      throw new Error(`${name}: status must be one of ${[...STATUSES].join(', ')}, got ${decl.status ?? 'nothing'}. `
        + 'There is no default: silence is how unchecked cells stay unchecked.');
    }
    if (decl.status !== 'comparable' && !flat(decl.reason)) {
      throw new Error(`${name}: ${decl.status} needs a reason. `
        + 'unknown is not a synonym for non-comparable: one says we know these differ and here is how, '
        + 'the other says nobody has established what this cell counts.');
    }
    for (const [cellKey, override] of Object.entries(decl.cells ?? {})) {
      if (!STATUSES.has(override?.status)) {
        throw new Error(`${name}.cells.${cellKey}: status must be one of ${[...STATUSES].join(', ')}`);
      }
      if (override.status !== 'comparable' && !flat(override.reason)) {
        throw new Error(`${name}.cells.${cellKey}: ${override.status} needs a reason`);
      }
    }
    if (decl.status === 'comparable') {
      if (!Array.isArray(decl.work) || decl.work.length === 0) {
        throw new Error(`${name}: comparable needs a work list to check against`);
      }
      const eq = decl.equivalence;
      if (!eq || (eq.kind !== 'psnr' && eq.kind !== 'not-available')) {
        throw new Error(`${name}: comparable needs an equivalence declaration of psnr or not-available`);
      }
      if (eq.kind === 'not-available' && !flat(eq.reason)) {
        throw new Error(`${name}: equivalence not-available needs a reason naming why no evidence is possible`);
      }
    }
  }
  return { ...contract, comparabilityTolerance: tolerance, scenarios };
}

/**
 * A cell's config key. The engines families key on image size and concurrency;
 * the storage family keys on its cell id, which is one pyramid shape at one
 * source. The caller says which, because guessing from the fields present is
 * how a family with neither would get an `undefinedxundefined` group.
 */
const defaultConfigOf = (run) =>
  `${run.width}x${run.height}${run.concurrency === undefined || run.concurrency === null ? '' : `_c${run.concurrency}`}`;

const technologyOf = (run) => run.engine ?? run.backend;

/**
 * Assess a set of runs against the contract.
 *
 * @param {readonly object[]} runs
 * @param {object} contract raw or already loaded
 * @param {{ scenario?: string }} [opts] a scenario name for runs that do not carry one
 */
export function assessComparison(runs, contract, opts = {}) {
  const loaded = contract.scenarios && contract.comparabilityTolerance ? contract : loadContract(contract);
  const configOf = opts.configOf ?? defaultConfigOf;
  const tolerance = loaded.comparabilityTolerance;
  const reference = loaded.equivalenceDefaults?.referenceTechnology ?? null;

  /** @type {Map<string, {scenario: string, config: string, runs: object[]}>} */
  const grouped = new Map();
  const violations = [];

  for (const run of runs) {
    const scenario = run.scenario ?? opts.scenario;
    if (!scenario) {
      violations.push({
        rule: 'every-scenario-is-declared',
        message: `a run for ${technologyOf(run)} carries no scenario and none was supplied`,
      });
      continue;
    }
    const key = `${scenario}\u0000${configOf(run)}`;
    if (!grouped.has(key)) grouped.set(key, { scenario, config: configOf(run), runs: [] });
    grouped.get(key).runs.push(run);
  }

  const cells = [];
  for (const { scenario, config, runs: members } of grouped.values()) {
    const scenarioDecl = loaded.scenarios[scenario];
    // A scenario can be comparable in general and still have a cell nobody has
    // established. That is an honest `unknown`: a to-do with a name on it,
    // rather than a status invented to keep a chart.
    const override = scenarioDecl?.cells?.[config];
    const decl = override ? { ...scenarioDecl, ...override } : scenarioDecl;
    if (!decl) {
      violations.push({
        rule: 'every-scenario-is-declared',
        scenario,
        config,
        message: `${scenario} is not declared in the contract, so it is not charted. `
          + 'Add it with a status and, if it is not comparable, a reason.',
      });
      cells.push({ scenario, config, technologies: members.map(technologyOf), status: null, chartable: false, work: null, equivalence: null, reasons: ['undeclared'] });
      continue;
    }

    const work = assessWork(members, decl.work ?? [], tolerance);
    const equivalence = assessEquivalence(members, decl, loaded, reference);
    const reasons = [];
    let chartable = decl.status === 'comparable';

    if (decl.status === 'comparable') {
      if (work.ratio !== null && work.ratio > tolerance) {
        chartable = false;
        reasons.push('work-asymmetry');
        violations.push({
          rule: 'declared-work-is-within-tolerance',
          scenario,
          config,
          message: `[${scenario}@${config}] declared work differs by ${fmtRatio(work.ratio)} `
            + `(${work.metric}: ${work.spread}), over a tolerance of ${tolerance}x`,
        });
      }
      if (!equivalence.ok) {
        chartable = false;
        reasons.push('equivalence-unproven');
        violations.push({
          rule: 'comparable-cells-prove-equivalence',
          scenario,
          config,
          message: `[${scenario}@${config}] ${equivalence.why}`,
        });
      }
    } else {
      reasons.push(decl.status);
      // Held out of the chart is not excused from the census: the count is the
      // evidence for the reason it is held out, and a reason whose count has
      // drifted is describing a benchmark that no longer exists.
      if (work.ratio !== null && work.ratio > tolerance) {
        violations.push({
          rule: 'held-out-cells-keep-their-census',
          scenario,
          config,
          message: `[${scenario}@${config}] is ${decl.status} and its work has drifted to `
            + `${fmtRatio(work.ratio)} (${work.metric}: ${work.spread}). `
            + 'Re-read the recorded reason against the numbers.',
        });
      }
    }

    cells.push({
      scenario,
      config,
      technologies: members.map(technologyOf),
      status: decl.status,
      chartable,
      work,
      equivalence,
      reasons,
    });
  }

  return { cells, violations };
}

function fmtRatio(r) {
  return `${Number.isInteger(r) ? r : r.toFixed(2)}x`;
}

function assessWork(members, metrics, tolerance) {
  let worst = null;
  let worstMetric = null;
  let spread = '';
  for (const metric of metrics) {
    const counts = members
      .map((m) => [technologyOf(m), m[metric]])
      .filter(([, v]) => Number.isFinite(v) && v > 0);
    if (counts.length < 2) continue;
    const values = counts.map(([, v]) => v);
    const ratio = Math.max(...values) / Math.min(...values);
    if (worst === null || ratio > worst) {
      worst = ratio;
      worstMetric = metric;
      spread = counts.map(([t, v]) => `${t}=${v}`).join(', ');
    }
  }
  return { ratio: worst, metric: worstMetric, spread, tolerance };
}

function assessEquivalence(members, decl, contract, reference) {
  const eq = decl.equivalence ?? {};
  if (decl.status !== 'comparable') return { ok: true, kind: eq.kind ?? 'not-required', why: '' };
  if (eq.kind === 'not-available') {
    return { ok: true, kind: 'not-available', why: flat(eq.reason) };
  }
  const floor = eq.floorDb ?? contract.equivalenceDefaults?.psnrFloorDb;
  const judged = members.filter((m) => technologyOf(m) !== reference);
  const missing = judged.filter((m) => !Number.isFinite(m.equivalence_psnr_db));
  if (judged.length === 0) {
    return { ok: false, kind: 'psnr', why: 'no technology other than the reference ran, so there is nothing to compare' };
  }
  if (missing.length > 0) {
    return {
      ok: false,
      kind: 'psnr',
      why: `${missing.map(technologyOf).join(', ')} carry no equivalence_psnr_db, so this cell is charted `
        + 'as a like-for-like race with the output-equivalence check never having run',
    };
  }
  const below = judged.filter((m) => m.equivalence_psnr_db < floor);
  if (below.length > 0) {
    return {
      ok: false,
      kind: 'psnr',
      why: `${below.map((m) => `${technologyOf(m)}=${m.equivalence_psnr_db}dB`).join(', ')} under the ${floor}dB floor`,
    };
  }
  return { ok: true, kind: 'psnr', why: '' };
}

/** The runs a cross-technology chart may draw. */
export function chartableRuns(runs, assessment, opts = {}) {
  const configOf = opts.configOf ?? defaultConfigOf;
  const ok = new Set(assessment.cells.filter((c) => c.chartable).map((c) => `${c.scenario}\u0000${c.config}`));
  return runs.filter((r) => ok.has(`${r.scenario ?? opts.scenario}\u0000${configOf(r)}`));
}

/* CLI */
if (process.argv[1] && import.meta.url === `file://${process.argv[1]}`) {
  const files = process.argv.slice(2).filter((a) => !a.startsWith('-'));
  const scenario = (process.argv.find((a) => a.startsWith('--scenario=')) ?? '').split('=')[1];
  const contract = loadContract(CONTRACT_PATH);
  let failed = 0;
  for (const file of files) {
    const doc = JSON.parse(readFileSync(file, 'utf8'));
    const runs = Array.isArray(doc) ? doc : (doc.cells ?? []);
    const { cells, violations } = assessComparison(runs, contract, { scenario });
    const charted = cells.filter((c) => c.chartable).length;
    console.log(`${file}: ${cells.length} cells, ${charted} chartable across technologies`);
    for (const c of cells.filter((x) => !x.chartable)) {
      console.log(`  held out  ${c.scenario}@${c.config}  ${c.status ?? 'undeclared'}  ${c.reasons.join(', ')}`);
    }
    for (const v of violations) {
      console.error(`  FAIL ${v.rule} ${v.message}`);
      failed++;
    }
  }
  process.exit(failed > 0 ? 1 : 0);
}
