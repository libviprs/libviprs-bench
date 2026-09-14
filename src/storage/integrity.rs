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
use serde_json::Value;

use crate::sha256::sha256_hex;

/// Largest integer a JavaScript `Number` holds exactly, `2^53 - 1`.
///
/// An integer above it survives the Rust side perfectly and loses its low bits
/// the moment JavaScript parses the document, so the two languages would digest
/// different values while both believing they had read the file correctly.
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

// The two thresholds where `Number.prototype.toString` switches to exponent
// form, `1e-6` and `1e21`, used to live here as `f64` constants. They are gone
// on purpose: nothing on the digest path constructs a float any more, so both
// bounds are checked by counting digits in `canonical_number`, which is exact
// and cannot round.

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
    /// A number `Number.prototype.toString` would print in exponent form,
    /// where Rust prints the digits out in full.
    ///
    /// Carries the token rather than a value, because nothing on this path
    /// builds a float and an error is a poor reason to start.
    OutOfPlainRange { path: String, token: String },
    /// An integer JavaScript cannot hold exactly.
    UnsafeInteger { path: String, value: String },
    /// The same key twice in one object.
    DuplicateKey { path: String, key: String },
    /// A key whose sort position differs between UTF-16 and UTF-8 order.
    NonAsciiKey { path: String, key: String },
    /// The text is not JSON.
    Malformed { detail: String },
}

impl std::fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CanonicalError::NonFinite { path } => write!(
                f,
                "{path} is NaN or an infinity; a digest over it would be a digest over a \
                 measurement that went wrong, written as though it had not"
            ),
            CanonicalError::OutOfPlainRange { path, token } => write!(
                f,
                "{path} is {token}, which JavaScript prints in exponent form and Rust prints \
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
            CanonicalError::DuplicateKey { path, key } => write!(
                f,
                "{path} has the key {key:?} twice; every JSON reader resolves that \
                 differently, so the document would digest to whatever the reader kept"
            ),
            CanonicalError::Malformed { detail } => {
                write!(f, "the document is not JSON: {detail}")
            }
        }
    }
}

impl std::error::Error for CanonicalError {}

// ---------------------------------------------------------------------------
// A JSON reader that never builds a float
//
// `serde_json`'s number *reader* is not correctly rounded. Measured in this
// lane's container:
//
//     witness text          0.09090909090909091     (the cov of [10, 11, 12])
//     std parse             3fb745d1745d1746        prints 0.09090909090909091
//     serde_json parse      3fb745d1745d1747        prints 0.09090909090909093
//     serde_json print(std) 0.09090909090909091
//
// The printer is right and `std` agrees with it, so the reader is the one that
// is wrong, and `parse(print(x)) == x` is false through `serde_json`. A digest
// recomputed from re-parsed floats therefore does not match the digest the
// producer derived, and `--verify` refuses a file that nothing is wrong with.
//
// It is worse across languages. V8's `JSON.parse` *is* correctly rounded, so
// Rust and JavaScript read one archived file as two different floats and derive
// two different digests from a file neither of them wrote incorrectly. K2.2's
// cross-language test would go red with nothing wrong in either implementation,
// and the parser is the last place anybody would look.
//
// So the rule, and it is a rule rather than a workaround: **a digest comes from
// the producer's own bytes and never from a re-serialised parse**. Everything
// below reads JSON into a tree that keeps each number as the text it was written
// as, and canonicalises that text by moving the decimal point and trimming
// zeros. No float is constructed anywhere on the digest path, so there is
// nothing for a reader to round.
//
// The cost is a JSON parser in this file. The alternative was `serde_json`'s
// `arbitrary_precision` feature, which does exactly this but is graph-wide: it
// would turn on for every crate in the build, it has documented interactions
// with `to_value`, and it needed checking against the `preserve_order` K1.2 has
// already enabled. Two hundred lines that only this module depends on is the
// smaller commitment.
// ---------------------------------------------------------------------------

