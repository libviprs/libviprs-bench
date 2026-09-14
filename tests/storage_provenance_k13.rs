//! Provenance, the emulation probe, attestation, digests and the archive
//! (libviprs-bench #66).
//!
//! Every test here names, in a comment above it, the wrong implementation it
//! goes red against. That is not decoration: most of these guard against code
//! that looks right and answers the same thing every time, and a test that
//! passes against both the right and the wrong version is worse than no test
//! because it reads like coverage.
//!
//! The one thing that cannot live here is the emulation probe's own control.
//! A probe hard-wired to `false` passes every assertion Rust can make about it
//! in a single process, so the control has to build the probe twice on one
//! machine under two Docker platforms and assert opposite answers. That is
//! `tools/probe-emulation.sh`, and what this file can do about it is make sure
//! the control itself still asserts both directions.

use std::path::PathBuf;

use libviprs_bench::provenance::{FilesystemInfo, Provenance, SourceTrees, ToolchainInfo};
use libviprs_bench::storage::archive::{self, ArchiveError};
use libviprs_bench::storage::attest::{
    EquivalenceSample, ObservedArchive, Regime, RootEntry, attest,
};
use libviprs_bench::storage::integrity::{self, CanonicalError};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A document that is admissible, so that every refusal test can break exactly
/// one thing and know that is what it broke.
///
/// Built by hand rather than captured, because a fixture taken from a real run
/// on this machine would be refused for being emulated and every test below
/// would pass for the wrong reason.
fn clean_document() -> Value {
    json!({
        "schemaVersion": 1,
        "family": "libviprs-storage",
        "runner": "libviprs-storage",
        "startedAt": "2026-09-13T21:45:00.000Z",
        "finishedAt": "2026-09-13T22:05:11.000Z",
        "measurement": {
            "unit": "fresh-process-per-cell",
            "isolation": "subprocess-per-cell",
            "reps": 7,
            "minReps": 5,
            "seed": 1_589_281_650_671i64,
            "clock": "std::time::Instant",
            "tieBandPct": 3,
            "covLowConfidence": 0.15,
            "freshProcessPerCell": true
        },
        "runners": [
            {"name": "libviprs-storage", "version": "0.3.0"}
        ],
        "provenance": {
            "library": {
                "name": "libviprs",
                "version": "0.4.0",
                "commit": "0f1e2d3c4b5a69788796a5b4c3d2e1f0a9b8c7d6",
                "dirty": false,
                "gitNote": "clean read"
            },
            "commit": "c2c3255aa11bb22cc33dd44ee55ff6600112233",
            "dirty": false,
            "gitNote": "clean read",
            "allowDirty": false,
            "emulated": false,
            "emulationEvidence": [
                {
                    "source": "proc-self-maps",
                    "verdict": "native",
                    "detail": "read 23 mappings, none of them a translator"
                },
                {
                    "source": "daemon-arch",
                    "verdict": "native",
                    "detail": "the runner says the daemon runs on aarch64, matching this binary"
                }
            ],
            "filesystem": {
                "scratchDir": "/scratch/storage",
                "fsType": "ext4",
                "mountSource": "/dev/vda1",
                "bindMount": false,
                "declaredTmpfs": false
            },
            "node": {
                "rustc": "rustc 1.98.1 (48a229cea 2026-09-01)",
                "cargo": "cargo 1.98.1",
                "buildProfile": "release",
                "buildFlags": "lto=thin,codegen-units=1",
                "rustflags": "-C target-cpu=native",
                "debugAssertions": false
            },
            "os": "linux",
            "arch": "aarch64",
            "cpuModel": "Neoverse-N1",
            "ncpu": 8,
            "inContainer": true,
            "cgroupCpuQuota": null,
            "cgroupMemoryLimit": null,
            "lockfileHash": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "dependencies": {
                "libviprs": {"version": "0.4.0", "source": null, "checksum": null}
            },
            "invocation": {
                "argv": ["storage", "--profile", "full"],
                "command": "storage",
                "cwd": "/src/libviprs-bench",
                "env": {"RUSTFLAGS": "-C target-cpu=native"},
                "resolved": {
                    "reps": 7,
                    "scenarios": ["generate", "read_random"],
                    "scales": [93, 21851]
                }
            }
        },
        "cells": [
            cell("pmtiles", "read_random", 21851, 7),
            cell("directory", "read_random", 21851, 7)
        ]
    })
}

fn cell(backend: &str, scenario: &str, scale: u64, reps: u64) -> Value {
    json!({
        "backend": backend,
        "scenario": scenario,
        "scale": scale,
        "reps": reps,
        "outcome": "ok",
        "storageAttested": true,
        "dirty": false,
        "regime": "leaf",
        "unit": "us",
        "samples": [1210.0, 1198.5, 1205.25, 1211.0, 1199.75, 1202.5, 1207.0],
        "median": 1205.25,
        // The coefficient of variation of `[10, 11, 12]`, which is `1.0 / 11.0`.
        // It is in the fixture rather than in one test because it is a value this
        // suite really produces and because `serde_json`'s number *reader* does not
        // round it correctly: the printer writes `0.09090909090909091`, which is
        // right, and the reader hands back the float one ULP above it. Every test
        // that goes near a file therefore carries the witness.
        "cov": WITNESS_COV
    })
}

