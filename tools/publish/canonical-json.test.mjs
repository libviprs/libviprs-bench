// The canonicalisation half of the importer, tested against the one real
// document this epic has rather than against values I made up.
//
// Every test names the wrong implementation it goes red against, because a
// canonicaliser has no observable behaviour except its digests and a test that
// only checks "it produced 64 hex characters" passes against every one of the
// four ways this is known to go wrong.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import {
  canonicalJson,
  digestOf,
  computeDigests,
  documentEvidence,
  CanonicalError,
} from './canonical-json.mjs';

import { ARCHIVED_RUN_ID, archivedDocumentText, archivedDocument } from './test-material.mjs';

test('the javascript digests reproduce the producers own integrity block', () => {
  const doc = archivedDocument();
  const recomputed = computeDigests(doc);

  // Not "they are 64 hex characters". The producer sealed this file in Rust and
  // wrote the four digests into it; if JavaScript disagrees about key order,
  // about an integral float, about absent-versus-null or about float parsing,
  // one of these four differs and a reader can never verify an archived run.
  //
  // RED against: sorting keys by code point, printing `1.0` rather than `1`,
  // dropping a null-valued key, or digesting the whole file including
  // `integrity`.
  assert.deepEqual(recomputed, doc.integrity);
});

test('the document digest ignores integrity and combinedAt', () => {
  const doc = archivedDocument();
  const evidence = documentEvidence(doc);

  assert.ok(!('integrity' in evidence), 'integrity must be out of its own digest');
  assert.ok(!('combinedAt' in evidence), 'the wall clock must be out of the digest');

  // Sealing has to be idempotent: re-sealing an already-sealed document must
  // not move the digest. RED against a digest over the whole file, which could
  // never verify once it was written back in.
  const resealed = { ...doc, combinedAt: '2099-01-01T00:00:00.000Z' };
  assert.equal(digestOf(documentEvidence(resealed)), doc.integrity.document);
});

test('absent and null digest differently', () => {
  // `Object.keys` enumerates a key whose value is undefined and `value ?? null`
  // writes it as null, so the producer is forbidden from using
  // `skip_serializing_if`. The two shapes must not collide here either.
  //
  // RED against a canonicaliser that drops null-valued keys, which is what a
  // naive "omit empty" port does.
  assert.notEqual(canonicalJson({ a: 1, b: null }), canonicalJson({ a: 1 }));
  assert.equal(canonicalJson({ a: 1, b: null }), '{"a":1,"b":null}');
});

test('keys are sorted ascending and arrays are left alone', () => {
  assert.equal(canonicalJson({ b: 1, a: 2, C: 3 }), '{"C":3,"a":2,"b":1}');
  // RED against a canonicaliser that sorts arrays too, which would make two
  // different cell orders digest identically.
  assert.equal(canonicalJson([3, 1, 2]), '[3,1,2]');
});

test('a non ascii key is refused rather than digested two ways', () => {
  // JavaScript sorts by UTF-16 code unit and Rust by UTF-8 byte, and the two
  // orders differ above the BMP. RED against a canonicaliser that just sorts
  // and hopes.
  assert.throws(() => canonicalJson({ 'é': 1 }), CanonicalError);
  assert.throws(() => canonicalJson({ nested: { '\u{10000}': 1 } }), CanonicalError);
});

test('a number outside plain notation is refused rather than written in exponent form', () => {
  // `Number.prototype.toString` switches to exponent form at 1e21 and below
  // 1e-6, where Rust writes the digits out in full. RED against a canonicaliser
  // that hands the number to JSON.stringify and moves on.
  assert.throws(() => canonicalJson({ v: 1e21 }), CanonicalError);
  assert.throws(() => canonicalJson({ v: 1e-7 }), CanonicalError);
  assert.equal(canonicalJson({ v: 1e-6 }), '{"v":0.000001}');
  // The producer allows an f64 of 1e20 (it is below 1e21 and prints the same in
  // both languages) and refuses a u64 above 2^53-1. JavaScript cannot tell the
  // two apart after JSON.parse, so this side refuses both: admitting the pair
  // would mean admitting a digest the producer would have refused, and no
  // benchmark document carries a number up there anyway.
  assert.throws(() => canonicalJson({ v: 1e20 }), CanonicalError);
  assert.equal(canonicalJson({ v: 0 }), '{"v":0}');
  assert.equal(canonicalJson({ v: -0 }), '{"v":0}');
});

test('a non finite number and an unsafe integer are refused', () => {
  // JSON.stringify writes NaN as null and loses the fact that a measurement
  // went wrong. RED against exactly that.
  assert.throws(() => canonicalJson({ v: NaN }), CanonicalError);
  assert.throws(() => canonicalJson({ v: Infinity }), CanonicalError);
  assert.throws(() => canonicalJson({ v: 9007199254740992 }), CanonicalError);
  assert.equal(canonicalJson({ v: 9007199254740991 }), '{"v":9007199254740991}');
});

test('an integral value prints without a fractional part', () => {
  // The single character that separates `JSON.stringify(1.0)` from serde_json's
  // `1.0`. RED against a port that preserves the producer's float formatting.
  assert.equal(canonicalJson({ v: 2.0 }), '{"v":2}');
  assert.equal(canonicalJson({ v: 2.5 }), '{"v":2.5}');
});

test('the refusal names the path of the value it refused', () => {
  // "a number somewhere in this document is out of range" is not something a
  // reader can act on. RED against a bare throw.
  try {
    canonicalJson({ cells: [{ median: NaN }] });
    assert.fail('expected a refusal');
  } catch (e) {
    assert.match(e.message, /\$\.cells\[0\]\.median/);
  }
});

test('the archived text parses to the document the digests cover', () => {
  // A positive control on the material itself: if the checked-in file ever
  // stops being the bytes the producer sealed, every other test in this lane is
  // measuring something else.
  const text = archivedDocumentText();
  const doc = JSON.parse(text);
  assert.equal(doc.integrity.document, digestOf(documentEvidence(doc)));
  assert.match(ARCHIVED_RUN_ID, /^20260914T145707Z-809ee8014d002518ce55edaceba698ca7a8b8a79-0bc00939$/);
  assert.equal(readFileSync(new URL('../../archive/storage/index.json', import.meta.url), 'utf8').includes(ARCHIVED_RUN_ID), true);
});