/// A JSON tree in which every number is still the text it was written as.
///
/// Object entries keep their file order; [`RawJson::canonical`] sorts. That is
/// deliberate: the archive file stays readable in the order the producer wrote,
/// and the digest is over the sorted form, so the two never have to agree.
#[derive(Debug, Clone, PartialEq)]
pub enum RawJson {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number, exactly as it appeared in the text.
    Number(String),
    /// A string, with its escapes decoded.
    Str(String),
    /// An array.
    Array(Vec<RawJson>),
    /// An object, in file order.
    Object(Vec<(String, RawJson)>),
}

/// How deep a document may nest before this refuses to follow it.
///
/// A recursive-descent parser on a hostile input is a stack overflow, which is
/// an abort rather than an error. Storage documents are five or six deep.
const MAX_DEPTH: usize = 128;

impl RawJson {
    /// Read JSON text, keeping every number token verbatim.
    pub fn parse(text: &str) -> Result<RawJson, CanonicalError> {
        let mut reader = Reader {
            bytes: text.as_bytes(),
            pos: 0,
            depth: 0,
        };
        reader.skip_whitespace();
        let value = reader.value()?;
        reader.skip_whitespace();
        if reader.pos != reader.bytes.len() {
            return Err(CanonicalError::Malformed {
                detail: format!("trailing input at byte {}", reader.pos),
            });
        }
        Ok(value)
    }

    /// The value at `key`, when this is an object that has one.
    pub fn get(&self, key: &str) -> Option<&RawJson> {
        match self {
            RawJson::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The string, when this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            RawJson::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Drop `key`, if this is an object.
    pub fn remove(&mut self, key: &str) {
        if let RawJson::Object(entries) = self {
            entries.retain(|(k, _)| k != key);
        }
    }

    /// Set `key`, replacing it in place if it is already there so that the file
    /// order does not shuffle when a document is re-sealed.
    pub fn insert(&mut self, key: &str, value: RawJson) {
        if let RawJson::Object(entries) = self {
            if let Some(slot) = entries.iter_mut().find(|(k, _)| k == key) {
                slot.1 = value;
            } else {
                entries.push((key.to_string(), value));
            }
        }
    }

    /// The canonical JSON string for this value, per the rules in this module's
    /// docs.
    pub fn canonical(&self) -> Result<String, CanonicalError> {
        let mut out = String::new();
        self.write_canonical("$", &mut out)?;
        Ok(out)
    }

    fn write_canonical(&self, path: &str, out: &mut String) -> Result<(), CanonicalError> {
        match self {
            RawJson::Null => out.push_str("null"),
            RawJson::Bool(true) => out.push_str("true"),
            RawJson::Bool(false) => out.push_str("false"),
            RawJson::Number(token) => out.push_str(&canonical_number(token, path)?),
            RawJson::Str(s) => out.push_str(&canonical_string(s)),
            RawJson::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write_canonical(&format!("{path}[{i}]"), out)?;
                }
                out.push(']');
            }
            RawJson::Object(entries) => {
                let mut sorted: Vec<&(String, RawJson)> = entries.iter().collect();
                for (key, _) in &sorted {
                    if !key.is_ascii() {
                        return Err(CanonicalError::NonAsciiKey {
                            path: path.to_string(),
                            key: key.clone(),
                        });
                    }
                }
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                // Duplicate keys are legal JSON and every parser resolves them
                // differently, so a document carrying one digests to whatever the
                // reader happened to keep. Refusing is the only answer that is the
                // same in both languages.
                for pair in sorted.windows(2) {
                    if pair[0].0 == pair[1].0 {
                        return Err(CanonicalError::DuplicateKey {
                            path: path.to_string(),
                            key: pair[0].0.clone(),
                        });
                    }
                }
                out.push('{');
                for (i, (key, value)) in sorted.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&canonical_string(key));
                    out.push(':');
                    value.write_canonical(&format!("{path}.{key}"), out)?;
                }
                out.push('}');
            }
        }
        Ok(())
    }

    /// Print this back out as an indented JSON file, numbers verbatim.
    ///
    /// Verbatim is the whole point: the archiver writes a document it has read,
    /// and re-printing a number it never turned into a float is the only way the
    /// bytes it files are the bytes it was given.
    pub fn to_pretty(&self) -> String {
        let mut out = String::new();
        self.write_pretty(0, &mut out);
        out.push('\n');
        out
    }

    fn write_pretty(&self, indent: usize, out: &mut String) {
        let pad = |n: usize| "  ".repeat(n);
        match self {
            RawJson::Null => out.push_str("null"),
            RawJson::Bool(true) => out.push_str("true"),
            RawJson::Bool(false) => out.push_str("false"),
            RawJson::Number(token) => out.push_str(token),
            RawJson::Str(s) => out.push_str(&canonical_string(s)),
            RawJson::Array(items) if items.is_empty() => out.push_str("[]"),
            RawJson::Array(items) => {
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    out.push_str(&pad(indent + 1));
                    item.write_pretty(indent + 1, out);
                    out.push_str(if i + 1 == items.len() { "\n" } else { ",\n" });
                }
                out.push_str(&pad(indent));
                out.push(']');
            }
            RawJson::Object(entries) if entries.is_empty() => out.push_str("{}"),
            RawJson::Object(entries) => {
                out.push_str("{\n");
                for (i, (key, value)) in entries.iter().enumerate() {
                    out.push_str(&pad(indent + 1));
                    out.push_str(&canonical_string(key));
                    out.push_str(": ");
                    value.write_pretty(indent + 1, out);
                    out.push_str(if i + 1 == entries.len() { "\n" } else { ",\n" });
                }
                out.push_str(&pad(indent));
                out.push('}');
            }
        }
    }
}