/// `1.0 / 11.0`, the coefficient of variation of `[10, 11, 12]`.
///
/// Written as the division rather than as a literal on purpose: a literal would
/// be parsed by `rustc`, which is correctly rounded, and the point of the value
/// is what happens to it on the way through a JSON file.
const WITNESS_COV: f64 = 1.0 / 11.0;

/// The refusal codes a document produced.
fn codes(doc: &Value) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = archive::admit(doc).into_iter().map(|r| r.code).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Set a nested field, creating nothing: every path here exists in the fixture.
fn set(doc: &mut Value, path: &str, value: Value) {
    let mut cursor = doc;
    let segments: Vec<&str> = path.split('.').collect();
    for segment in &segments[..segments.len() - 1] {
        cursor = cursor
            .get_mut(*segment)
            .unwrap_or_else(|| panic!("{path}: {segment} is not in the fixture"));
    }
    let last = segments[segments.len() - 1];
    match cursor {
        Value::Object(map) => {
            map.insert(last.to_string(), value);
        }
        other => panic!("{path} is not an object, it is {other}"),
    }
}

/// Remove a nested field entirely, which is a different document from setting
/// it to null.
fn remove(doc: &mut Value, path: &str) {
    let mut cursor = doc;
    let segments: Vec<&str> = path.split('.').collect();
    for segment in &segments[..segments.len() - 1] {
        cursor = cursor
            .get_mut(*segment)
            .unwrap_or_else(|| panic!("{path}: {segment} is not in the fixture"));
    }
    match cursor {
        Value::Object(map) => {
            map.remove(segments[segments.len() - 1]);
        }
        other => panic!("{path} is not an object, it is {other}"),
    }
}

