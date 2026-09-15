//! What the `engines` producer actually writes, put through the aggregator that
//! judges it.
//!
//! Every fixture in this file is the real binary's output. That is not a style
//! choice: the first admission suite in this epic was green against a
//! hand-written fixture whose shape the producer never emits. It carried a
//! `runners` array that does not exist and a scalar `reps` where the real
//! `Measurement` writes a map, so a whole suite passed against a document
//! nobody writes, and the first real one was refused for 26 reasons. A fixture
//! is allowed to be a MUTATION of the producer's output and nothing else.
//!
//! The sweep is driven as a subprocess rather than called. `run_sweep`
//! re-executes `current_exe` to give every repetition its own process, and
//! under libtest that is the test binary, which would re-enter the harness
//! rather than the engine. `CARGO_BIN_EXE_engines` is the real binary.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use libviprs_bench::storage::archive::{self, ArchiveError};
use libviprs_bench::storage::integrity;
use serde_json::{Value, json};

/// Run the smallest real sweep once per test binary and keep what it wrote.
///
/// `ci` is the profile that exists to prove the harness runs: one small canvas
/// at one thread budget, three repetitions after a discarded pass, so this is
/// seconds rather than the minutes a publishable profile takes. Once, because
/// every test below wants the same bytes and twelve child processes is not a
/// thing to do six times.
fn producer_document() -> &'static str {
    static DOCUMENT: OnceLock<String> = OnceLock::new();
    DOCUMENT.get_or_init(|| {
        let exe = env!("CARGO_BIN_EXE_engines");
        // `--out`, because the binary keeps stdout for the per-cell child
        // protocol. A unique path per run: nothing here deletes, and pids
        // repeat in a fresh container while the target directory outlives it.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("k22-engines-{}-{nonce}.json", std::process::id()));
        let run = Command::new(exe)
            .args(["--family", "engines", "--profile", "ci", "--out"])
            .arg(&out)
            .output()
            .expect("the engines binary runs");
        assert!(
            run.status.success(),
            "the sweep exited {:?}\n--- stderr ---\n{}",
            run.status.code(),
            String::from_utf8_lossy(&run.stderr)
        );
        std::fs::read_to_string(&out)
            .unwrap_or_else(|e| panic!("the sweep wrote {}: {e}", out.display()))
    })
}

/// The producer's document, parsed.
fn document() -> Value {
    integrity::parse_document(producer_document()).expect("the sweep wrote canonical JSON")
}

/// Two refusals are properties of the harness running this test rather than of
/// the producer. `cargo test` builds with debug assertions on, which the
/// aggregator refuses and should; and this runs in a working tree that is dirty
/// by definition while somebody is working in it. Both are correct refusals of
/// THIS run and say nothing about the document's shape.
const ENVIRONMENTAL: [&str; 2] = ["debug-build", "dirty-not-allowed"];

/// The producer's document with only those two environmental facts corrected,
/// so the accept path can be exercised from a test build.
///
/// Nothing else is touched. Every field the refusal rules read is the one the
/// sweep wrote.
fn as_if_release_and_clean() -> Value {
    let mut doc = document();
    doc["provenance"]["node"]["debugAssertions"] = json!(false);
    doc["provenance"]["node"]["buildProfile"] = json!("release");
    doc["provenance"]["dirty"] = json!(false);
    doc["provenance"]["library"]["dirty"] = json!(false);
    doc
}

/// Refusal codes, counted, with the environmental ones dropped.
fn codes(doc: &Value) -> BTreeMap<String, usize> {
    let text = serde_json::to_string(doc).expect("it serialises");
    let refusals = archive::admit_text(&text).expect("the document canonicalises");
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for refusal in refusals {
        if ENVIRONMENTAL.contains(&refusal.code) {
            continue;
        }
        *out.entry(refusal.code.to_string()).or_default() += 1;
    }
    out
}

/// Refusal codes, counted, with NOTHING filtered.
///
/// `codes` drops the two environmental refusals, which is right for every test
/// about the document's shape and wrong for the one test that is ABOUT a
/// debug build: filtering there made the assertion unfalsifiable. It cost me a
/// red to notice, which is the right way round.
fn all_codes(doc: &Value) -> BTreeMap<String, usize> {
    let text = serde_json::to_string(doc).expect("it serialises");
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for refusal in archive::admit_text(&text).expect("the document canonicalises") {
        *out.entry(refusal.code.to_string()).or_default() += 1;
    }
    out
}