/// The recursive-descent reader behind [`RawJson::parse`].
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

impl<'a> Reader<'a> {
    fn malformed(&self, detail: impl Into<String>) -> CanonicalError {
        CanonicalError::Malformed {
            detail: format!("{} at byte {}", detail.into(), self.pos),
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(b) = self.bytes.get(self.pos) {
            match b {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn eat(&mut self, literal: &str) -> Result<(), CanonicalError> {
        if self.bytes[self.pos..].starts_with(literal.as_bytes()) {
            self.pos += literal.len();
            Ok(())
        } else {
            Err(self.malformed(format!("expected {literal:?}")))
        }
    }

    fn value(&mut self) -> Result<RawJson, CanonicalError> {
        if self.depth > MAX_DEPTH {
            return Err(self.malformed(format!("nested deeper than {MAX_DEPTH}")));
        }
        match self.bytes.get(self.pos) {
            None => Err(self.malformed("unexpected end of input")),
            Some(b'n') => self.eat("null").map(|()| RawJson::Null),
            Some(b't') => self.eat("true").map(|()| RawJson::Bool(true)),
            Some(b'f') => self.eat("false").map(|()| RawJson::Bool(false)),
            Some(b'"') => self.string().map(RawJson::Str),
            Some(b'[') => self.array(),
            Some(b'{') => self.object(),
            Some(b'-') => self.number(),
            Some(b) if b.is_ascii_digit() => self.number(),
            Some(b) => Err(self.malformed(format!("unexpected byte {:?}", *b as char))),
        }
    }

    fn array(&mut self) -> Result<RawJson, CanonicalError> {
        self.pos += 1;
        self.depth += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.bytes.get(self.pos) == Some(&b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(RawJson::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value()?);
            self.skip_whitespace();
            match self.bytes.get(self.pos) {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(RawJson::Array(items));
                }
                _ => return Err(self.malformed("expected ',' or ']'")),
            }
        }
    }

    fn object(&mut self) -> Result<RawJson, CanonicalError> {
        self.pos += 1;
        self.depth += 1;
        let mut entries = Vec::new();
        self.skip_whitespace();
        if self.bytes.get(self.pos) == Some(&b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(RawJson::Object(entries));
        }
        loop {
            self.skip_whitespace();
            let key = self.string()?;
            self.skip_whitespace();
            if self.bytes.get(self.pos) != Some(&b':') {
                return Err(self.malformed("expected ':'"));
            }
            self.pos += 1;
            self.skip_whitespace();
            let value = self.value()?;
            entries.push((key, value));
            self.skip_whitespace();
            match self.bytes.get(self.pos) {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(RawJson::Object(entries));
                }
                _ => return Err(self.malformed("expected ',' or '}'")),
            }
        }
    }

    fn string(&mut self) -> Result<String, CanonicalError> {
        if self.bytes.get(self.pos) != Some(&b'"') {
            return Err(self.malformed("expected a string"));
        }
        self.pos += 1;
        let mut out = String::new();
        loop {
            let byte = *self
                .bytes
                .get(self.pos)
                .ok_or_else(|| self.malformed("unterminated string"))?;
            match byte {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    let escape = *self
                        .bytes
                        .get(self.pos)
                        .ok_or_else(|| self.malformed("unterminated escape"))?;
                    self.pos += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        other => {
                            return Err(
                                self.malformed(format!("unknown escape \\{}", other as char))
                            );
                        }
                    }
                }
                // A raw control character is not legal JSON, and letting one
                // through would mean the canonical form escapes something the
                // input did not, which is a silent rewrite.
                b if b < 0x20 => return Err(self.malformed("raw control character in a string")),
                _ => {
                    let start = self.pos;
                    while self
                        .bytes
                        .get(self.pos)
                        .is_some_and(|b| *b != b'"' && *b != b'\\' && *b >= 0x20)
                    {
                        self.pos += 1;
                    }
                    out.push_str(
                        std::str::from_utf8(&self.bytes[start..self.pos])
                            .map_err(|e| self.malformed(format!("invalid UTF-8: {e}")))?,
                    );
                }
            }
        }
    }

