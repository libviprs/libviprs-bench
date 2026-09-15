// Canonical JSON and the four integrity digests, in JavaScript.
//
// This is the reader half of `libviprs-bench/src/storage/integrity.rs`. The
// producer seals a document in Rust and writes four sha256 digests into it; the
// importer has to recompute them or "archived by digest" is a sentence nobody
// ever checks. Everything below is the same four rules, and the reason each one
// is spelled out rather than delegated to `JSON.stringify` is that this side is
// where a Rust port silently disagrees:
//
//   1. **Key order.** Ascending, arrays untouched. JavaScript sorts by UTF-16
//      code unit and Rust by UTF-8 byte, and the two differ above the BMP, so a
//      non-ASCII key is refused rather than digested two ways.
//   2. **Absent is not null.** A key that is not there is not emitted; an
//      explicit `null` is emitted as `null`. The producer is forbidden
//      `skip_serializing_if` for the same reason.
//   3. **Numbers.** `JSON.stringify(1.0)` is `1` and serde_json writes `1.0`,
//      so an integral value prints as a plain integer and `-0` prints as `0`.
//      Outside plain notation the two languages disagree outright (`1e+21`
//      against the digits in full), so anything at or above 1e21, anything
//      non-zero below 1e-6, any integer past 2^53-1 and anything non-finite is
//      refused.
//   4. **Strings.** RFC 8259 escaping, which is what both `JSON.stringify` and
//      serde_json already produce for a well-formed string.
//
// The Rust side carries a fifth rule that has no analogue here: it turns on
// serde_json's `float_roundtrip` because its default reader is not correctly
// rounded. V8's `JSON.parse` already is, which is exactly why the Rust side had
// to be fixed rather than this one.
//
// A digest is `sha256:` followed by 64 lowercase hex characters over the UTF-8
// bytes of the canonical string.

import { createHash } from 'node:crypto';

/** Largest integer a JavaScript number holds exactly, 2^53 - 1. */
const MAX_SAFE_INTEGER = 9007199254740991;
/** Below this magnitude `Number.prototype.toString` switches to exponent form. */
const SMALLEST_PLAIN_MAGNITUDE = 1e-6;
/** At and above this magnitude it switches to exponent form. */
const LARGEST_PLAIN_MAGNITUDE = 1e21;

/** A value this canonicaliser will not digest, and the path that carries it.
 *
 *  The path is not decoration. "a number somewhere in this document is out of
 *  range" is not a message anyone can act on across a document with 343 cells.
 */
export class CanonicalError extends Error {
  constructor(message, path) {
    super(`${path}: ${message}`);
    this.name = 'CanonicalError';
    this.path = path;
  }
}

/** The three block digests, and the document key each one actually covers.
 *
 *  The names differ on purpose and the difference has already cost this
 *  mechanism one of its four digests. The digest names are causl's and are
 *  plural; the document field is `runner`, singular, because this family has one
 *  runner. Spelling the document key `runners` here would look up a key no
 *  document has, digest JSON `null`, and produce the same value for every
 *  document this producer will ever write, in the block whose whole purpose is
 *  to say which part moved.
 */
const BLOCKS = [
  ['cells', 'cells'],
  ['runners', 'runner'],
  ['measurements', 'measurement'],
];

/** The document keys the three block digests cover. */
export function digestedKeys() {
  return BLOCKS.map(([, key]) => key);
}

/** The canonical JSON string for `value`. */
export function canonicalJson(value) {
  return write(value, '$');
}

/** `sha256:<64 lowercase hex>` over the canonical JSON of `value`. */
export function digestOf(value) {
  return `sha256:${createHash('sha256').update(canonicalJson(value), 'utf8').digest('hex')}`;
}

/** The document as the `document` digest sees it: everything but two keys.
 *
 *  Sealing has to be idempotent. If the digest covered `integrity` then writing
 *  the digest in would change the digest and verification could never pass;
 *  `combinedAt` comes out because two aggregations of the same evidence differ
 *  only by a wall clock, and a digest that moved because time passed is a
 *  refusal nobody can clear.
 */
export function documentEvidence(doc) {
  if (doc === null || typeof doc !== 'object' || Array.isArray(doc)) return doc;
  const out = { ...doc };
  delete out.integrity;
  delete out.combinedAt;
  return out;
}

/** All four digests over `doc`.
 *
 *  A block that is absent still gets a digest, over JSON `null`: a document that
 *  lost its `cells` must not digest the same as one that never had the key, and
 *  a missing block is the importer's business to refuse rather than this
 *  function's business to paper over.
 */
export function computeDigests(doc) {
  const [cells, runners, measurements] = BLOCKS.map(([, key]) =>
    digestOf(doc?.[key] === undefined ? null : doc[key]),
  );
  return { cells, runners, measurements, document: digestOf(documentEvidence(doc)) };
}

function write(value, path) {
  if (value === null) return 'null';
  switch (typeof value) {
    case 'boolean':
      return value ? 'true' : 'false';
    case 'number':
      return number(value, path);
    case 'string':
      return JSON.stringify(value);
    case 'object':
      break;
    default:
      // `undefined`, a function or a symbol cannot come out of `JSON.parse`, so
      // reaching here means the caller built the value by hand and rule 2 says
      // absent and null are different documents. Refusing is the only answer
      // that does not silently pick one of them.
      throw new CanonicalError(`cannot canonicalise a ${typeof value}`, path);
  }
  if (Array.isArray(value)) {
    return `[${value.map((item, i) => write(item, `${path}[${i}]`)).join(',')}]`;
  }
  const keys = Object.keys(value);
  for (const key of keys) {
    // eslint-disable-next-line no-control-regex
    if (!/^[\x00-\x7F]*$/.test(key)) {
      throw new CanonicalError(
        `the key ${JSON.stringify(key)} is not ASCII, and JavaScript and Rust do not agree ` +
          'about where it sorts, so its digest would depend on which language computed it',
        path,
      );
    }
  }
  keys.sort();
  return `{${keys
    .map((key) => `${JSON.stringify(key)}:${write(value[key], `${path}.${key}`)}`)
    .join(',')}}`;
}

function number(v, path) {
  if (!Number.isFinite(v)) {
    throw new CanonicalError(
      'is NaN or an infinity; JSON.stringify would write null here and lose the fact that a ' +
        'measurement went wrong',
      path,
    );
  }
  if (Number.isInteger(v)) {
    if (Math.abs(v) > MAX_SAFE_INTEGER) {
      throw new CanonicalError(
        `${v} is beyond 2^53-1, so JavaScript cannot hold it exactly and the two languages ` +
          'would digest different values while both believing they read the file correctly',
        path,
      );
    }
    // `Object.is(v, -0)` is the one case where the integer path and
    // `JSON.stringify` disagree: `String(-0)` is `"0"` and so is
    // `JSON.stringify(-0)`, but writing `-0` out by hand would not be.
    return Object.is(v, -0) ? '0' : String(v);
  }
  const magnitude = Math.abs(v);
  if (magnitude >= LARGEST_PLAIN_MAGNITUDE || magnitude < SMALLEST_PLAIN_MAGNITUDE) {
    throw new CanonicalError(
      `${v} falls outside plain decimal notation, where JavaScript writes an exponent and Rust ` +
        'writes the digits out in full; a benchmark document has no business carrying it',
      path,
    );
  }
  return String(v);
}