/// Every refusal, environmental ones included, as text for a message.
fn every_refusal(doc: &Value) -> String {
    let text = serde_json::to_string(doc).expect("it serialises");
    archive::admit_text(&text)
        .expect("the document canonicalises")
        .iter()
        .map(|r| format!("  {r}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// The end-to-end claim
// ---------------------------------------------------------------------------

/// RED against a producer whose own aggregator refuses its output, and against
/// a fixture that hides the difference.
///
/// The assertion is on the exact refusal set rather than on "no refusals",
/// because two of them are environmental and a test that demanded silence would
/// either be a lie or would have to be deleted the first time one reappeared.
/// What is asserted is that the set is the one I intend, so a new refusal is a
/// failure rather than something somebody notices in a log.
#[test]
fn the_sweep_writes_a_document_its_own_aggregator_accepts() {
    let doc = document();

    assert_eq!(doc["family"], "libviprs-engines");
    assert_eq!(doc["runner"], "libviprs-engines");
    assert!(
        doc["provenance"].is_object(),
        "the sweep fills its own provenance; `Document::new_for` leaves it null and the \
         aggregator refuses that"
    );
    assert!(
        doc["runId"].as_str().is_some_and(|id| !id.is_empty()),
        "and it stamps its own run id from that provenance: {:?}",
        doc["runId"]
    );
    assert!(
        !doc["cells"].as_array().expect("cells").is_empty(),
        "an empty reading is a refusal, not a result"
    );

    // Every digested block is a key the producer really writes. This is the
    // guard for the class of bug that cost the storage module one of its four
    // digests: `BLOCKS` looked up a key `Document` does not emit, so that
    // digest was the hash of the four bytes `null` on every document ever
    // written and the block that "never moved" could not move.
    for key in integrity::digested_keys() {
        assert!(
            doc.get(key).is_some(),
            "the `{key}` block is digested but the producer writes no such key, so its digest \
             is a constant"
        );
    }

    let expected: BTreeMap<String, usize> = BTreeMap::new();
    assert_eq!(
        codes(&doc),
        expected,
        "the producer's own document is refused by its own aggregator:\n{}",
        every_refusal(&doc)
    );
}

/// RED against an aggregator that only accepts storage documents.
///
/// The acceptance criterion in words: `--check` and `--archive` take an
/// `engines` document under the same rules. Through the real binary, because
/// that is what the criterion is about.
#[test]
fn the_aggregator_binary_checks_and_archives_an_engines_document() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("k22-admit-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let path = dir.join("engines-results.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&as_if_release_and_clean()).expect("it serialises"),
    )
    .expect("the fixture is written");

    let aggregate = env!("CARGO_BIN_EXE_storage-aggregate");
    let check = Command::new(aggregate)
        .arg("--check")
        .arg(&path)
        .output()
        .expect("the aggregator runs");
    assert!(
        check.status.success(),
        "--check refused an engines document:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );

    let root = dir.join("archive");
    let archived = Command::new(aggregate)
        .args(["--archive"])
        .arg(&path)
        .arg("--root")
        .arg(&root)
        .output()
        .expect("the aggregator runs");
    assert!(
        archived.status.success(),
        "--archive refused an engines document:\n{}",
        String::from_utf8_lossy(&archived.stderr)
    );

    // It was sealed, it is in the index, and it verifies.
    let files: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("the archive directory exists")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    assert_eq!(files.len(), 2, "one document and one index: {files:?}");
    let filed = files
        .iter()
        .find(|p| p.file_name().unwrap() != "index.json")
        .expect("a filed document");
    let verify = Command::new(aggregate)
        .arg("--verify")
        .arg(filed)
        .output()
        .expect("the aggregator runs");
    assert!(
        verify.status.success(),
        "an archived engines document does not verify:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );

    // Archiving it again changes nothing: the id is derived from the document's
    // own evidence, so an idempotent re-archive must not double the series.
    let again = Command::new(aggregate)
        .args(["--archive"])
        .arg(&path)
        .arg("--root")
        .arg(&root)
        .output()
        .expect("the aggregator runs");
    assert!(again.status.success());
    assert!(
        String::from_utf8_lossy(&again.stdout).contains("already archived"),
        "{}",
        String::from_utf8_lossy(&again.stdout)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// RED against a default archive root that ignores the document's family.
///
/// `--archive` with no `--root` has to read the family out of the document and
/// file it under `archive/<family>/`. Driven through the binary with its working
/// directory moved, because the default root is relative and a unit test of
/// `dir_for_document` proves the mapping without proving the binary uses it.
#[test]
fn the_aggregator_files_an_engines_document_under_its_own_family_by_default() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let cwd = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("k22-default-root-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&cwd).expect("a scratch working directory");
    let path = cwd.join("engines-results.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&as_if_release_and_clean()).expect("it serialises"),
    )
    .expect("the fixture is written");

    let archived = Command::new(env!("CARGO_BIN_EXE_storage-aggregate"))
        .arg("--archive")
        .arg(&path)
        .current_dir(&cwd)
        .output()
        .expect("the aggregator runs");
    assert!(
        archived.status.success(),
        "--archive with no --root refused an engines document:\n{}",
        String::from_utf8_lossy(&archived.stderr)
    );
    assert!(
        cwd.join("archive/engines/index.json").is_file(),
        "it lands under archive/engines/, not archive/storage/: {:?}",
        std::fs::read_dir(cwd.join("archive")).map(|d| d
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .collect::<Vec<_>>())
    );
    assert!(
        !cwd.join("archive/storage").exists(),
        "and nothing was written into the other family's directory"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}

/// RED against one archive directory for both families.
///
/// Two families derive their run ids the same way from the same fields, so a
/// storage and an engines sweep started in the same second against the same
/// commit on the same host derive the SAME id. In one directory the second
/// would be reported as a collision with the first, which is a true statement
/// about ids and a useless one about the runs.
#[test]
fn an_engines_document_is_filed_apart_from_a_storage_one() {
    let doc = document();
    assert_eq!(
        archive::dir_for_document(&doc),
        std::path::Path::new("archive/engines")
    );
    assert_eq!(
        archive::dir_for_family("libviprs-storage"),
        std::path::Path::new("archive/storage")
    );
    assert_ne!(
        archive::dir_for_family("libviprs-engines"),
        archive::dir_for_family("libviprs-storage")
    );
    // A family nothing here has heard of gets its own directory rather than
    // being filed beside documents that mean something else.
    assert_eq!(
        archive::dir_for_family("libviprs-thermals"),
        std::path::Path::new("archive/thermals")
    );

    // And the collision this prevents is real: the same id really does come out
    // of both families' evidence.
    let mut as_storage = doc.clone();
    as_storage["family"] = json!("libviprs-storage");
    assert_eq!(
        archive::run_id(&doc).expect("an id"),
        archive::run_id(&as_storage).expect("an id"),
        "the id is a function of the run's environment and not of its family, which is why \
         the directory has to be"
    );
}

// ---------------------------------------------------------------------------
// The refusals, each fired on an engines document
// ---------------------------------------------------------------------------

/// RED against refusal rules that were only ever exercised on a storage
/// document.
///
/// Each mutation is applied to the real `engines` output, so a rule that
/// reached into a storage-shaped field, or an aggregator that read the family
/// and took a different path, fails here.
#[test]
fn an_engines_document_without_provenance_is_refused() {
    let mut blind = as_if_release_and_clean();
    blind["provenance"] = Value::Null;
    let refused = codes(&blind);
    for expected in ["emulated", "commit", "dirty", "filesystem", "invocation"] {
        assert!(
            refused.contains_key(expected),
            "a document with no provenance has to be refused for {expected}: {refused:?}"
        );
    }

    // A control: the same document WITH its provenance is not refused for any
    // of them, so the assertion above is about the mutation rather than about
    // the engines family being refused generally.
    assert!(codes(&as_if_release_and_clean()).is_empty());
}

/// RED against an aggregator that measures a debug build and says nothing.
///
/// Every bounds check and overflow check in the engine is then inside the
/// measurement, and the numbers are of a build nobody ships.
#[test]
fn an_engines_document_built_in_debug_is_refused() {
    let mut debug = as_if_release_and_clean();
    debug["provenance"]["node"]["debugAssertions"] = json!(true);
    debug["provenance"]["node"]["buildProfile"] = json!("debug");
    let text = serde_json::to_string(&debug).expect("it serialises");
    let refusals = archive::admit_text(&text).expect("it canonicalises");
    assert_eq!(
        refusals.iter().filter(|r| r.code == "debug-build").count(),
        2,
        "both halves refuse: the assertions flag and the profile name: {refusals:?}"
    );

    // And a document that does not SAY is refused too. That is the case the
    // published PMTiles numbers are in, and refusing only `true` would admit
    // exactly the artefact this epic exists because of.
    let mut silent = as_if_release_and_clean();
    silent["provenance"]["node"]["debugAssertions"] = Value::Null;
    assert!(all_codes(&silent).contains_key("debug-build"));

    // And the control that makes the two above mean something: with both
    // fields saying release, nothing is refused for the build at all.
    assert!(!all_codes(&as_if_release_and_clean()).contains_key("debug-build"));
}

/// RED against an aggregator that refuses only an admitted emulation.
///
/// Three states, not two: `false` passes, `true` is refused because the timings
/// describe the translator as much as the code, and anything else (a probe that
/// could not observe, a missing field) is refused because nothing in the document
/// can settle it. The third is the state the published numbers this
/// epic replaced are in.
#[test]
fn an_engines_document_taken_under_emulation_is_refused() {
    let mut emulated = as_if_release_and_clean();
    emulated["provenance"]["emulated"] = json!(true);
    assert_eq!(codes(&emulated).get("emulated"), Some(&1));

    let mut unsure = as_if_release_and_clean();
    unsure["provenance"]["emulated"] = json!("unknown");
    assert_eq!(codes(&unsure).get("emulated"), Some(&1));

    let mut absent = as_if_release_and_clean();
    absent["provenance"]
        .as_object_mut()
        .expect("provenance is an object")
        .remove("emulated");
    assert_eq!(codes(&absent).get("emulated"), Some(&1));

    // The control: the real run observed itself native, and that is why the
    // three above are about the mutation.
    assert_eq!(document()["provenance"]["emulated"], json!(false));
}

/// RED against an `ok` cell nobody looked at.
///
/// An engine that was never observed to have written the pyramid its row is
/// about is a label, not a measurement, and the aggregator refuses `null`
/// exactly as it refuses `false`.
#[test]
fn an_engines_document_with_an_unattested_ok_cell_is_refused() {
    for replacement in [json!(false), Value::Null] {
        let mut doc = as_if_release_and_clean();
        doc["cells"][0]["attested"] = replacement.clone();
        assert_eq!(
            codes(&doc).get("unattested-cell"),
            Some(&1),
            "attested {replacement} on an ok cell has to be refused"
        );
    }

    let mut gone = as_if_release_and_clean();
    gone["cells"][0]
        .as_object_mut()
        .expect("a cell is an object")
        .remove("attested");
    assert_eq!(codes(&gone).get("unattested-cell"), Some(&1));
}

/// RED against a document that cannot agree with itself about how many times it
/// measured.
///
/// This is the rule the hand-written storage fixture broke: it carried a scalar
/// `reps` where the producer writes a map, so the rule matched on nothing and
/// refused every real run. Here both halves come from the producer and the
/// mutation is what has to be caught.
#[test]
fn an_engines_document_whose_reps_disagree_is_refused() {
    let doc = document();
    assert_eq!(
        doc["measurement"]["reps"], doc["provenance"]["invocation"]["resolved"]["reps"],
        "the producer's two halves agree, which is what makes the mutation below meaningful"
    );
    assert_eq!(
        doc["measurement"]["reps"],
        json!({ "pyramid": 3 }),
        "and the shape is the producer's own, not a scalar somebody imagined"
    );

    let mut disagreeing = as_if_release_and_clean();
    disagreeing["provenance"]["invocation"]["resolved"]["reps"] = json!({ "pyramid": 7 });
    assert_eq!(codes(&disagreeing).get("reps-disagree"), Some(&1));

    // A cell that took fewer repetitions than its own floor is the other half
    // of the rule: a sweep that declares three and has a cell that managed one.
    let mut short = as_if_release_and_clean();
    short["cells"][0]["reps"] = json!(1);
    assert_eq!(codes(&short).get("reps-disagree"), Some(&1));
}

/// RED against a document whose scenarios were never resolved.
///
/// `"all"` means a different set on every day the suite grows, so two runs that
/// both say it are not comparable.
#[test]
fn an_engines_document_with_unresolved_scenarios_is_refused() {
    assert_eq!(
        document()["provenance"]["invocation"]["resolved"]["scenarios"],
        json!(["pyramid"]),
        "the producer writes the names out"
    );
    let mut vague = as_if_release_and_clean();
    vague["provenance"]["invocation"]["resolved"]["scenarios"] = json!(["all"]);
    assert_eq!(codes(&vague).get("scenarios-unresolved"), Some(&1));
}

/// RED against a digest that cannot move, over an engines document.
///
/// Sealing is idempotent in the only sense that matters: `combinedAt` moves on
/// every call and is not covered, so the `document` digest and the archive path
/// are the same on the second call as on the first. Change one sample and the
/// cells digest and the document digest move while the environment blocks stay
/// put, and that pattern is the finding.
#[test]
fn the_four_digests_over_an_engines_document_move_when_its_numbers_do() {
    let doc = as_if_release_and_clean();
    let sealed = archive::seal(&doc).expect("it seals");
    let again = archive::seal(&doc).expect("it seals twice");
    assert_eq!(
        sealed.digests, again.digests,
        "sealing twice must not move a digest, or --verify can never pass"
    );

    let mut edited = doc.clone();
    let samples = edited["cells"][0]["samples"]
        .as_array_mut()
        .expect("a samples array");
    samples[0] = json!(samples[0].as_f64().expect("a sample") + 1.0);
    let moved = archive::seal(&edited).expect("it seals");
    assert_ne!(moved.digests.cells, sealed.digests.cells);
    assert_ne!(moved.digests.document, sealed.digests.document);
    assert_eq!(
        moved.digests.measurements, sealed.digests.measurements,
        "an edit to one sample is a change to the numbers rather than to the environment they \
         were measured in, and the point of four digests is to say so"
    );
    assert_eq!(moved.digests.runners, sealed.digests.runners);
}

/// RED against an archive that files two different runs under one id.
#[test]
fn an_engines_run_that_collides_is_refused_rather_than_overwritten() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("k22-collide-{}-{nonce}", std::process::id()));
    let doc = as_if_release_and_clean();
    archive::archive(&doc, &root).expect("the first one files");

    let mut different = doc.clone();
    different["cells"][0]["samples"][0] = json!(1.0);
    match archive::archive(&different, &root) {
        Err(ArchiveError::Collision { .. }) => {}
        other => panic!("a second run under one id must not overwrite the first: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The sweep looks at the machine before it measures anything, and the document
/// carries what it saw.
///
/// This is the reading the publish gate turns on (#100). A cell's own
/// `machineLoad` cannot say whether anyone else was on the box, because a
/// one-minute load average has a one-minute memory and this family spends every
/// second of it saturating the cores it was given. `startingLoad` is taken
/// before the first child is spawned, so at that instant none of the load is
/// ours.
///
/// Goes red against: a runner that never samples, which leaves the field null
/// and earns the importer's refusal; and against a `skip_serializing_if` that
/// drops the key, which would make an unsampled run indistinguishable from one
/// written before the field existed, and those two are judged by different
/// rules.
///
/// What it cannot see is a sample taken at the END of the sweep. That is held by
/// the call site instead: the sample is taken before `Document::new_for` runs,
/// so it necessarily precedes every cell the document holds.
#[test]
fn the_sweep_records_the_machine_before_it_measures_anything() {
    let doc = document();
    let starting = &doc["startingLoad"];
    assert!(
        starting.is_object(),
        "the sweep published `startingLoad: {starting}`, so nothing looked at the machine \
         before it started measuring"
    );

    // `available_parallelism` answers on every platform this crate builds for,
    // so the core count is not allowed to be a platform excuse.
    assert!(
        starting["cores"].as_u64().is_some_and(|n| n > 0),
        "the core count is {} and a zero-core host is a read that failed",
        starting["cores"]
    );

    // No `#[cfg]` split. Both platforms this crate builds for can say what they
    // are carrying, Linux through `/proc/loadavg` and macOS through
    // `getloadavg`, and `MachineLoad` now goes through the one reader that knows
    // both. A test that asserted on Linux and shrugged elsewhere would be a skip
    // wearing a pass's colour, and it would have hidden exactly the bug this
    // paragraph is about: the old reader was Linux-only, so a document captured
    // on a Mac published a null load the publish gate then refused as
    // unreadable.
    let load = starting["loadAvg1m"]
        .as_f64()
        .expect("every platform this crate builds for reports a load average");
    let contention = starting["contentionPerCore"]
        .as_f64()
        .expect("the contention is derived from the load and the cores");
    let cores = starting["cores"].as_u64().expect("the cores are a number") as f64;
    assert!(load >= 0.0 && load.is_finite(), "load {load}");
    assert!(
        (contention - load / cores).abs() < 1e-9,
        "the contention must be the load over the cores: {contention} against {load}/{cores}"
    );
    assert_eq!(
        starting["quiet"].as_bool(),
        Some(contention < 1.0),
        "the verdict has to follow from the number beside it, or the importer refuses the run"
    );
}