    /// A `\uXXXX` escape, joining a surrogate pair when it is one.
    fn unicode_escape(&mut self) -> Result<char, CanonicalError> {
        let first = self.hex4()?;
        if (0xD800..0xDC00).contains(&first) {
            if !self.bytes[self.pos..].starts_with(b"\\u") {
                return Err(self.malformed("a high surrogate with no low surrogate after it"));
            }
            self.pos += 2;
            let second = self.hex4()?;
            if !(0xDC00..0xE000).contains(&second) {
                return Err(self.malformed("a high surrogate followed by a non-surrogate"));
            }
            let combined = 0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
            return char::from_u32(combined).ok_or_else(|| self.malformed("bad surrogate pair"));
        }
        char::from_u32(first).ok_or_else(|| self.malformed("a lone low surrogate"))
    }

    fn hex4(&mut self) -> Result<u32, CanonicalError> {
        let slice = self
            .bytes
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| self.malformed("truncated \\u escape"))?;
        let text = std::str::from_utf8(slice).map_err(|_| self.malformed("bad \\u escape"))?;
        let value = u32::from_str_radix(text, 16).map_err(|_| self.malformed("bad \\u escape"))?;
        self.pos += 4;
        Ok(value)
    }

    /// A number token, taken as text and never as a value.
    fn number(&mut self) -> Result<RawJson, CanonicalError> {
        let start = self.pos;
        if self.bytes.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        let int_start = self.pos;
        while self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
            self.pos += 1;
        }
        if self.pos == int_start {
            return Err(self.malformed("a number with no integer part"));
        }
        if self.bytes[int_start] == b'0' && self.pos - int_start > 1 {
            return Err(self.malformed("a number with a leading zero"));
        }
        if self.bytes.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            let frac_start = self.pos;
            while self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
                self.pos += 1;
            }
            if self.pos == frac_start {
                return Err(self.malformed("a number with an empty fraction"));
            }
        }
        if matches!(self.bytes.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.bytes.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let exp_start = self.pos;
            while self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
                self.pos += 1;
            }
            if self.pos == exp_start {
                return Err(self.malformed("a number with an empty exponent"));
            }
        }
        Ok(RawJson::Number(
            std::str::from_utf8(&self.bytes[start..self.pos])
                .expect("digits, sign and exponent marker are all ASCII")
                .to_string(),
        ))
    }
}

