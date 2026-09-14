//! Canonical JSON, the four digests, and a `--verify` that names the block that
//! moved.
//!
//! # The canonicalisation rules, pinned here on purpose
//!
//! K2.2 will pin a cross-language test: these digests must reproduce, byte for
//! byte, what causl's JavaScript `digest.mjs` produces over the same document.
//! That test is not written yet, and the rules it will hold me to are decided
//! now rather than discovered then, because reconciling a float format across
//! two languages after both sides have shipped archives costs an afternoon and
//! invalidates every digest already written down.
//!
//! The reference is `causl-bench/tools/suite/regression-gate.mjs`,
//! `canonicalJson` at line 1502 and `baselineDigest` at 1529:
//!
//! ```js
//! function canonicalJson(value) {
//!   if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`
//!   if (value && typeof value === 'object') {
//!     return `{${Object.keys(value).sort()
//!       .map((k) => `${JSON.stringify(k)}:${canonicalJson(value[k])}`).join(',')}}`
//!   }
//!   return JSON.stringify(value ?? null)
//! }
//! ```
//!
//! Read literally that is four decisions, and three of them are places where a
//! Rust port silently disagrees.
//!
//! **1. Key order.** Object keys ascending, arrays left alone. JavaScript's
//! `Array.prototype.sort()` with no comparator sorts by UTF-16 code unit;
//! Rust's `sort()` on `&str` sorts by UTF-8 byte, which is code point order.
//! Those two orders are not the same: `"\u{10000}"` is `D800 DC00` in UTF-16,
//! so JavaScript sorts it *before* `"\u{FFFD}"`, while Rust sorts it after.
//! They agree for every key that is pure ASCII, so [`canonical_json`] **refuses
//! a non-ASCII key** rather than emit a string whose digest depends on which
//! language computed it. Nothing in a benchmark document needs a non-ASCII key
//! and a refusal is cheaper than a divergence nobody can see.
//!
//! **2. Absent is not null.** `Object.keys` enumerates a key whose value is
//! `undefined`, and `value ?? null` then writes it as `null`; a key that is not
//! there at all is not enumerated. So an absent key and an explicit `null` are
//! different documents with different digests, and that is the rule here too.
//! The consequence for the producer is a hard one: **no
//! `#[serde(skip_serializing_if)]` anywhere in a storage document**. A field
//! that Rust drops and JavaScript writes as `null` is exactly the divergence
//! this paragraph exists to prevent, and it is invisible until the
//! cross-language test fails.
//!
//! **3. Numbers.** `JSON.stringify(1.0)` is `"1"`; `serde_json` writes `1.0`.
//! That single character is the whole trap. The rule: a value with no
//! fractional part prints as a plain decimal integer, no `.0` and no exponent,
//! whatever Rust type it arrived in, so a counter that happens to be `f64` and a
//! mean that happens to land on a whole number digest identically. `-0.0`
//! prints as `0`, matching `JSON.stringify(-0)`. Everything else prints as
//! Rust's shortest round-trip decimal, which is the same digit string
//! `Number.prototype.toString` produces, *in the range where both use plain
//! notation*. Outside that range they diverge — JavaScript writes `1e+21` and
//! `1e-7` where Rust writes the digits out in full — so [`canonical_json`]
//! refuses any value at or above `1e21` or strictly below `1e-6`. Those are the
//! two thresholds in ECMA-262's `Number::toString`, and a benchmark document
//! has no business carrying a number outside them. Non-finite values are
//! refused rather than written as `null`, because a `NaN` in a measurement is a
//! bug and `null` would hide it.
//!
//! **4. Strings.** Valid UTF-8 (Rust cannot hold anything else), escaped per RFC
//! 8259: `\"` and `\\`, the five two-character control escapes `\b \t \n \f
//! \r`, `\u00xx` for the remaining C0 controls, and every other character
//! literal — `/` unescaped, non-ASCII unescaped as UTF-8. That is what
//! `JSON.stringify` does for a well-formed string and what `serde_json` does,
//! and they agree.
//!
//! Two more, which are format rather than canonicalisation: a digest is
//! `sha256:` followed by 64 lowercase hex characters, over the UTF-8 bytes of
//! the canonical string; and the `document` digest is taken over the document
//! with `integrity` and `combinedAt` removed from the top level, so that
//! sealing a document does not change the thing that was sealed.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::sha256::sha256_hex;

