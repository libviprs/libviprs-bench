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

use libviprs_bench::provenance::{
    FilesystemInfo, MainRelation, Provenance, SourceTrees, ToolchainInfo,
};
use libviprs_bench::storage::archive::{self, ArchiveError};
use libviprs_bench::storage::artefact_digest;
use libviprs_bench::storage::attest::{
    EquivalenceSample, ObservedArchive, Regime, RootEntry, attest,
};
use libviprs_bench::storage::cells::{Backend, Cell, Profile, Source};
use libviprs_bench::storage::document::{
    CellLabels, CellReport, Document, DocumentCell, InvariantBlock, MachineLoad,
};
use libviprs_bench::storage::integrity::{self, CanonicalError};
use libviprs_bench::storage::scenarios::{Direction, Isolation, MetricSpec, Outcome, Unit, Warmup};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A document that is admissible, built from the type the producer writes.
///
/// It used to be hand-written JSON, and that is how the `runners` digest went
/// unnoticed for a whole lane. The fixture carried a `runners` array because
/// `BLOCKS` looked for one; the product emits `runner`, singular, and never had
/// the key at all. So `compute_digests` hashed `Value::Null` on every real
/// document while the tests hashed a real block, and
/// `the_four_digests_say_which_block_moved` passed for a reason it did not mean.
/// The fixture also had `"reps": 7` where `Document` emits
/// `{"generate": 7, "read": 20}`, so the whole admit suite was green against a
/// shape that does not exist.
///
/// Now the shape comes from `Document::to_json()` and only the values are the
/// test's. A field that moves in `document.rs` moves here, and a key the
/// aggregator looks for that the producer does not write is a failure rather
/// than a coincidence.
fn clean_document() -> Value {
    let mut doc = Document::new(Profile::Ci, "2026-09-13T21:45:00.000Z".to_string());
    doc.finished_at = Some("2026-09-13T22:05:11.000Z".to_string());
    doc.push(fixture_cell("pmtiles", 21851));
    doc.push(fixture_cell("directory", 21851));
    doc.rebuild_invariant_table();
    doc.provenance = Some(fixture_provenance(&doc));
    let text = doc.to_json();
    serde_json::from_str(&text).expect("the document the producer writes is JSON")
}

/// One admissible cell, with the two fields the aggregator reads set the way an
/// observed cell has them.
fn fixture_cell(backend: &str, scale: u64) -> DocumentCell {
    let mut cell = sample_cell(backend, scale);
    // Attested because the fixture stands for a cell that WAS observed. Nothing
    // in the product sets this from a label; `run_sweep` sets it from
    // `attest_artefacts`, and the tests that matter drive that end to end.
    cell.attested = Some(true);
    cell.dirty = Some(false);
    cell
}

/// The provenance block, shaped as `Provenance::to_document_block` shapes it, so
/// the refusal tests below poke at the same keys the producer writes.
fn fixture_provenance(doc: &Document) -> Value {
    json!({
        "library": {
            "name": "libviprs",
            "version": "0.4.0",
            "commit": "0f1e2d3c4b5a69788796a5b4c3d2e1f0a9b8c7d6",
            "dirty": false,
            "gitNote": "clean read",
            "mainCommit": "0f1e2d3c4b5a69788796a5b4c3d2e1f0a9b8c7d6",
            "mainRelation": "at-main",
            "commitsBehindMain": 0
        },
        "commit": "c2c3255aa11bb22cc33dd44ee55ff6600112233",
        "dirty": false,
        "gitNote": "clean read",
        "allowDirty": false,
        "emulated": false,
        "emulationEvidence": [
            {"source": "proc-self-maps", "verdict": "native",
             "detail": "read 23 mappings, none of them a translator"}
        ],
        "filesystem": {
            "scratchDir": "/scratch/storage", "fsType": "ext4",
            "mountSource": "/dev/vda1", "bindMount": false, "declaredTmpfs": false
        },
        "node": {
            "rustc": "rustc 1.98.1", "cargo": "cargo 1.98.1",
            "buildProfile": "release", "buildFlags": "lto=thin",
            "rustflags": "", "debugAssertions": false
        },
        "os": "linux", "arch": "aarch64", "cpuModel": "Neoverse-N1", "ncpu": 8,
        "inContainer": true, "cgroupCpuQuota": null, "cgroupMemoryLimit": null,
        "loadAverage": null, "thermalThrottleCount": null,
        "lockfileHash": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "dependencies": {"libviprs": {"version": "0.4.0", "source": null, "checksum": null}},
        "invocation": {
            "argv": ["storage", "--profile", "ci"],
            "command": "storage",
            "cwd": "/src/libviprs-bench",
            "env": {"RUSTFLAGS": null},
            "resolved": {
                "profile": "ci",
                "reps": doc.measurement.reps,
                "scenarios": ["generate", "read_random"],
                "scales": [93, 21851]
            }
        }
    })
}