/// A directory this test may write into, unique per test so parallel tests do
/// not share one.
///
/// `CARGO_TARGET_TMPDIR` is cargo's own scratch for integration tests, so this
/// never writes into the source tree and never needs to delete anything to
/// clean up after itself.
///
/// The name is made unique because nothing here deletes. Without that, the
/// second run of the suite finds the first run's archive already filed and
/// `run_id_is_derived_from_the_document_not_the_clock` fails its `written: true`
/// assertion for a reason that has nothing to do with what it is testing.
///
/// The process id alone is not enough, and finding that out cost a confusing
/// red. Every run of the suite happens in a fresh container, pids there start
/// from the low numbers and repeat, and the target directory is a volume that
/// outlives the container: eleven runs had left `k13-archive-idempotent-20`,
/// `-101`, `-191`, `-738` and so on behind, and the twelfth drew a pid it had
/// drawn before. The clock is the part that does not repeat, so it is in the
/// name too.
fn scratch(name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("k13-{name}-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("cargo's target tmpdir is writable");
    dir
}

// ---------------------------------------------------------------------------
// The fixture itself
// ---------------------------------------------------------------------------

/// RED against a fixture that is already refused for some unrelated reason,
/// which would make every refusal test below pass whatever the aggregator did.
#[test]
fn the_clean_fixture_is_admitted() {
    assert_eq!(
        archive::admit(&clean_document()),
        vec![],
        "the fixture every other test mutates must itself be admissible"
    );
}

// ---------------------------------------------------------------------------
// Emulation
// ---------------------------------------------------------------------------

/// RED against a rule that only refuses `true`.
///
/// The document this whole lane exists because of is not the emulated one, it
/// is the silent one: `docs/pmtiles-benchmarks.md` records no platform at all,
/// so its emulation field is absent rather than `true`. A gate that refuses
/// only `true` would have admitted it.
#[test]
fn an_emulated_or_unknown_document_is_refused_by_the_aggregator() {
    let mut translated = clean_document();
    set(&mut translated, "provenance.emulated", json!(true));
    assert!(
        codes(&translated).contains(&"emulated"),
        "a translated run must be refused: {:?}",
        archive::admit(&translated)
    );

    let mut unknown = clean_document();
    set(&mut unknown, "provenance.emulated", json!("unknown"));
    assert!(
        codes(&unknown).contains(&"emulated"),
        "a run nobody could observe must be refused: {:?}",
        archive::admit(&unknown)
    );

    let mut silent = clean_document();
    remove(&mut silent, "provenance.emulated");
    assert!(
        codes(&silent).contains(&"emulated"),
        "a document that records no platform at all must be refused, because that is the \
         defect this lane exists for: {:?}",
        archive::admit(&silent)
    );

    let mut null = clean_document();
    set(&mut null, "provenance.emulated", json!(null));
    assert!(
        codes(&null).contains(&"emulated"),
        "an explicit null is not an observation either: {:?}",
        archive::admit(&null)
    );
}

/// RED against a control that only ever runs one platform, or that asserts the
/// same answer on both.
///
/// The real control is the shell run, which cannot happen inside cargo: it
/// needs a Docker daemon and two platforms. What this can do is fail if
/// somebody quietly reduces the control to something a hard-wired probe would
/// pass, which is the shape the erosion would take.
#[test]
fn the_probe_control_asserts_both_directions() {
    let script = std::fs::read_to_string("tools/probe-emulation.sh")
        .expect("the probe control lives at tools/probe-emulation.sh");
    for needle in ["linux/amd64", "linux/arm64", "--platform"] {
        assert!(
            script.contains(needle),
            "the control must still name {needle}"
        );
    }
    assert!(
        script.contains("expected false") && script.contains("expected true"),
        "the control must still assert opposite answers on the two platforms"
    );
    assert!(
        script.contains("native_platform") && script.contains("foreign_platform"),
        "the control must still work out which way round it should be from the daemon's own \
         architecture rather than hard-coding this Mac"
    );
}

// ---------------------------------------------------------------------------
// The source trees, the build, the invocation
// ---------------------------------------------------------------------------

/// RED against an aggregator that accepts.
///
/// Three separate absences, each fatal on its own, because a previous campaign
/// produced a null commit three different ways and a rule that catches one of
/// them catches none of the others.
#[test]
fn a_document_without_commit_dirty_or_invocation_is_refused() {
    for (path, expected) in [
        ("provenance.commit", "commit"),
        ("provenance.library.commit", "commit"),
        ("provenance.dirty", "dirty"),
        ("provenance.library.dirty", "dirty"),
        ("provenance.invocation", "invocation"),
    ] {
        let mut doc = clean_document();
        remove(&mut doc, path);
        assert!(
            codes(&doc).contains(&expected),
            "removing {path} must produce a {expected} refusal, got {:?}",
            archive::admit(&doc)
        );
    }

    // The shape a failed `git rev-parse` leaves behind in a shell pipeline. An
    // empty string is not a commit, and treating it as one is how a document
    // with no provenance gets archived as though it had some.
    let mut empty = clean_document();
    set(&mut empty, "provenance.commit", json!(""));
    assert!(
        codes(&empty).contains(&"commit"),
        "an empty commit is a missing commit: {:?}",
        archive::admit(&empty)
    );
}

/// RED against an aggregator that accepts a dirty tree, and against one that
/// accepts `--allow-dirty` without making the dirt travel with every number.
#[test]
fn a_dirty_tree_is_refused_without_the_flag_and_stamped_with_it() {
    let mut dirty = clean_document();
    set(&mut dirty, "provenance.dirty", json!(true));
    assert!(
        codes(&dirty).contains(&"dirty-not-allowed"),
        "a dirty tree without the flag must be refused: {:?}",
        archive::admit(&dirty)
    );

    // Allowed, but the cells still say `dirty: false`, so a reader quoting one
    // cell would never know.
    let mut allowed_unstamped = dirty.clone();
    set(&mut allowed_unstamped, "provenance.allowDirty", json!(true));
    assert!(
        codes(&allowed_unstamped).contains(&"dirty-not-stamped"),
        "allowing the dirt does not excuse leaving it out of the cells: {:?}",
        archive::admit(&allowed_unstamped)
    );

    // Allowed and stamped on every cell: admissible, and every number carries
    // the caveat with it.
    let mut allowed_stamped = allowed_unstamped.clone();
    for cell in allowed_stamped["cells"]
        .as_array_mut()
        .expect("cells is an array")
    {
        set(cell, "dirty", json!(true));
    }
    assert_eq!(
        archive::admit(&allowed_stamped),
        vec![],
        "an allowed dirty run whose cells all carry the stamp is archivable"
    );
}

/// RED against an aggregator that accepts, and specifically against one that
/// checks only the declared profile string. A release profile with debug
/// assertions forced on is a real configuration and it is not measurable.
#[test]
fn a_debug_build_is_refused() {
    let mut assertions_on = clean_document();
    set(
        &mut assertions_on,
        "provenance.node.debugAssertions",
        json!(true),
    );
    assert!(
        codes(&assertions_on).contains(&"debug-build"),
        "debug assertions are inside the measurement: {:?}",
        archive::admit(&assertions_on)
    );

    let mut debug_profile = clean_document();
    set(
        &mut debug_profile,
        "provenance.node.buildProfile",
        json!("debug"),
    );
    assert!(
        codes(&debug_profile).contains(&"debug-build"),
        "a debug profile is refused: {:?}",
        archive::admit(&debug_profile)
    );

    let mut coverage = clean_document();
    set(
        &mut coverage,
        "provenance.node.rustflags",
        json!("-C target-cpu=native -C instrument-coverage"),
    );
    assert!(
        codes(&coverage).contains(&"perturbing-rustflags"),
        "coverage instrumentation changes the code being timed: {:?}",
        archive::admit(&coverage)
    );
}

/// RED against an aggregator that accepts.
///
/// The failure this catches is a sweep whose header says seven repetitions and
/// whose cells took one, which averages into a series as though it were the
/// same measurement at a seventh of the cost.
#[test]
fn resolved_reps_must_equal_every_cells_reps() {
    let mut mismatched = clean_document();
    set(&mut mismatched["cells"][1], "reps", json!(1));
    let refusals = archive::admit(&mismatched);
    assert!(
        codes(&mismatched).contains(&"reps-disagree"),
        "a cell that took fewer reps than the run claims must be refused: {refusals:?}"
    );
    assert!(
        refusals
            .iter()
            .any(|r| r.detail.contains("directory/read_random@21851")),
        "the refusal must name which cell disagreed: {refusals:?}"
    );

    let mut unresolved = clean_document();
    set(
        &mut unresolved,
        "provenance.invocation.resolved.scenarios",
        json!(["all"]),
    );
    assert!(
        codes(&unresolved).contains(&"scenarios-unresolved"),
        "\"all\" means a different set of scenarios on every day the suite grows: {:?}",
        archive::admit(&unresolved)
    );
}

// ---------------------------------------------------------------------------
// Attestation
// ---------------------------------------------------------------------------

/// RED against an attestation that reads the cell's own label.
///
/// The cell here declares the `root` regime, meaning its whole index fits in
/// one directory. The archive it was measured against has a root full of
/// pointers to leaf directories, which is the other regime entirely and costs a
/// second read per lookup. An attestation built from the label says "attested,
/// root"; an attestation built from the archive says these are not the same
/// thing.
#[test]
fn storage_attestation_is_observed_not_asserted() {
    let spilled = ObservedArchive {
        backend: "pmtiles".to_string(),
        root_entries: vec![RootEntry::LeafPointer; 84],
        leaf_directories: 84,
        tiles: 21_851,
    };
    let sample = EquivalenceSample {
        seed: 0x5EED_1234_ABCD_0001,
        sampled: 64,
        matched: 64,
    };

    let verdict = attest(Regime::Root, &spilled, &sample);
    assert!(
        !verdict.is_attested(),
        "a cell declaring the root regime against an archive whose root points at leaf \
         directories must be refused, and this passed: {verdict:?}"
    );
    assert!(
        verdict
            .reasons()
            .iter()
            .any(|r| r.contains("'root'") && r.contains("'leaf'")),
        "the refusal must name both regimes so a reader can see which way round it is: {:?}",
        verdict.reasons()
    );

    // The same archive with the regime it is actually in attests.
    assert!(
        attest(Regime::Leaf, &spilled, &sample).is_attested(),
        "the same archive attests against the regime it is really in"
    );

    // An archive nobody managed to read is not in the root regime, it is
    // unobserved. RED against `observed_regime` returning `Root` for an empty
    // root directory, which is the natural way to write the fold and produces
    // an attestation from a code path that looked at nothing.
    let unread = ObservedArchive {
        backend: "pmtiles".to_string(),
        root_entries: vec![],
        leaf_directories: 0,
        tiles: 0,
    };
    assert!(
        !attest(Regime::Root, &unread, &sample).is_attested(),
        "an empty root directory is not an observation of the root regime"
    );

    // Bytes, the second observation. RED against an attestation that checks the
    // shape and calls it done.
    let one_mismatch = EquivalenceSample {
        matched: 63,
        ..sample
    };
    assert!(
        !attest(Regime::Leaf, &spilled, &one_mismatch).is_attested(),
        "two backends that disagree about a tile's bytes are not two measurements of one \
         workload"
    );
    let too_few = EquivalenceSample {
        sampled: 8,
        matched: 8,
        ..sample
    };
    assert!(
        !attest(Regime::Leaf, &spilled, &too_few).is_attested(),
        "eight coordinates and sixty-four must not both mean storageAttested: true"
    );
}

/// RED against an aggregator that treats the cell's `storageAttested` as
/// optional, which would let an unobserved cell into the archive wearing an
/// `ok`.
#[test]
fn an_ok_cell_without_attestation_is_refused() {
    let mut doc = clean_document();
    set(&mut doc["cells"][0], "storageAttested", json!(false));
    assert!(
        codes(&doc).contains(&"unattested-cell"),
        "{:?}",
        archive::admit(&doc)
    );

    let mut absent = clean_document();
    remove(&mut absent["cells"][0], "storageAttested");
    assert!(
        codes(&absent).contains(&"unattested-cell"),
        "{:?}",
        archive::admit(&absent)
    );

    // A cell that failed is allowed to be unattested, but it has to say why.
    let mut failed = clean_document();
    set(&mut failed["cells"][0], "outcome", json!("error"));
    set(&mut failed["cells"][0], "storageAttested", json!(false));
    assert!(
        codes(&failed).contains(&"outcome-without-reason"),
        "{:?}",
        archive::admit(&failed)
    );
}

// ---------------------------------------------------------------------------
// Digests and canonicalisation
// ---------------------------------------------------------------------------

/// RED against `serde_json::to_string`, which writes an integral `f64` as `1.0`
/// where `JSON.stringify` writes `1`.
///
/// This is the single character that would have made K2.2's cross-language test
/// fail after both sides had shipped archives, which is why it is pinned now.
#[test]
fn an_integral_float_digests_as_javascript_prints_it() {
    let from_float = json!({"median": 1205.0, "reps": 7});
    let from_integer = json!({"median": 1205, "reps": 7});
    assert_eq!(
        integrity::canonical_json(&from_float).expect("in range"),
        r#"{"median":1205,"reps":7}"#,
        "a whole number prints as an integer whatever Rust type it arrived in"
    );
    assert_eq!(
        integrity::digest(&from_float).expect("in range"),
        integrity::digest(&from_integer).expect("in range"),
        "1205.0 and 1205 are the same number and must digest the same"
    );

    // The other half: a value with a fraction keeps it, and keeps exactly the
    // digits JavaScript would print.
    assert_eq!(
        integrity::canonical_json(&json!({"p": 0.15, "q": 1205.25, "r": -0.0})).expect("in range"),
        r#"{"p":0.15,"q":1205.25,"r":0}"#,
        "negative zero prints as 0, matching JSON.stringify(-0)"
    );
}

/// RED against a canonicaliser that drops null-valued keys, which is what
/// `#[serde(skip_serializing_if = "Option::is_none")]` does to a document and
/// what a reader of causl's `value ?? null` might think the rule is.
#[test]
fn absent_and_null_are_different_documents() {
    let absent = json!({"reps": 7});
    let null = json!({"reps": 7, "throughput": null});
    assert_ne!(
        integrity::canonical_json(&absent).expect("fine"),
        integrity::canonical_json(&null).expect("fine"),
        "an absent key and an explicit null are different documents"
    );
    assert_eq!(
        integrity::canonical_json(&null).expect("fine"),
        r#"{"reps":7,"throughput":null}"#
    );
}

/// RED against a canonicaliser that just prints the number, which for `1e21`
/// gives Rust twenty-two digits and JavaScript `1e+21`.
#[test]
fn a_number_javascript_would_print_in_exponent_form_is_refused() {
    for value in [1e21_f64, -1e21, 1e-7, 5e-9] {
        let err = integrity::canonical_json(&json!({"v": value}))
            .expect_err("{value} is outside the range where the two languages agree");
        assert!(
            matches!(err, CanonicalError::OutOfPlainRange { .. }),
            "{value} produced {err:?}"
        );
    }
    // The two boundaries themselves are inside the plain-notation range and
    // must be accepted, or the rule would be off by one at both ends.
    assert_eq!(
        integrity::canonical_json(&json!({"v": 1e-6})).expect("1e-6 is plain in both"),
        r#"{"v":0.000001}"#
    );
    assert!(integrity::canonical_json(&json!({"v": 1e20})).is_ok());

    // A NaN cannot reach here through serde_json, which refuses to build one,
    // so the guard that matters is the unsafe-integer one: a byte count above
    // 2^53 keeps its low bits in Rust and loses them in JavaScript.
    let err = integrity::canonical_json(&json!({"bytes": 9_007_199_254_740_993u64 }))
        .expect_err("beyond 2^53 the two languages hold different numbers");
    assert!(
        matches!(err, CanonicalError::UnsafeInteger { .. }),
        "{err:?}"
    );
}

/// RED against sorting keys without restricting them to ASCII.
///
/// JavaScript sorts by UTF-16 code unit, so `"\u{10000}"` (the surrogate pair
/// `D800 DC00`) comes *before* `"\u{FFFD}"`. Rust sorts by UTF-8 byte, which is
/// code point order, and puts it after. Two languages, one document, two
/// digests, and nothing anywhere says so.
#[test]
fn a_non_ascii_key_is_refused_because_the_two_sort_orders_disagree() {
    let mut map = serde_json::Map::new();
    map.insert("\u{FFFD}".to_string(), json!(1));
    map.insert("\u{10000}".to_string(), json!(2));
    let err = integrity::canonical_json(&Value::Object(map))
        .expect_err("these two keys sort differently in the two languages");
    assert!(matches!(err, CanonicalError::NonAsciiKey { .. }), "{err:?}");

    // ASCII keys sort identically in both, and are sorted rather than left in
    // insertion order.
    let mut ascii = serde_json::Map::new();
    ascii.insert("zeta".to_string(), json!(1));
    ascii.insert("Alpha".to_string(), json!(2));
    ascii.insert("_under".to_string(), json!(3));
    assert_eq!(
        integrity::canonical_json(&Value::Object(ascii)).expect("ASCII is fine"),
        r#"{"Alpha":2,"_under":3,"zeta":1}"#
    );
}

/// RED against a canonicaliser that resolves a duplicate key instead of
/// refusing it.
///
/// `{"a":1,"a":2}` is legal JSON and readers disagree about it: most keep the
/// last, some keep the first, a few error. A document carrying one digests to
/// whatever the reader happened to keep, which is the same class of bug as the
/// number reader and has the same answer. Refusing is the only verdict that is
/// identical in both languages.
#[test]
fn the_same_key_twice_in_one_object_is_refused() {
    let err = integrity::canonical_json_from_text("{\"a\":1,\"a\":2}")
        .expect_err("a duplicate key has no single canonical form");
    assert!(
        matches!(err, CanonicalError::DuplicateKey { .. }),
        "{err:?}"
    );

    // The ordinary case still canonicalises, and sorts.
    assert_eq!(
        integrity::canonical_json_from_text("{\"b\":2,\"a\":1}").expect("fine"),
        "{\"a\":1,\"b\":2}"
    );
}

/// RED against a reader that does not decode what it read, which would make the
/// canonical form depend on how a producer chose to spell a string.
///
/// Writing a JSON reader by hand is the cost of keeping floats off the digest
/// path, and this is the test that says the reader is a reader rather than a
/// scanner that happens to work on the documents I tried it on.
#[test]
fn the_reader_decodes_escapes_and_ignores_layout() {
    // An escaped character and a literal one are the same string.
    assert_eq!(
        integrity::digest_from_text("{\"k\":\"\\u0041\"}").expect("fine"),
        integrity::digest_from_text("{\"k\":\"A\"}").expect("fine"),
    );
    // Layout between tokens is not content.
    assert_eq!(
        integrity::canonical_json_from_text("{ \"k\" :  [ 1 , 2 ]  }").expect("fine"),
        "{\"k\":[1,2]}"
    );
    // A surrogate pair is one character, not two.
    assert_eq!(
        integrity::canonical_json_from_text("{\"k\":\"\\ud83d\\ude00\"}").expect("fine"),
        "{\"k\":\"\u{1f600}\"}"
    );
    // Control characters come back out escaped the way JSON.stringify writes
    // them: the five short forms, and \u00xx for the rest.
    assert_eq!(
        integrity::canonical_json_from_text("{\"k\":\"\\u0009\\u0000\"}").expect("fine"),
        "{\"k\":\"\\t\\u0000\"}"
    );
    // A raw control character is not legal JSON. Escaping it quietly would mean
    // the canonical form says something the input did not.
    assert!(
        integrity::canonical_json_from_text("{\"k\":\"\u{9}\"}").is_err(),
        "a raw tab inside a string must be refused, not silently escaped"
    );
    // A lone high surrogate cannot be a character and must not become U+FFFD.
    assert!(
        integrity::canonical_json_from_text("{\"k\":\"\\ud83d\"}").is_err(),
        "a lone surrogate must be refused"
    );
}

/// RED against a `--verify` that recomputes one digest, or that stops at the
/// first mismatch.
///
/// Editing a single sample moves `cells` and moves `document`, and leaves
/// `runners` and `measurements` exactly where they were. That pattern is the
/// finding: it says the numbers changed and the environment did not, which is
/// the difference between a re-measured sweep and a tampered-with one.
#[test]
fn verify_recomputes_all_four_digests_and_names_the_block_that_moved() {
    let doc = clean_document();
    let sealed = archive::seal(&doc).expect("the fixture canonicalises");

    let clean = integrity::verify_text(&sealed.text).expect("still canonicalises");
    assert!(clean.ok(), "a freshly sealed document verifies: {clean:?}");
    assert_eq!(clean.unchanged.len(), 4);

    // Edited in the text rather than by parsing, editing and re-serialising,
    // which is both what a person tampering with an archive would do and the
    // only way to change one sample without every other number in the file
    // going through `serde_json`'s reader on the way past.
    let edited = sealed.text.replacen("1211.0", "1211.5", 1);
    assert_ne!(edited, sealed.text, "the edit has to have landed");
    let report = integrity::verify_text(&edited).expect("still canonicalises");

    assert_eq!(
        report.moved,
        vec!["cells", "document"],
        "one edited sample moves the cells block and the document, and nothing else"
    );
    assert_eq!(
        report.unchanged,
        vec!["runners", "measurements"],
        "the environment did not change, and the report has to say so"
    );
    let lines = report.lines();
    assert_eq!(lines.len(), 2, "one line per block that moved: {lines:?}");
    assert!(
        lines.iter().any(|l| l.contains("cells block moved")),
        "{lines:?}"
    );

    // A document that carries digests which no longer hold is refused outright,
    // rather than being quietly re-sealed with the new numbers.
    let refusals = archive::admit_text(&edited).expect("still canonicalises");
    assert!(
        refusals.iter().any(|r| r.code == "integrity"),
        "{refusals:?}"
    );
}

/// RED against sealing that covers `integrity` or `combinedAt`, which would
/// make `--verify` a check that can never pass and a run id that moves every
/// time somebody files the same document.
#[test]
fn sealing_a_document_does_not_change_what_was_sealed() {
    let doc = clean_document();
    let first = archive::seal(&doc).expect("canonicalises");
    let second = archive::seal_text(&first.text).expect("canonicalises");
    assert_eq!(
        first.digests, second.digests,
        "sealing an already-sealed document is the same evidence and must digest the same"
    );
    assert!(
        first.text.contains("\"combinedAt\""),
        "the sealed copy records when it was combined"
    );
    // And the second seal is over the first's bytes, so the witness survived the
    // trip: a re-seal that went through floats would have moved it.
    assert!(
        second.text.contains("0.09090909090909091"),
        "re-sealing must not rewrite a number it read: {}",
        second.text
    );
}

// ---------------------------------------------------------------------------
// The archive
// ---------------------------------------------------------------------------

/// RED against `SystemTime::now()` in the id.
///
/// Archiving the same document twice must file it once. With a clock in the id
/// the second call writes a second entry, so a re-run of the archiver silently
/// doubles a series and the page draws one measurement as two points.
#[test]
fn run_id_is_derived_from_the_document_not_the_clock() {
    let doc = clean_document();
    let root = scratch("archive-idempotent");

    let first = archive::archive(&doc, &root).expect("the fixture is admissible");
    let second = archive::archive(&doc, &root).expect("the fixture is still admissible");

    assert_eq!(
        first.run_id, second.run_id,
        "the same document must derive the same id both times"
    );
    assert!(first.written, "the first call files it");
    assert!(
        !second.written,
        "the second call finds it already filed and changes nothing"
    );

    let index: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(archive::INDEX_FILE)).expect("the index was written"),
    )
    .expect("the index is JSON");
    assert_eq!(
        index.as_array().expect("the index is an array").len(),
        1,
        "one document, one row, however many times it is archived"
    );

    // And the id is built out of the document's own evidence, so a reader can
    // check it by eye rather than trusting it.
    assert!(
        first.run_id.starts_with("20260913T214500Z-0f1e2d3c4b5a"),
        "the id carries the run's own startedAt and the library commit: {}",
        first.run_id
    );

    // A different environment is a different bucket even at the same instant on
    // the same commit, or two incomparable runs would collide.
    let mut elsewhere = doc.clone();
    set(&mut elsewhere, "provenance.cpuModel", json!("Apple M2 Pro"));
    let other_id = archive::run_id(&elsewhere).expect("still derivable");
    assert_ne!(
        first.run_id, other_id,
        "a different cpu model is a different environment bucket"
    );
}

