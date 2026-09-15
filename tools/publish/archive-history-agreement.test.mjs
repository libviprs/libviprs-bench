// The published history is the truth of what this repository has published, not
// a subset of it.
//
// It was a subset, and the asymmetry that produced was invisible until something
// drew it. `main` carried four archived documents and a history naming one. Two
// of them were the x86_64 runs the live page cites by digest, which were not in
// this repository at all (#98, #99). The fourth was the arm64 `engines` sweep,
// archived and simply never imported, so `storage` had two runs and `engines`
// had one, the page was lopsided across a comparison it exists to make, and the
// reason had nothing to do with any measurement.
//
// Both directions are defects and they are different defects:
//
//   * archived and not in the history is a run this repository can prove and
//     does not publish. That is the shape above, and the page silently loses a
//     point on an axis whose whole job is to carry it.
//   * in the history and not archived is a number with no artefact behind it,
//     which is the thing every gate in this suite exists to stop. libviprs-org
//     refuses it at ingest now, and this catches it a repository earlier.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const readJson = (p) => JSON.parse(readFileSync(p, 'utf8'));

/** Every family directory under `archive/`, found rather than listed, so a new
 *  family is covered the day it is archived instead of the day somebody
 *  remembers this file. */
function archiveFamilies() {
  const root = join(repoRoot, 'archive');
  return readdirSync(root, { withFileTypes: true })
    .filter((e) => e.isDirectory() && existsSync(join(root, e.name, 'index.json')))
    .map((e) => e.name);
}

test('every archived run is in the published history, and every published run is archived', () => {
  const families = archiveFamilies();
  // A positive control on the walk. With no families found, both sets below are
  // empty, they agree, and the test passes having compared nothing.
  assert.ok(families.length > 0, 'the archive has family directories to read');

  const archived = new Map();
  for (const family of families) {
    for (const row of readJson(join(repoRoot, 'archive', family, 'index.json'))) {
      archived.set(row.runId, family);
    }
  }
  assert.ok(archived.size > 0, `the archive holds runs; found families ${families.join(', ')}`);

  const history = readJson(join(repoRoot, 'tools', 'publish', 'history.json'));
  const published = new Map(history.map((entry) => [entry.runId, entry.family]));

  const notPublished = [...archived].filter(([runId]) => !published.has(runId));
  assert.deepEqual(
    notPublished.map(([runId, family]) => `${family} ${runId}`),
    [],
    'these runs are archived and the history does not name them, so this repository can prove ' +
      'them and does not publish them. Import each one with tools/publish/import-run.mjs. If a ' +
      'run is archived that should never be published, a `ci` sweep for instance, the archive ' +
      'is the wrong place for it: it is the door to the page and `ci` archives ' +
      'indistinguishably from a calibrated run.',
  );

  const notArchived = [...published].filter(([runId]) => !archived.has(runId));
  assert.deepEqual(
    notArchived.map(([runId, family]) => `${family} ${runId}`),
    [],
    'these runs are in the history and no archive index names them, so the page would draw a ' +
      'number with no artefact behind it. That is the state the live page was in before #98.',
  );
});

test('each archived run id resolves to a file that is actually there', () => {
  // The index is a claim about files. A row whose file is missing reads as an
  // archived run right up until somebody tries to check it, which is the moment
  // it matters.
  let rows = 0;
  for (const family of archiveFamilies()) {
    const dir = join(repoRoot, 'archive', family);
    for (const row of readJson(join(dir, 'index.json'))) {
      assert.ok(
        existsSync(join(dir, row.file)),
        `${family} index names ${row.file} and that file is not in ${dir}`,
      );
      rows += 1;
    }
  }
  assert.ok(rows > 0, 'there were rows to check');
});
