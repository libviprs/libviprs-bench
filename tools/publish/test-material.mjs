// The material every test in this lane runs against.
//
// There is one rule here and it is the reason this file exists rather than a
// `fixtures/` directory full of hand-written JSON: **a test document is the real
// archived document with one thing changed**. Nothing in this lane builds a
// document from scratch.
//
// That is not tidiness. The first admission suite in this epic was green against
// a document shape the producer never emits, and the gap hid a digest that could
// never move: the suite asserted on a block whose key the producer spells
// differently, so the check was over `null` for every real document and could
// not fail. A fixture is a second producer, written by whoever is writing the
// test, and it agrees with the test by construction.
//
// So `mutate()` takes the archived document, applies one edit, re-seals it (the
// four digests and the archive index row recomputed the way the producer would),
// and hands back a temporary archive containing exactly that. A refusal test
// then fires on the one thing it changed and on nothing else, and the control
// that the unmutated document is *admitted* is what stops the whole suite from
// passing because everything is refused.

import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { computeDigests } from './canonical-json.mjs';

/** The archived runs this suite is allowed to run against, by family.
 *
 *  Two families and not one, because a cell in `storage` and a cell in
 *  `engines` are spelled differently and the importer has to identify both. The
 *  storage sweep varies the source image and spells it into the cell name; the
 *  engines sweep varies the thread budget and spells that in instead. A rule
 *  that reads as identity on one of them can be a coincidence on the other, and
 *  with only the storage document in the tree there was no way to find out.
 */
export const ARCHIVED = {
  storage: {
    runId: '20260914T145707Z-809ee8014d002518ce55edaceba698ca7a8b8a79-0bc00939',
    dir: fileURLToPath(new URL('../../archive/storage/', import.meta.url)),
  },
  engines: {
    runId: '20260915T021911Z-809ee8014d002518ce55edaceba698ca7a8b8a79-a7bac4a9',
    dir: fileURLToPath(new URL('../../archive/engines/', import.meta.url)),
  },
};

/** The storage run, which is what `family` defaults to everywhere below. */
export const ARCHIVED_RUN_ID = ARCHIVED.storage.runId;

export const IMPORTER = fileURLToPath(new URL('./import-run.mjs', import.meta.url));
export const CONFIG = fileURLToPath(new URL('./config.json', import.meta.url));

function archived(family) {
  const entry = ARCHIVED[family];
  // Not a default. A typo in a family name would otherwise read the storage
  // document under an engines-shaped assertion, and the test would be green
  // about a document it never opened.
  if (!entry) throw new Error(`no archived run for family ${JSON.stringify(family)}`);
  return entry;
}

/** The archived document exactly as the producer sealed it. */
export function archivedDocumentText(family = 'storage') {
  const { dir, runId } = archived(family);
  return readFileSync(join(dir, `${runId}.json`), 'utf8');
}

/** The archived document, parsed. */
export function archivedDocument(family = 'storage') {
  return JSON.parse(archivedDocumentText(family));
}

/** The archive index as the producer wrote it. */
export function archivedIndex(family = 'storage') {
  return JSON.parse(readFileSync(join(archived(family).dir, 'index.json'), 'utf8'));
}

const scratches = [];

/** A temporary directory that is removed when the process exits. */
export function scratch() {
  const dir = mkdtempSync(join(tmpdir(), 'k24-import-'));
  scratches.push(dir);
  return dir;
}

process.on('exit', () => {
  for (const dir of scratches) {
    try {
      rmSync(dir, { recursive: true, force: true });
    } catch {
      /* a scratch directory that outlives the run is not a test failure */
    }
  }
});

/** `2026-09-14T14:57:07.877Z` into `20260914T145707Z`, the producer's rule. */
function compactTimestamp(iso) {
  const withoutFraction = iso.includes('.')
    ? `${iso.split('.')[0]}${iso.endsWith('Z') ? 'Z' : ''}`
    : iso;
  return [...withoutFraction].filter((c) => /[0-9a-zA-Z]/.test(c)).join('');
}

/** The run id the producer would derive from this document.
 *
 *  `host8` is deliberately NOT recomputed here: it is a sha256 over six
 *  provenance fields and re-deriving it in the test helper would make the helper
 *  a second implementation of the thing the importer checks. A mutation that
 *  moves one of those six fields therefore keeps the original bucket, which is
 *  what a test wants: the run id changes only where the mutation says it should.
 */
function runIdFor(doc, host8) {
  return `${compactTimestamp(doc.startedAt)}-${doc.provenance.library.commit}-${host8}`;
}

/** Follow a dotted path into an object, returning the parent and the last key. */
function locate(root, path) {
  const parts = path.split('.');
  let node = root;
  for (const p of parts.slice(0, -1)) {
    node = node[p];
    if (node === undefined) throw new Error(`no such path: ${path}`);
  }
  return [node, parts[parts.length - 1]];
}

/**
 * The archived document with one thing changed, sealed and archived.
 *
 * @param {object} edits
 * @param {Record<string, unknown>} [edits.set]     dotted path -> new value
 * @param {string[]}                [edits.remove]  dotted paths to delete
 * @param {(doc: object) => void}   [edits.edit]    an edit the paths cannot express
 * @param {boolean}                 [edits.reseal]  false to leave the stale digests in place
 * @param {boolean}                 [edits.index]   false to write an archive with no index row
 * @param {string}                  [edits.family]  which archived run to start from
 * @returns {{dir: string, doc: object, runId: string, documentPath: string}}
 */
export function mutate(edits = {}) {
  const family = edits.family ?? 'storage';
  const doc = archivedDocument(family);
  const originalHost8 = archived(family).runId.split('-').pop();

  for (const [path, value] of Object.entries(edits.set ?? {})) {
    const [parent, key] = locate(doc, path);
    parent[key] = value;
  }
  for (const path of edits.remove ?? []) {
    const [parent, key] = locate(doc, path);
    delete parent[key];
  }
  if (edits.edit) edits.edit(doc);

  const reseal = edits.reseal !== false;
  if (reseal) {
    delete doc.integrity;
    doc.integrity = computeDigests(doc);
  }

  const runId = runIdFor(doc, originalHost8);
  const dir = join(scratch(), 'archive');
  mkdirSync(dir, { recursive: true });
  const documentPath = join(dir, `${runId}.json`);
  writeFileSync(documentPath, `${JSON.stringify(doc, null, 2)}\n`);

  if (edits.index !== false) {
    const rows = [
      {
        runId,
        documentDigest: doc.integrity?.document ?? null,
        startedAt: doc.startedAt,
        libraryCommit: doc.provenance?.library?.commit ?? null,
        emulated: doc.provenance?.emulated ?? null,
        fsType: doc.provenance?.filesystem?.fsType ?? null,
        file: `${runId}.json`,
      },
    ];
    writeFileSync(join(dir, 'index.json'), `${JSON.stringify(rows, null, 2)}\n`);
  }

  return { dir, doc, runId, documentPath };
}

/** The unmutated archived document, copied into a temporary archive.
 *
 *  The control for every refusal test. If this one does not import, a suite full
 *  of refusals proves nothing at all.
 */
export function pristine(family = 'storage') {
  return mutate({ family });
}

/** An empty `history.json`, ready to import into. */
export function emptyHistory() {
  const path = join(scratch(), 'history.json');
  writeFileSync(path, '[]\n');
  return path;
}