/// RED against a digest path that re-parses the file into floats and
/// canonicalises those, which is what `--verify` did until this test existed.
///
/// `serde_json`'s number reader is not correctly rounded. Measured in this
/// lane's container on the witness the fixture carries:
///
/// ```text
/// witness text          0.09090909090909091
/// std parse             3fb745d1745d1746   prints 0.09090909090909091
/// serde_json parse      3fb745d1745d1747   prints 0.09090909090909093
/// serde_json print(std) 0.09090909090909091
/// ```
///
/// So the printer and `std` agree and the reader is the one that is wrong, and a
/// document whose bytes are exactly what the producer emitted comes back a ULP
/// away. A digest recomputed from those floats does not match the one the
/// producer derived, and the archive refuses a file that nothing is wrong with.
///
/// The direction that matters more is the one this lane cannot see: V8's
/// `JSON.parse` *is* correctly rounded, so Rust and JavaScript read one archived
/// file as two different floats and derive two different digests. K2.2's
/// cross-language test would go red with neither implementation at fault.
///
/// The fix is that a digest is taken from the producer's own bytes and never
/// from a re-serialised parse, so nothing on the digest path builds a float at
/// all.
#[test]
fn a_document_written_and_read_back_verifies_against_its_own_bytes() {
    let root = scratch("byte-roundtrip");
    let entry = archive::archive(&clean_document(), &root).expect("the fixture is admissible");
    let text = std::fs::read_to_string(&entry.path).expect("the archive was written");

    assert!(
        text.contains("0.09090909090909091"),
        "the witness must survive into the file as the digits the printer chose, so that \
         reading it back is the thing under test: {text}"
    );

    let report = integrity::verify_text(&text).expect("the archived text canonicalises");
    assert!(
        report.ok(),
        "a document that is byte for byte what the producer wrote must verify, and this one \
         did not: {:?}",
        report.lines()
    );

    // And the digest the file claims is the one the producer derived, rather than
    // one that happens to agree with a lossy re-read of itself.
    assert_eq!(
        report
            .stated
            .as_ref()
            .expect("a sealed document states its digests")
            .document,
        entry.document_digest,
        "the archived digest must be the one archive() returned"
    );
}