/// Largest integer a JavaScript `Number` holds exactly, `2^53 - 1`.
///
/// An integer above it survives the Rust side perfectly and loses its low bits
/// the moment JavaScript parses the document, so the two languages would digest
/// different values while both believing they had read the file correctly.
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Below this magnitude `Number.prototype.toString` switches to exponent form.
const SMALLEST_PLAIN_MAGNITUDE: f64 = 1e-6;
/// At and above this magnitude `Number.prototype.toString` switches to exponent
/// form.
const LARGEST_PLAIN_MAGNITUDE: f64 = 1e21;

/// Why a document could not be canonicalised.
///
/// Every variant carries the JSON path to the offending value, because "a
/// number somewhere in this document is out of range" is not a message anyone
/// can act on.
#[derive(Debug, Clone, PartialEq)]
pub enum CanonicalError {
    /// `NaN` or an infinity. JavaScript would write `null` here and lose the
    /// fact that a measurement went wrong.
    NonFinite { path: String },
    /// A finite number that `Number.prototype.toString` would print in
    /// exponent form, where Rust prints the digits out in full.
    OutOfPlainRange { path: String, value: f64 },
    /// An integer JavaScript cannot hold exactly.
    UnsafeInteger { path: String, value: String },
    /// A key whose sort position differs between UTF-16 and UTF-8 order.
    NonAsciiKey { path: String, key: String },
}

impl std::fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CanonicalError::NonFinite { path } => write!(
                f,
                "{path} is NaN or an infinity; a digest over it would be a digest over a \
                 measurement that went wrong, written as though it had not"
            ),
            CanonicalError::OutOfPlainRange { path, value } => write!(
                f,
                "{path} is {value:e}, which JavaScript prints in exponent form and Rust prints \
                 in full, so the two languages would digest different bytes"
            ),
            CanonicalError::UnsafeInteger { path, value } => write!(
                f,
                "{path} is {value}, beyond 2^53-1, so JavaScript loses its low bits on parse \
                 while Rust keeps them"
            ),
            CanonicalError::NonAsciiKey { path, key } => write!(
                f,
                "{path} has the non-ASCII key {key:?}; JavaScript sorts keys by UTF-16 code \
                 unit and Rust by UTF-8 byte, and those two orders only agree on ASCII"
            ),
        }
    }
}

impl std::error::Error for CanonicalError {}

/// The canonical JSON string for `value`, per the rules in this module's docs.
pub fn canonical_json(value: &Value) -> Result<String, CanonicalError> {
    let mut out = String::new();
    write_canonical(value, "$", &mut out)?;
    Ok(out)
}

/// `sha256:<64 lowercase hex>` over the UTF-8 bytes of [`canonical_json`].
pub fn digest(value: &Value) -> Result<String, CanonicalError> {
    Ok(format!("sha256:{}", sha256_hex(canonical_json(value)?.as_bytes())))
}

fn write_canonical(value: &Value, path: &str, out: &mut String) -> Result<(), CanonicalError> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&canonical_number(n, path)?),
        Value::String(s) => out.push_str(&canonical_string(s)),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, &format!("{path}[{i}]"), out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, key) in sorted_keys(map, path)?.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&canonical_string(key));
                out.push(':');
                write_canonical(&map[key], &format!("{path}.{key}"), out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// Object keys in ascending order, having first established that ascending
/// order means the same thing in both languages.
fn sorted_keys<'a>(
    map: &'a Map<String, Value>,
    path: &str,
) -> Result<Vec<&'a String>, CanonicalError> {
    let mut keys: Vec<&String> = map.keys().collect();
    for key in &keys {
        if !key.is_ascii() {
            return Err(CanonicalError::NonAsciiKey {
                path: path.to_string(),
                key: (*key).clone(),
            });
        }
    }
    keys.sort();
    Ok(keys)
}