/// The canonical spelling of a number token, worked out by moving its decimal
/// point rather than by parsing it.
///
/// Nothing here constructs a float, which is the point. The digits that come out
/// are the digits that went in, so a document digests to the same value however
/// many times it is read and written, and a reader that rounds differently
/// cannot change the answer.
///
/// The rules, in order:
///
/// * the exponent is applied by shifting the decimal point, which is exact;
/// * leading zeros in the integer part and trailing zeros in the fraction go,
///   and a fraction that empties takes its `.` with it, so `1.0` prints as `1`
///   the way `JSON.stringify` does;
/// * every zero, `-0.0` included, prints as `0`;
/// * a magnitude at or above `1e21`, or below `1e-6` and not zero, is refused,
///   because those are the two points where `Number.prototype.toString` switches
///   to exponent form and the two languages stop agreeing;
/// * a token written *as an integer*, with no `.` and no exponent, must fit in
///   `2^53 - 1`. That distinction is only visible in the bytes: a producer
///   writing a `u64` counter emits `9007199254740993`, and one writing the float
///   `1e20` emits `1e20`, and JavaScript holds the second exactly and loses the
///   low bits of the first.
fn canonical_number(token: &str, path: &str) -> Result<String, CanonicalError> {
    let out_of_range = || CanonicalError::OutOfPlainRange {
        path: path.to_string(),
        token: token.to_string(),
    };

    let bytes = token.as_bytes();
    let mut i = 0;
    let negative = bytes.first() == Some(&b'-');
    if negative {
        i += 1;
    }
    let int_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    let int_part = &token[int_start..i];

    let mut frac_part = "";
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        frac_part = &token[start..i];
    }

    let has_exponent = i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E');
    let mut exponent: i64 = 0;
    if has_exponent {
        i += 1;
        let exp_negative = bytes[i] == b'-';
        if exp_negative || bytes[i] == b'+' {
            i += 1;
        }
        // An exponent this large is out of range whatever its digits are, and
        // shifting the point by it would build a string of that many characters.
        let digits = &token[i..];
        if digits.len() > 4 {
            return Err(out_of_range());
        }
        exponent = digits.parse::<i64>().map_err(|_| out_of_range())?;
        if exp_negative {
            exponent = -exponent;
        }
        if !(-400..=400).contains(&exponent) {
            return Err(out_of_range());
        }
    }
    // A token written as a plain integer is the one the 2^53 rule is about.
    let written_as_integer = frac_part.is_empty() && !has_exponent;

    // Shift the point. `digits` is the significand with the point conceptually
    // after `point` characters; both moves below are exact.
    let digits: String = format!("{int_part}{frac_part}");
    let point = int_part.len() as i64 + exponent;
    let (mut integer_digits, mut fraction_digits) = if point <= 0 {
        (
            "0".to_string(),
            format!("{}{}", "0".repeat((-point) as usize), digits),
        )
    } else if point as usize >= digits.len() {
        (
            format!("{digits}{}", "0".repeat(point as usize - digits.len())),
            String::new(),
        )
    } else {
        (
            digits[..point as usize].to_string(),
            digits[point as usize..].to_string(),
        )
    };

    let trimmed = integer_digits.trim_start_matches('0').to_string();
    integer_digits = if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed
    };
    fraction_digits = fraction_digits.trim_end_matches('0').to_string();

    if integer_digits == "0" && fraction_digits.is_empty() {
        // Every zero prints as `0`, sign and all, matching `JSON.stringify(-0)`.
        return Ok("0".to_string());
    }

    if integer_digits != "0" && integer_digits.len() >= 22 {
        // 1e21 is the first value with 22 integer digits.
        return Err(out_of_range());
    }
    if integer_digits == "0" {
        let leading_zeros = fraction_digits.chars().take_while(|c| *c == '0').count();
        if leading_zeros >= 6 {
            // 1e-6 is `0.000001`, five leading zeros; a sixth is below it.
            return Err(out_of_range());
        }
    }
    if written_as_integer && fraction_digits.is_empty() {
        // Compared as digits rather than parsed, so the ceiling and the check
        // share one source of truth without either becoming a number.
        let max_safe = MAX_SAFE_INTEGER.to_string();
        let too_big = integer_digits.len() > max_safe.len()
            || (integer_digits.len() == max_safe.len() && integer_digits > max_safe);
        if too_big {
            return Err(CanonicalError::UnsafeInteger {
                path: path.to_string(),
                value: token.to_string(),
            });
        }
    }

    let sign = if negative { "-" } else { "" };
    Ok(if fraction_digits.is_empty() {
        format!("{sign}{integer_digits}")
    } else {
        format!("{sign}{integer_digits}.{fraction_digits}")
    })
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