/// RED against any path that re-reads a number it wrote.
///
/// `parse(print(x)) == x` is the assumption the bug above is made of, and it is
/// false here, so it is worth one test that says so in one line rather than
/// leaving it as folklore in a comment.
#[test]
fn printing_a_float_and_reading_it_back_through_serde_json_is_not_the_identity() {
    let printed = serde_json::to_string(&WITNESS_COV).expect("finite");
    let reread: f64 = serde_json::from_str(&printed).expect("valid JSON");
    assert_eq!(
        printed, "0.09090909090909091",
        "the printer is not the problem and this pins that"
    );
    assert_ne!(
        reread.to_bits(),
        WITNESS_COV.to_bits(),
        "if this ever passes, serde_json's reader has been fixed upstream; the digest path \
         still must not depend on it, but this test's premise is gone and it should be \
         retired rather than relaxed"
    );
    // `std`, by contrast, is correctly rounded, which is how the fixture's own
    // expectation above is anchored to something trustworthy.
    assert_eq!(
        printed.parse::<f64>().expect("valid float").to_bits(),
        WITNESS_COV.to_bits()
    );
}

/// RED against an archive that files a refused document anyway, and against one
/// that reports the first reason and stops.
#[test]
fn the_archive_refuses_rather_than_reports() {
    let mut broken = clean_document();
    set(&mut broken, "provenance.emulated", json!(true));
    set(&mut broken, "provenance.node.debugAssertions", json!(true));
    remove(&mut broken, "provenance.commit");

    let root = scratch("archive-refusal");
    let err = archive::archive(&broken, &root).expect_err("three things are wrong with it");
    let ArchiveError::Refused(refusals) = err else {
        panic!("expected a refusal, got {err}");
    };
    let found: Vec<&str> = refusals.iter().map(|r| r.code).collect();
    for expected in ["emulated", "debug-build", "commit"] {
        assert!(
            found.contains(&expected),
            "every reason, not the first: {found:?}"
        );
    }
    assert!(
        std::fs::read_dir(&root)
            .map(|mut d| d.next().is_none())
            .unwrap_or(true),
        "a refused document leaves nothing behind"
    );
}

