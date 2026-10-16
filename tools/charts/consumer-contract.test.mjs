// The renderer as a cross-repo contract (#103).
//
// libviprs-org checks this repo out at a pinned revision and imports
// tools/charts/render.mjs by path. That works today because nothing in the
// module graph reaches for the caller's cwd or this repo's report/ directory,
// which is a property nobody had checked from outside. This file consumes the
// renderer the way the downstream repo will: a separate directory, a separate
// process, a different cwd, and a decoy report/ in that cwd that must never be
// read.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, copyFileSync, readdirSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { spawnSync } from 'node:child_process';

const HERE = dirname(fileURLToPath(import.meta.url));
const RENDER = join(HERE, 'render.mjs');
const FIX = join(HERE, 'fixtures');

const PUBLIC_API = [
  'buildHistoryCharts', 'buildComparisonCharts', 'buildScalabilityCharts', 'renderAll',
  'historyShapeOk', 'scalabilityShapeOk', 'resultsShapeOk',
];

/** A minimal well-formedness check: balanced, properly nested tags, one root. */
function parsesAsXml(svg) {
  const body = svg.replace(/<\?xml[^>]*\?>/g, '').replace(/<!--[\s\S]*?-->/g, '');
  const stack = [];
  let roots = 0;
  for (const m of body.matchAll(/<(\/?)([A-Za-z][\w:-]*)((?:"[^"]*"|'[^']*'|[^>"'])*?)(\/?)>/g)) {
    const [, close, name, , selfClose] = m;
    if (selfClose) { if (!stack.length) roots++; continue; }
    if (!close) { if (!stack.length) roots++; stack.push(name); continue; }
    if (stack.pop() !== name) return false;
  }
  return stack.length === 0 && roots === 1 && body.trim().startsWith('<svg');
}

function consumer() {
  const dir = mkdtempSync(join(tmpdir(), 'consumer-'));
  // The decoy: if anything resolves report/ against the cwd, it finds this.
  mkdirSync(join(dir, 'report'));
  writeFileSync(join(dir, 'report', 'benchmark_results.json'), '[{"decoy":true}]');
  writeFileSync(join(dir, 'report', 'benchmark_history.json'), '[{"decoy":true}]');
  const data = join(dir, 'data');
  mkdirSync(data);
  for (const f of ['golden_history.json', 'golden_results.json', 'golden_scalability.json']) {
    copyFileSync(join(FIX, f), join(data, f));
  }
  return { dir, data, out: join(dir, 'out') };
}

function runConsumer(script, c) {
  writeFileSync(join(c.dir, 'consume.mjs'), script);
  const r = spawnSync(process.execPath, [join(c.dir, 'consume.mjs')], { cwd: c.dir, encoding: 'utf8' });
  assert.equal(r.status, 0, `consumer failed:\n${r.stderr}`);
  return r.stdout;
}

test('render.mjs exports the public API when imported by path from another directory', () => {
  const c = consumer();
  const out = runConsumer(
    `import * as r from ${JSON.stringify(pathToFileURL(RENDER).href)};
     console.log(JSON.stringify(Object.keys(r).sort()));`, c);
  const exported = JSON.parse(out);
  for (const name of PUBLIC_API) assert.ok(exported.includes(name), `missing export ${name}`);
});

test('renderAll from a foreign cwd reads only its arguments and writes only to outDir', () => {
  const c = consumer();
  const out = runConsumer(
    `import { renderAll } from ${JSON.stringify(pathToFileURL(RENDER).href)};
     const written = renderAll({
       historyPath: 'data/golden_history.json',
       resultsPath: 'data/golden_results.json',
       scalabilityPath: 'data/golden_scalability.json',
       outDir: 'out',
     });
     console.log(JSON.stringify(written));`, c);
  const written = JSON.parse(out);
  assert.ok(written.length >= 3, `expected a chart per family, got ${written.length}`);
  assert.deepEqual(readdirSync(c.dir).sort(), ['consume.mjs', 'data', 'out', 'report'],
    'nothing is written beside the caller outDir');
  assert.deepEqual(readdirSync(join(c.dir, 'report')).sort(),
    ['benchmark_history.json', 'benchmark_results.json'], 'the decoy report/ is untouched');

  const names = readdirSync(c.out).sort();
  assert.ok(names.every((n) => n.endsWith('.svg')));
  assert.ok(names.some((n) => n.startsWith('chart_history_')), 'history family emitted');
  assert.ok(names.some((n) => n.startsWith('chart_') && !n.startsWith('chart_history_')), 'comparison family emitted');
  assert.ok(names.some((n) => n.startsWith('scalability_')), 'scalability family emitted');
  for (const n of names) {
    const svg = readFileSync(join(c.out, n), 'utf8');
    assert.ok(parsesAsXml(svg), `${n} is not well-formed SVG`);
    assert.ok(!svg.includes('decoy'), `${n} drew the decoy data`);
  }
});

test('the emitted filenames match an in-repo run, so a pinned checkout renders the same set', () => {
  const c = consumer();
  runConsumer(
    `import { renderAll } from ${JSON.stringify(pathToFileURL(RENDER).href)};
     renderAll({ historyPath: 'data/golden_history.json', resultsPath: 'data/golden_results.json',
                 scalabilityPath: 'data/golden_scalability.json', outDir: 'out' });`, c);
  const inRepo = mkdtempSync(join(tmpdir(), 'inrepo-'));
  const r = spawnSync(process.execPath, [RENDER, '--history', join(FIX, 'golden_history.json'),
    '--results', join(FIX, 'golden_results.json'), '--scalability', join(FIX, 'golden_scalability.json'),
    '--out-dir', inRepo], { encoding: 'utf8' });
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(readdirSync(c.out).sort(), readdirSync(inRepo).sort());
});

// The two things the issue says would break a downstream import.
test('no renderer module resolves a path from the cwd', () => {
  for (const f of readdirSync(HERE).filter((n) => n.endsWith('.mjs') && !n.endsWith('.test.mjs'))) {
    const src = readFileSync(join(HERE, f), 'utf8');
    assert.ok(!/process\.cwd\s*\(/.test(src), `${f} reads process.cwd()`);
  }
});

test('the series vocabulary is data the caller can name, not an engines-only assumption', async () => {
  // A key the harness has never heard of must be drawn, not dropped or thrown on.
  const { buildComparisonCharts } = await import(pathToFileURL(RENDER).href);
  const results = JSON.parse(readFileSync(join(FIX, 'golden_results.json'), 'utf8'));
  const renamed = results.map((r) => ({ ...r, engine: r.engine === 'streaming' ? 'webp-lossless' : r.engine }));
  const charts = buildComparisonCharts(renamed);
  assert.ok(charts.length > 0);
  assert.ok(charts.some((c) => /webp-lossless/i.test(c.svg)), 'an unknown series is still labelled in the chart');
});