/// A number as `JSON.stringify` would write it, or a refusal where the two
/// languages would disagree.
fn canonical_number(n: &Number, path: &str) -> Result<String, CanonicalError> {
    if let Some(u) = n.as_u64() {
        return if u > MAX_SAFE_INTEGER as u64 {
            Err(CanonicalError::UnsafeInteger {
                path: path.to_string(),
                value: u.to_string(),
            })
        } else {
            Ok(u.to_string())
        };
    }
    if let Some(i) = n.as_i64() {
        return if i < -MAX_SAFE_INTEGER {
            Err(CanonicalError::UnsafeInteger {
                path: path.to_string(),
                value: i.to_string(),
            })
        } else {
            Ok(i.to_string())
        };
    }

    let v = n.as_f64().ok_or_else(|| CanonicalError::NonFinite {
        path: path.to_string(),
    })?;
    if !v.is_finite() {
        return Err(CanonicalError::NonFinite {
            path: path.to_string(),
        });
    }
    // `JSON.stringify(-0)` is `"0"`, and `0.0 == -0.0` in Rust, so this one
    // comparison covers both zeros before any sign can reach the output.
    if v == 0.0 {
        return Ok("0".to_string());
    }
    let magnitude = v.abs();
    if magnitude >= LARGEST_PLAIN_MAGNITUDE || magnitude < SMALLEST_PLAIN_MAGNITUDE {
        return Err(CanonicalError::OutOfPlainRange {
            path: path.to_string(),
            value: v,
        });
    }
    // A whole number prints as an integer whatever Rust type it arrived in.
    // This is the `1.0` against `1` trap, and it is the single most likely way
    // for the two languages to produce different digests over identical data.
    if v.fract() == 0.0 && magnitude <= MAX_SAFE_INTEGER as f64 {
        return Ok(format!("{}", v as i64));
    }
    // Rust's `Display` for `f64` is the shortest decimal that round-trips and
    // never uses exponent notation, which inside the range checked above is the
    // same digit string `Number.prototype.toString` produces.
    let formatted = format!("{v}");
    debug_assert!(
        !formatted.contains(['e', 'E']),
        "Rust's float Display produced exponent notation for {v}, which the range check \
         above was supposed to make impossible"
    );
    Ok(formatted)
}

/// A string as `JSON.stringify` would write it.
fn canonical_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The four digests a sealed storage document carries.
///
/// Four rather than one so `--verify` can say *what* moved. A single
/// whole-file hash tells a reader that something changed and leaves them to
/// diff 480 lines of numbers to find out what; these four turn that into "the
/// cells moved and the runner did not", which is the difference between a
/// re-measured sweep and a tampered-with one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Digests {
    /// Over `cells`.
    pub cells: String,
    /// Over `runners`.
    pub runners: String,
    /// Over `measurement`.
    pub measurements: String,
    /// Over the whole document with `integrity` and `combinedAt` removed.
    pub document: String,
}

/// The blocks, in the order [`Digests`] declares them, paired with the
/// top-level document key each one covers.
///
/// `document` is the odd one out and is handled separately, because it is not a
/// key but the whole thing minus two.
const BLOCKS: [(&str, &str); 3] = [
    ("cells", "cells"),
    ("runners", "runners"),
    ("measurements", "measurement"),
];