// ---------------------------------------------------------------------------
// The filesystem
// ---------------------------------------------------------------------------

/// RED against a provenance that does not record the scratch filesystem, and
/// against an aggregator that accepts tmpfs silently.
///
/// tmpfs is RAM with a filesystem interface. A PMTiles read on it measures a
/// memcpy and a directory-tree read on it measures a memcpy through several
/// hundred thousand inodes, and the ratio between those two is not the ratio
/// anybody will see on a disk.
#[test]
fn the_scratch_filesystem_is_recorded_and_tmpfs_is_refused_by_default() {
    let dir = scratch("filesystem");
    let observed = FilesystemInfo::of(&dir);
    assert!(
        !observed.fs_type.is_empty() && observed.fs_type != "unknown",
        "the filesystem under {} was not identified, it read as {:?}",
        dir.display(),
        observed.fs_type
    );
    assert!(
        observed.scratch_dir.contains("k13-filesystem-"),
        "the recorded directory must be the one that was asked about: {}",
        observed.scratch_dir
    );
    assert!(
        !observed.declared_tmpfs,
        "nothing is declared until the driver declares it"
    );

    let mut on_tmpfs = clean_document();
    set(
        &mut on_tmpfs,
        "provenance.filesystem.fsType",
        json!("tmpfs"),
    );
    assert!(
        codes(&on_tmpfs).contains(&"tmpfs"),
        "{:?}",
        archive::admit(&on_tmpfs)
    );

    let mut declared = on_tmpfs.clone();
    set(
        &mut declared,
        "provenance.filesystem.declaredTmpfs",
        json!(true),
    );
    assert_eq!(
        archive::admit(&declared),
        vec![],
        "a deliberate RAM-bound cell is legitimate once it is declared"
    );

    let mut unrecorded = clean_document();
    remove(&mut unrecorded, "provenance.filesystem.fsType");
    assert!(
        codes(&unrecorded).contains(&"filesystem"),
        "a document that does not say what it wrote onto is refused: {:?}",
        archive::admit(&unrecorded)
    );
}

