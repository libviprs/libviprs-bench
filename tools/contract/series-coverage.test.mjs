// Every library the history feed carries is a series the config names.
//
// This exists because the config declared two and the feed carried five. The
// engines family was added in #75 and the dashboard config was not extended, so
// `mapreduce`, `monolithic` and `streaming` had no label, no colour and no
// place in the draw order. Nothing failed: the dashboard fell through to the
// raw id, and the tell was a lowercase word in a legend.
//
// `config-coverage.test.mjs` could not catch it. That file checks the config
// KEYS the importer reads, which is a different axis: every key it wanted was
// defined. Nobody was checking that the values covered the data.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '..', '..');
const config = JSON.parse(readFileSync(join(here, 'config.json'), 'utf8'));
const history = JSON.parse(readFileSync(join(repo, 'tools', 'publish', 'history.json'), 'utf8'));

/**
 * Every library id the feed mentions.
 *
 * `libraries` is an object keyed by id, not an array. Iterating it as an array
 * throws rather than returning nothing, which is the better failure: a reader
 * that quietly found no libraries would have made this whole file pass while
 * checking nothing.
 */
function librariesInFeed() {
  const out = new Set();
  for (const entry of history) {
    const libs = entry.libraries;
    if (!libs) continue;
    if (Array.isArray(libs)) {
      for (const lib of libs) {
        const id = typeof lib === 'string' ? lib : (lib?.id ?? lib?.name ?? lib?.backend);
        if (id) out.add(id);
      }
    } else if (typeof libs === 'object') {
      for (const id of Object.keys(libs)) out.add(id);
    }
  }
  return [...out].sort();
}

test('the reader actually finds libraries, so a pass below means something', () => {
  const found = librariesInFeed();
  assert.ok(found.length >= 2, `found ${found.length}: ${found.join(', ')}`);
});

test('the feed carries more than one family, so this is not a storage-only config', () => {
  const families = new Set(history.map((e) => e.family));
  assert.ok(families.size >= 2, `only ${[...families].join(', ')}; the point of this file is the second family`);
});

test('every library in the feed has a declared label', () => {
  const missing = librariesInFeed().filter((id) => !config.series.label[id]);
  assert.deepEqual(missing, [], `undeclared: ${missing.join(', ')} — the dashboard would draw the raw id`);
});

test('every library in the feed has a declared colour', () => {
  const missing = librariesInFeed().filter((id) => !config.series.color[id]);
  assert.deepEqual(missing, [], `undeclared: ${missing.join(', ')} — the dashboard would fall back`);
});

test('every library in the feed has a place in the draw order', () => {
  const missing = librariesInFeed().filter((id) => !config.series.order.includes(id));
  assert.deepEqual(missing, [], `unordered: ${missing.join(', ')}`);
});

test('every declared colour is a validated #rrggbb', () => {
  for (const [id, colour] of Object.entries(config.series.color)) {
    assert.match(colour, /^#[0-9a-f]{6}$/, `${id} is ${colour}`);
  }
});

test('no two series share a colour, because that is a chart that lies', () => {
  const used = Object.entries(config.series.color);
  const seen = new Map();
  for (const [id, colour] of used) {
    assert.ok(!seen.has(colour), `${id} and ${seen.get(colour)} both draw ${colour}`);
    seen.set(colour, id);
  }
});

test('the default view names only libraries the config declares', () => {
  for (const id of config.defaults?.libraries ?? []) {
    assert.ok(config.series.label[id], `${id} is the default view and is not declared`);
  }
});