/// The canonical JSON string for archived text.
///
/// This is the one canonicaliser. Everything else in this module goes through
/// it, so there is no second implementation to drift.
pub fn canonical_json_from_text(text: &str) -> Result<String, CanonicalError> {
    RawJson::parse(text)?.canonical()
}

/// `sha256:<64 lowercase hex>` over the UTF-8 bytes of the canonical form.
pub fn digest_from_text(text: &str) -> Result<String, CanonicalError> {
    Ok(format!(
        "sha256:{}",
        sha256_hex(canonical_json_from_text(text)?.as_bytes())
    ))
}

/// The canonical JSON string for a value built in memory.
///
/// Correct for a `Value` the caller **constructed**, and not for one it parsed.
/// Serialising and re-canonicalising recovers the producer's own number tokens
/// when the floats came from the producer; when the `Value` came out of
/// `from_str` the damage was done before this was called, and the right entry
/// point is [`canonical_json_from_text`] on the bytes.
pub fn canonical_json(value: &Value) -> Result<String, CanonicalError> {
    canonical_json_from_text(&serde_json::to_string(value).map_err(|err| {
        CanonicalError::Malformed {
            detail: err.to_string(),
        }
    })?)
}

/// `sha256:` over [`canonical_json`]. Same caveat about parsed values.
pub fn digest(value: &Value) -> Result<String, CanonicalError> {
    Ok(format!(
        "sha256:{}",
        sha256_hex(canonical_json(value)?.as_bytes())
    ))
}

/// The four digests a sealed storage document carries.
///
/// Four rather than one so `--verify` can say *what* moved. A single whole-file
/// hash tells a reader that something changed and leaves them to diff 480 lines
/// of numbers to find out what; these four turn that into "the cells moved and
/// the runner did not", which is the difference between a re-measured sweep and
/// a tampered-with one.
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

/// The three block digests, paired with the top-level key each one covers.
///
/// `document` is the odd one out and is handled separately, because it is not a
/// key but the whole thing minus two.
const BLOCKS: [(&str, &str); 3] = [
    ("cells", "cells"),
    ("runners", "runners"),
    ("measurements", "measurement"),
];

/// Compute all four digests from a document's own bytes.
///
/// A block that is absent still gets a digest, over JSON `null`. That is
/// deliberate: a document that lost its `runners` block entirely must not
/// produce the same `runners` digest as one that never had the key, and a
/// missing block is the aggregator's business to refuse rather than this
/// function's business to paper over.
pub fn compute_digests_raw(doc: &RawJson) -> Result<Digests, CanonicalError> {
    let mut computed = Vec::with_capacity(3);
    for (_, key) in BLOCKS {
        let block = doc.get(key).cloned().unwrap_or(RawJson::Null);
        computed.push(format!(
            "sha256:{}",
            sha256_hex(block.canonical()?.as_bytes())
        ));
    }
    let evidence = document_evidence(doc);
    Ok(Digests {
        cells: computed[0].clone(),
        runners: computed[1].clone(),
        measurements: computed[2].clone(),
        document: format!("sha256:{}", sha256_hex(evidence.canonical()?.as_bytes())),
    })
}

