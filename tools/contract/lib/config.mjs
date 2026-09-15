// The contract config: the fallbacks, the resolver, and a validator.
//
// Everything libviprs does differently from causl's frozen dashboard, importer
// and renderer is a value at a path in `config.json`. Every one of those paths
// has causl's own literal as its fallback, which is what makes "with no config
// present the page renders exactly what causl's does" a fact rather than an
// intention: with no config, every read returns the literal that is still
// sitting in the frozen file, unchanged.
//
// This module is for node consumers: K2.4's importer, K2.5's page generator,
// and the tests. The browser gets a small self-contained copy of `resolve`
// emitted into the parameterised dashboard by `parameterize.mjs`, because a
// dashboard that is one `<script>` tag on a flat static site cannot import
// anything.

import { readFileSync } from 'node:fs';

/** causl's literals, at the config paths that replace them. Read straight off
 *  `tools/contract/upstream/dashboard.js` at the pinned revision. A test asserts
 *  these still match what the frozen file holds, so this table cannot quietly
 *  stop being upstream's. */
export const UPSTREAM_DEFAULTS = {
  'series.order': ['causl-ts', 'causl-wasm', 'jotai', 'redux-toolkit', 'redux-rtk', 'mobx'],
  'series.label': { 'redux-rtk': 'redux-toolkit (rtk runner)' },
  'series.color': {
    'causl-ts': '#11D9FF',
    'causl-wasm': '#5EE6A8',
    jotai: '#C8743D',
    'redux-toolkit': '#7C4DFF',
    'redux-rtk': '#7C4DFF',
    mobx: '#8FA2AA',
  },
  'series.dash': { 'redux-rtk': '6 3' },
  'series.annotation': {},
  'defaults.libraries': ['causl-ts', 'causl-wasm', 'mobx'],
  'defaults.scales': [10000],
  'verdict.regressionPct': 0.1,
  'verdict.improvedPct': 0.1,
  'verdict.passPct': 0.05,
  'verdict.noisyProxy': 0.5,
  'verdict.color': {
    pass: '#5EE6A8',
    regressed: '#FF646E',
    improved: '#4F7CFF',
    noisy: '#FFB347',
    unknown: '#A9B5C9',
  },
  // The two behaviours upstream has no equivalent of. Both default OFF, and
  // off is upstream's behaviour exactly.
  'verdict.noise.enabled': false,
  'verdict.noise.kind': 'noise',
  'verdict.noise.spreadField': 'replicateSpreadPct',
  'verdict.noise.onMissingSpread': 'skip',
  'verdict.noise.floorPct': null,
  'verdict.gate.enabled': false,
  'verdict.gate.field': 'gated',
  'verdict.gate.kind': 'ungated',
  'verdict.gate.label': 'measured, not gated',
  'verdict.noChipKinds': [],
  'samples.carry': [],
  // Upstream breaks an era on the measured library set and nothing else.
  'era.axes': [{ id: 'series', from: 'samples[].library', kind: 'set' }],
};

/** Read one dotted path out of a config object. Returns `fallback` when the
 *  config is absent, when any segment along the way is missing, or when the
 *  value found is null or undefined.
 *
 *  Objects and arrays REPLACE the fallback rather than merging into it. A merge
 *  would leave causl's six series ids sitting in libviprs's plot order, which is
 *  the shape of bug that is hardest to see on a page that still renders. */
export function resolve(config, path, fallback) {
  if (config === null || typeof config !== 'object') return fallback;
  let node = config;
  for (const seg of path.split('.')) {
    if (node === null || typeof node !== 'object' || !(seg in node)) return fallback;
    node = node[seg];
  }
  return node === null || node === undefined ? fallback : node;
}

/** `resolve` bound to one config, with the upstream literal supplied
 *  automatically for any path this contract knows about. */
export function reader(config) {
  return (path, fallback) =>
    resolve(config, path, fallback !== undefined ? fallback : UPSTREAM_DEFAULTS[path]);
}

export function loadConfig(path) {
  return JSON.parse(readFileSync(path, 'utf8'));
}

/** Read a dotted path out of a producer document. `a.b[].c` means "the values
 *  of `c` across the array at `a.b`". Used by the era axes and by the importer
 *  for the host fingerprint, so the paths in `config.json` are checkable against
 *  a real document rather than being prose. */
export function pluck(doc, path) {
  const parts = path.split('.');
  let nodes = [doc];
  for (const raw of parts) {
    const isSplat = raw.endsWith('[]');
    const seg = isSplat ? raw.slice(0, -2) : raw;
    const next = [];
    for (const n of nodes) {
      if (n === null || typeof n !== 'object') continue;
      const v = n[seg];
      if (v === undefined) continue;
      if (isSplat) {
        if (Array.isArray(v)) next.push(...v);
      } else {
        next.push(v);
      }
    }
    nodes = next;
  }
  return nodes;
}

const HEX = /^#[0-9A-Fa-f]{6}$/;

/** Structural checks the JSON Schema cannot make, because they are about
 *  agreement between two parts of the file rather than the shape of one part.
 *  Returns an array of problems; empty means fine. */
export function validate(config, { upstreamRev } = {}) {
  const bad = [];
  const at = (p, fb) => resolve(config, p, fb);

  if (upstreamRev && at('contract.upstreamRev', null) !== upstreamRev) {
    bad.push(
      `contract.upstreamRev is ${JSON.stringify(at('contract.upstreamRev', null))} but the frozen ` +
        `copy is pinned at ${upstreamRev}: a config written against one revision of dashboard.js ` +
        'is not automatically valid against another',
    );
  }

  const order = at('series.order', []);
  const known = new Set(order);
  for (const [k, v] of Object.entries(at('series.color', {}))) {
    if (!HEX.test(v)) bad.push(`series.color.${k} is not a #rrggbb colour: ${JSON.stringify(v)}`);
  }
  for (const key of ['label', 'color', 'dash', 'annotation']) {
    for (const id of Object.keys(at(`series.${key}`, {}))) {
      if (!known.has(id)) bad.push(`series.${key} names "${id}", which is not in series.order`);
    }
  }
  for (const id of at('defaults.libraries', [])) {
    if (!known.has(id)) bad.push(`defaults.libraries names "${id}", which is not in series.order`);
  }

  // Every verdict kind that can be produced needs a colour, or it renders in
  // `unknown`'s grey and silently reads as "no data" instead of as itself.
  const colors = at('verdict.color', {});
  for (const req of ['pass', 'regressed', 'improved', 'noisy', 'unknown']) {
    if (!(req in colors)) bad.push(`verdict.color is missing "${req}"`);
  }
  if (at('verdict.noise.enabled', false) && !(at('verdict.noise.kind', 'noise') in colors)) {
    bad.push(`verdict.noise is enabled but verdict.color has no "${at('verdict.noise.kind', 'noise')}"`);
  }
  if (at('verdict.gate.enabled', false) && !(at('verdict.gate.kind', 'ungated') in colors)) {
    bad.push(`verdict.gate is enabled but verdict.color has no "${at('verdict.gate.kind', 'ungated')}"`);
  }
  if (at('verdict.passPct', 0) > at('verdict.regressionPct', 0)) {
    bad.push('verdict.passPct is above verdict.regressionPct, so a delta could be both');
  }

  const axes = at('era.axes', []);
  if (!Array.isArray(axes) || axes.length === 0) {
    bad.push('era.axes is empty: every history entry would be one era and a run on a new machine ' +
      'would draw a line across two machines');
  }

  return bad;
}