/// Compute all four digests over `doc`.
///
/// A block that is absent from the document still gets a digest, over JSON
/// `null`. That is deliberate: a document that lost its `runners` block
/// entirely must not produce the same `runners` digest as one that never had
/// the key, and a missing block is the aggregator's business to refuse, not
/// this function's business to paper over.
pub fn compute_digests(doc: &Value) -> Result<Digests, CanonicalError> {
    let mut computed = Vec::with_capacity(3);
    for (_, key) in BLOCKS {
        computed.push(digest(doc.get(key).unwrap_or(&Value::Null))?);
    }
    Ok(Digests {
        cells: computed[0].clone(),
        runners: computed[1].clone(),
        measurements: computed[2].clone(),
        document: digest(&document_evidence(doc))?,
    })
}

/// The document as the `document` digest sees it: everything except `integrity`
/// and `combinedAt`.
///
/// Sealing has to be idempotent. If the digest covered `integrity` then writing
/// the digest into the document would change the digest, and `--verify` would
/// be a check that can never pass. `combinedAt` comes out for causl's reason:
/// two aggregations of the same evidence differ only by a wall clock, and a
/// digest that moved because time passed is a refusal nobody can clear.
pub fn document_evidence(doc: &Value) -> Value {
    match doc {
        Value::Object(map) => {
            let mut out = map.clone();
            out.remove("integrity");
            out.remove("combinedAt");
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// What `--verify` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    /// The digests computed from the document as it stands now.
    pub recomputed: Digests,
    /// The digests the document claims, or `None` when it carries no
    /// `integrity` block at all.
    pub stated: Option<Digests>,
    /// The blocks whose stated digest does not match the recomputed one, in
    /// [`Digests`] declaration order.
    pub moved: Vec<&'static str>,
    /// The blocks that match.
    pub unchanged: Vec<&'static str>,
}

impl VerifyReport {
    /// Whether the document verifies: it stated all four digests and all four
    /// still hold.
    pub fn ok(&self) -> bool {
        self.stated.is_some() && self.moved.is_empty()
    }

    /// One line per finding, for stderr.
    pub fn lines(&self) -> Vec<String> {
        let Some(stated) = &self.stated else {
            return vec![
                "REFUSED: the document carries no integrity block, so there is nothing to \
                 verify it against."
                    .to_string(),
            ];
        };
        if self.moved.is_empty() {
            return vec![format!(
                "verified: all four digests still hold (document {})",
                self.recomputed.document
            )];
        }
        self.moved
            .iter()
            .map(|block| {
                let (stated, recomputed) = match *block {
                    "cells" => (&stated.cells, &self.recomputed.cells),
                    "runners" => (&stated.runners, &self.recomputed.runners),
                    "measurements" => (&stated.measurements, &self.recomputed.measurements),
                    _ => (&stated.document, &self.recomputed.document),
                };
                format!(
                    "REFUSED: the {block} block moved. The document says {stated} and the \
                     content now digests to {recomputed}."
                )
            })
            .collect()
    }
}

/// Recompute all four digests and compare each against what the document
/// claims.
///
/// All four, never stopping at the first mismatch: an edit to one sample moves
/// `cells` and `document` and leaves `runners` and `measurements` alone, and
/// that pattern is the finding. Reporting only the first difference would say
/// "cells moved" and throw away the half of the answer that says the
/// environment did not.
pub fn verify(doc: &Value) -> Result<VerifyReport, CanonicalError> {
    let recomputed = compute_digests(doc)?;
    let stated: Option<Digests> = doc
        .get("integrity")
        .and_then(|i| serde_json::from_value(i.clone()).ok());

    let mut moved = Vec::new();
    let mut unchanged = Vec::new();
    if let Some(stated) = &stated {
        for (name, claimed, actual) in [
            ("cells", &stated.cells, &recomputed.cells),
            ("runners", &stated.runners, &recomputed.runners),
            (
                "measurements",
                &stated.measurements,
                &recomputed.measurements,
            ),
            ("document", &stated.document, &recomputed.document),
        ] {
            if claimed == actual {
                unchanged.push(name);
            } else {
                moved.push(name);
            }
        }
    }

    Ok(VerifyReport {
        recomputed,
        stated,
        moved,
        unchanged,
    })
}