/// [`compute_digests_raw`] from text.
pub fn compute_digests_from_text(text: &str) -> Result<Digests, CanonicalError> {
    compute_digests_raw(&RawJson::parse(text)?)
}

/// [`compute_digests_raw`] from a value built in memory. Same caveat as
/// [`canonical_json`] about values that were parsed.
pub fn compute_digests(value: &Value) -> Result<Digests, CanonicalError> {
    compute_digests_from_text(&serde_json::to_string(value).map_err(|err| {
        CanonicalError::Malformed {
            detail: err.to_string(),
        }
    })?)
}

/// The document as the `document` digest sees it: everything except `integrity`
/// and `combinedAt`.
///
/// Sealing has to be idempotent. If the digest covered `integrity` then writing
/// the digest into the document would change the digest, and `--verify` would be
/// a check that can never pass. `combinedAt` comes out for causl's reason: two
/// aggregations of the same evidence differ only by a wall clock, and a digest
/// that moved because time passed is a refusal nobody can clear.
pub fn document_evidence(doc: &RawJson) -> RawJson {
    let mut out = doc.clone();
    out.remove("integrity");
    out.remove("combinedAt");
    out
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
    /// The document has an `integrity` key that is not four digests.
    ///
    /// Kept apart from `stated: None` because the two need different sentences.
    /// A document with no integrity block has not been sealed yet, which is the
    /// ordinary input to `--archive`. A document carrying an integrity block
    /// that will not parse has been sealed by something, and telling its author
    /// there is no block to verify would send them looking for the wrong
    /// problem.
    pub malformed_integrity: bool,
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
            return vec![if self.malformed_integrity {
                format!(
                    "REFUSED: the document carries an integrity block that is not four \
                     digests, so it was sealed by something that does not agree with this \
                     one about what a seal is. The content digests to {}.",
                    self.recomputed.document
                )
            } else {
                "REFUSED: the document carries no integrity block, so there is nothing to \
                 verify it against."
                    .to_string()
            }];
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

/// The digests a document claims, read straight out of its own bytes.
fn stated_digests(doc: &RawJson) -> Option<Digests> {
    let integrity = doc.get("integrity")?;
    let field = |name: &str| {
        integrity
            .get(name)
            .and_then(RawJson::as_str)
            .map(str::to_string)
    };
    Some(Digests {
        cells: field("cells")?,
        runners: field("runners")?,
        measurements: field("measurements")?,
        document: field("document")?,
    })
}

/// Recompute all four digests from the document's own bytes and compare each
/// against what the document claims.
///
/// All four, never stopping at the first mismatch: an edit to one sample moves
/// `cells` and `document` and leaves `runners` and `measurements` alone, and
/// that pattern is the finding. Reporting only the first difference would say
/// "cells moved" and throw away the half of the answer that says the environment
/// did not.
pub fn verify_raw(doc: &RawJson) -> Result<VerifyReport, CanonicalError> {
    let recomputed = compute_digests_raw(doc)?;
    let stated = stated_digests(doc);

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
        malformed_integrity: doc.get("integrity").is_some() && stated.is_none(),
        recomputed,
        stated,
        moved,
        unchanged,
    })
}

/// Verify an archived document from the bytes it is stored as.
///
/// This is the entry point everything that reads a file should use. Parsing the
/// file into floats first and verifying those is the bug this module's header
/// describes, and there is deliberately no function here that will do it.
pub fn verify_text(text: &str) -> Result<VerifyReport, CanonicalError> {
    verify_raw(&RawJson::parse(text)?)
}