// ---------------------------------------------------------------------------
// What the build stamped
// ---------------------------------------------------------------------------

/// RED against a build script that reports a missing commit as a clean read,
/// which is how a null commit becomes invisible.
///
/// This test runs in whatever environment the gate is in, so it cannot assert
/// that the commit is present. What it can assert is that the note and the
/// fields agree: a tree with no commit has to say why, and a tree with one has
/// to have been read cleanly.
#[test]
fn the_build_stamps_name_both_trees_and_say_why_when_they_cannot() {
    let trees = SourceTrees::stamped();
    for (label, tree) in [("harness", &trees.harness), ("library", &trees.library)] {
        assert!(
            !tree.note.is_empty(),
            "the {label} tree must carry a note even when everything went well"
        );
        if tree.commit.is_none() || tree.dirty.is_none() {
            assert_ne!(
                tree.note, "clean read",
                "the {label} tree has no commit or no dirty flag and claims a clean read, \
                 which is the exact shape of the bug: {tree:?}"
            );
        }
    }
}

/// RED against a provenance whose toolchain block is a placeholder, and against
/// a `debug_assertions` field that is declared rather than observed.
#[test]
fn the_toolchain_block_is_read_from_the_build_not_declared() {
    let toolchain = ToolchainInfo::stamped();
    assert!(
        toolchain.cargo_version.starts_with("cargo "),
        "cargo's own version is stamped at build time, got {:?}",
        toolchain.cargo_version
    );
    assert_eq!(
        toolchain.debug_assertions,
        cfg!(debug_assertions),
        "debug_assertions is compiled in, never declared"
    );
}