/// One cell of the fixture, built through the same `from_report` path the
/// producer uses, so its keys are the product's keys.
///
/// The witness value rides in the samples rather than in a hand-set `cov`: the
/// coefficient of variation of `[10, 11, 12]` is `1.0 / 11.0`, so asking the
/// real statistics for it is both more honest and harder to get wrong than
/// writing the float in.
fn sample_cell(backend: &str, scale: u64) -> DocumentCell {
    let samples = vec![10.0, 11.0, 12.0];
    let reps = samples.len() as u32;
    DocumentCell::from_report(CellReport {
        labels: CellLabels::storage(
            match backend {
                "pmtiles" => Backend::PmTiles,
                _ => Backend::Directory,
            },
            Cell::new(2048, 2048, 256, Source::Gradient, scale as u32),
        ),
        scenario: "read_random",
        metric: MetricSpec {
            name: "p50",
            unit: Unit::Microseconds,
            direction: Direction::LowerIsBetter,
        },
        isolation: Isolation::ProcessPerScenario,
        oversubscribed: None,
        warmup: Some(Warmup::ONE_DISCARDED_PASS),
        discarded_warmup: vec![41.0],
        reps_declared: reps,
        min_reps: reps,
        samples,
        outcome: Outcome::Ok,
        reason: None,
        invariants: InvariantBlock::default(),
        machine_load: MachineLoad::unknown(),
        timer: None,
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
        attest(Regime::Leaves, &spilled, &sample).is_attested(),
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
        !attest(Regime::Leaves, &spilled, &one_mismatch).is_attested(),
        "two backends that disagree about a tile's bytes are not two measurements of one \
         workload"
    );
    let too_few = EquivalenceSample {
        sampled: 8,
        matched: 8,
        ..sample
    };
    assert!(
        !attest(Regime::Leaves, &spilled, &too_few).is_attested(),
        "eight coordinates and sixty-four must not both mean attested: true"
    );
}

/// RED against an aggregator that treats the cell's `attested` as
/// optional, which would let an unobserved cell into the archive wearing an
/// `ok`.
#[test]
fn an_ok_cell_without_attestation_is_refused() {
    let mut doc = clean_document();
    set(&mut doc["cells"][0], "attested", json!(false));
    assert!(
        codes(&doc).contains(&"unattested-cell"),
        "{:?}",
        archive::admit(&doc)
    );

    let mut absent = clean_document();
    remove(&mut absent["cells"][0], "attested");
    assert!(
        codes(&absent).contains(&"unattested-cell"),
        "{:?}",
        archive::admit(&absent)
    );

    // A cell that failed is allowed to be unattested, but it has to say why.
    let mut failed = clean_document();
    set(&mut failed["cells"][0], "outcome", json!("error"));
    set(&mut failed["cells"][0], "attested", json!(false));
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

/// RED against a canonicaliser that digests a string as it was spelled rather
/// than as what it means.
///
/// `"\u0041"` and `"A"` are the same string and must digest the same, layout
/// between tokens is not content, and a surrogate pair is one character. These
/// were tests of a hand-written reader; they are kept because the property is
/// about the canonical form rather than about who parsed it, and because
/// `JSON.stringify` on the other side of K2.2's test makes exactly these choices.
#[test]
fn escapes_and_layout_do_not_change_a_digest() {
    assert_eq!(
        integrity::digest_from_text("{\"k\":\"\\u0041\"}").expect("fine"),
        integrity::digest_from_text("{\"k\":\"A\"}").expect("fine"),
    );
    assert_eq!(
        integrity::canonical_json_from_text("{ \"k\" :  [ 1 , 2 ]  }").expect("fine"),
        "{\"k\":[1,2]}"
    );
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
    // Text that is not JSON at all is a refusal with a reason, not a panic and
    // not an empty digest.
    let err = integrity::canonical_json_from_text("{\"k\": }").expect_err("that is not JSON");
    assert!(matches!(err, CanonicalError::Malformed { .. }), "{err:?}");
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
    let edited = sealed.text.replacen("12.0", "12.5", 1);
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
/// The fix is serde_json's `float_roundtrip`, which makes the reader correctly
/// rounded so that parsing and re-printing is exact.
/// `the_json_reader_is_correctly_rounded` is the canary for the feature being
/// on, and this test is what notices if the round trip stops being exact for any
/// other reason.
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

/// RED against `float_roundtrip` being off, which is the single line the whole
/// digest path now rests on.
///
/// `serde_json`'s number reader is not correctly rounded by default. Measured in
/// this lane's container with the feature OFF, on the witness the fixture
/// carries:
///
/// ```text
/// witness text          0.09090909090909091
/// std parse             3fb745d1745d1746   prints 0.09090909090909091
/// serde_json parse      3fb745d1745d1747   prints 0.09090909090909093
/// serde_json print(std) 0.09090909090909091
/// ```
///
/// The printer is right and `std` agrees with it, so the reader is the one that
/// is wrong, and `parse(print(x)) == x` is false. A digest recomputed from a
/// document read back off disk then does not match the one its producer derived,
/// and `--verify` refuses a file that nothing is wrong with. Worse across
/// languages: V8's `JSON.parse` *is* correctly rounded, so Rust and JavaScript
/// would read one archived file as two different floats.
///
/// `Cargo.toml` turns the feature on. This is what notices if anyone turns it
/// off, and it fails in one line rather than as a mysterious digest mismatch
/// somewhere downstream.
#[test]
fn the_json_reader_is_correctly_rounded() {
    let printed = serde_json::to_string(&WITNESS_COV).expect("finite");
    assert_eq!(
        printed, "0.09090909090909091",
        "the printer was never the problem, and this pins that"
    );

    let reread: f64 = serde_json::from_str(&printed).expect("valid JSON");
    assert_eq!(
        reread.to_bits(),
        WITNESS_COV.to_bits(),
        "serde_json read back a different float from the one it printed, which means \
         float_roundtrip is not enabled and every digest taken over a file is unsound"
    );

    // `std` is correctly rounded whatever features are on, so it is the fixed
    // point the assertion above is anchored to rather than another moving part.
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

/// RED against anything that changes what `artefact_digest` produces.
///
/// K1.4 owns the function; this pins its output, and it exists because of what
/// rests on it. K1.4 took the composed tree to the NAS and ran it natively on
/// both architectures: all 14 invariant entries agree exactly across arm64 and
/// x86_64, `artefact_digest` included, while throughput moves 2x to 5x. That is
/// the epic's claim demonstrated rather than asserted, and it is a property of
/// the *value*, not just of the two runs agreeing with each other on a given
/// day.
///
/// Cross-architecture agreement cannot catch a change that moves the digest on
/// both architectures at once, which is exactly what collapsing two SHA-256
/// wrappers into one could have done. These five values were captured from the
/// merged tree before that collapse and asserted after it; they were identical,
/// and they are written down here so the next change to the hashing path has to
/// answer for them rather than rediscover the question.
///
/// The sizes are chosen to reach the code: empty, three bytes, 100000 bytes,
/// and one file of 3 MB that crosses the 1 MiB buffer `artefact_digest` reads
/// with, so the streaming path is exercised across chunk boundaries. The tree
/// digest is over the sorted `(relative path, sha256)` list, which is what makes
/// it depend on the bytes and the layout rather than on directory iteration
/// order.
#[test]
fn artefact_digest_is_pinned_to_the_values_the_two_architectures_agreed_on() {
    let root = scratch("artefact-digest");
    std::fs::create_dir_all(root.join("z/inner")).expect("writable");
    std::fs::write(root.join("a.bin"), b"").expect("writable");
    std::fs::write(root.join("b.bin"), b"abc").expect("writable");
    std::fs::write(root.join("z/c.bin"), vec![b'a'; 100_000]).expect("writable");
    let big: Vec<u8> = (0..=255u8).cycle().take(3_000_000).collect();
    std::fs::write(root.join("z/inner/d.bin"), &big).expect("writable");

    for (name, expected) in [
        (
            "a.bin",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            "b.bin",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            "z/c.bin",
            "6d1cf22d7cc09b085dfc25ee1a1f3ae0265804c607bc2074ad253bcc82fd81ee",
        ),
        (
            "z/inner/d.bin",
            "1913233a0a87fe912497ee543021c40adc5d414614fc76fdff3e0c08b6a1d981",
        ),
    ] {
        assert_eq!(
            artefact_digest(&root.join(name)).as_deref(),
            Some(expected),
            "the file digest for {name} moved"
        );
    }

    assert_eq!(
        artefact_digest(&root).as_deref(),
        Some("49ae1d007e6f05625180663add937b87d73b3f72505187d102c0ef3f1b0a84bb"),
        "the tree digest moved, which would invalidate the cross-architecture \
         agreement rather than merely disagree with it"
    );

    // The file digests are also the canonical SHA-256 of their contents, which
    // is worth asserting once: it says `artefact_digest` on a file is the plain
    // hash and not a hash of something wrapped, so anyone can reproduce one with
    // `sha256sum` and no knowledge of this crate.
    assert_eq!(
        artefact_digest(&root.join("b.bin")).as_deref(),
        Some(libviprs_bench::sha256::sha256_hex(b"abc").as_str())
    );
}

/// RED against an archive write that is not atomic, and against one that leaves
/// its temporary behind when the rename fails.
///
/// A plain `write` can be interrupted, and the failure mode hides itself: the
/// truncated file stays at the archive path, `parse_document` fails on it
/// forever, and the operator is told "the document is not JSON" about a document
/// that was whole when they handed it over. Writing a complete temporary and
/// renaming it means a reader sees the old file or the new one and never half of
/// either.
///
/// Interruption cannot be staged from a test, so what is pinned is the property
/// that survives one: a failed rename leaves nothing behind. A non-empty
/// directory at the destination is a rename target the kernel refuses, which is
/// the cheapest real failure to arrange.
#[test]
fn an_interrupted_write_leaves_no_half_document() {
    let root = scratch("atomic-write");
    let blocked = root.join("blocked.json");
    std::fs::create_dir_all(blocked.join("occupied")).expect("writable");

    let failed = archive::write_atomically(&blocked, "{\"whole\": true}");
    assert!(
        failed.is_err(),
        "renaming onto a non-empty directory has to fail, or this test proves nothing"
    );

    let strays: Vec<String> = std::fs::read_dir(&root)
        .expect("readable")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(".tmp-"))
        .collect();
    assert!(
        strays.is_empty(),
        "a failed write left its temporary beside the archive: {strays:?}"
    );

    // And the ordinary path still writes exactly what it was given.
    let good = root.join("good.json");
    archive::write_atomically(&good, "{\"whole\": true}").expect("an ordinary write");
    assert_eq!(
        std::fs::read_to_string(&good).expect("readable"),
        "{\"whole\": true}"
    );
}

/// RED against a core that is not on its own `main` passing silently, and
/// equally against it being refused.
///
/// Benchmarking a core that is not on `main` is a legitimate thing to want: an
/// intentional baseline looks exactly like this. Provenance's job is to make a
/// run's conditions legible rather than to narrow what may be measured, so this
/// is a warning and the run still archives. What was wrong before is only that
/// nothing said anything.
///
/// Two conditions and not one, which is worth a test of its own because the
/// obvious predicate catches only the second. The core checkout on this machine
/// sits on `m826` at `576910df` while `origin/main` is `809ee801`, and I
/// measured it rather than assuming: `merge-base --is-ancestor` says HEAD **is**
/// an ancestor of main, 0 ahead and 458 behind. So a check written as "warn when
/// the commit is not an ancestor of origin/main" would have stayed silent on the
/// exact tree it was written for. `Behind` and `Diverged` are separate answers
/// and both warn.
#[test]
fn a_core_that_is_not_at_main_warns_and_is_still_archivable() {
    const HEAD: &str = "576910dff04567274ed88b4c199a45bb04acc8d2";
    const MAIN: &str = "809ee8014d002518ce55edaceba698ca7a8b8a79";

    let warnings_for = |relation: MainRelation, behind: Option<u32>| {
        let mut provenance = Provenance::capture();
        provenance.trees.library.commit = Some(HEAD.to_string());
        provenance.trees.library.dirty = Some(false);
        provenance.trees.library.main_commit = Some(MAIN.to_string());
        provenance.trees.library.main_relation = relation;
        provenance.trees.library.commits_behind_main = behind;
        (
            provenance.document_provenance_warnings(),
            provenance.to_document_block(&json!({"argv": []}), false),
        )
    };

    // Behind: the shape the core is actually in right now.
    let (behind, block) = warnings_for(MainRelation::Behind, Some(458));
    let line = behind
        .iter()
        .find(|w| w.contains("origin/main"))
        .unwrap_or_else(|| panic!("a core that is behind main must say so: {behind:?}"));
    assert!(
        line.contains(HEAD),
        "the warning must name the commit: {line}"
    );
    assert!(line.contains(MAIN), "and the main it is not on: {line}");
    assert!(line.contains("458"), "and how far: {line}");
    assert_eq!(block["library"]["mainRelation"], json!("behind"));
    assert_eq!(block["library"]["commitsBehindMain"], json!(458));

    // Diverged: carries work that is not in main at all.
    let (diverged, block) = warnings_for(MainRelation::Diverged, None);
    assert!(
        diverged
            .iter()
            .any(|w| w.contains("diverged") && w.contains(MAIN)),
        "{diverged:?}"
    );
    assert_eq!(block["library"]["mainRelation"], json!("diverged"));

    // At main: silence. A warning that fires on a clean tree is one nobody reads.
    let (at_main, _) = warnings_for(MainRelation::AtMain, Some(0));
    assert!(
        !at_main.iter().any(|w| w.contains("origin/main")),
        "a core at main must not be warned about: {at_main:?}"
    );

    // Unknown: says it could not tell rather than implying the tree is fine.
    let (unknown, _) = warnings_for(MainRelation::Unknown, None);
    assert!(
        unknown.iter().any(|w| w.contains("no local origin/main")),
        "an unanswerable question is not a clean answer: {unknown:?}"
    );

    // And none of it is a refusal: a document from a parked core still archives.
    let mut doc = clean_document();
    set(&mut doc, "provenance.library.mainRelation", json!("behind"));
    set(&mut doc, "provenance.library.commitsBehindMain", json!(458));
    assert_eq!(
        archive::admit(&doc),
        vec![],
        "a parked core is a warning and never a refusal"
    );
}

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
    let provenance = Provenance::capture_for_document(&dir);
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
    let block = provenance.to_document_block(&json!({"argv": []}), false);
    assert!(block["dependencies"]["libviprs"].is_object());
    assert!(block["node"]["debugAssertions"].is_boolean());
    assert!(block["filesystem"]["fsType"].is_string());
}