/// RED against a dependency graph that is empty, or that does not carry the
/// library the document says it measured.
///
/// `provenance.library` names a version; the graph has to agree with it, or the
/// document is naming a library it did not link.
#[test]
fn the_dependency_graph_names_the_measured_library() {
    let dir = scratch("dependencies");
    let provenance = Provenance::capture_for_storage(&dir);
    assert!(
        provenance.dependencies.len() > 10,
        "the resolved graph should have the whole tree in it, it has {}",
        provenance.dependencies.len()
    );
    let libviprs = provenance
        .dependencies
        .get("libviprs")
        .expect("the measured library must be in the graph it was built from");
    assert_eq!(
        libviprs.source, None,
        "libviprs is a path dependency, and a path dependency has no registry source"
    );
    assert!(
        provenance
            .lockfile_hash
            .as_deref()
            .unwrap_or("")
            .starts_with("sha256:"),
        "the lockfile digest is stamped at build time, got {:?}",
        provenance.lockfile_hash
    );

    // The block the aggregator actually reads has to carry all of it.
    let block = provenance.to_storage_block(&json!({"argv": []}), false);
    assert!(block["dependencies"]["libviprs"].is_object());
    assert!(block["node"]["debugAssertions"].is_boolean());
    assert!(block["filesystem"]["fsType"].is_string());
}
