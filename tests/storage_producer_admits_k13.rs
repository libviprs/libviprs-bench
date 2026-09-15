//! What the producer actually writes, put through the aggregator that judges it
//! (libviprs-bench #66).
//!
//! Every other test in this lane feeds `admit` a document a test built. This one
//! feeds it a document the *sweep* built, and it exists because the two had
//! drifted apart without anything noticing: the hand-written fixture carried a
//! `runners` array the producer never writes and a scalar `reps` where the real
//! `Measurement` emits `{generate, read}`, so a whole admit suite was green
//! against a shape that does not exist. Running `storage --profile ci` and then
//! `storage-aggregate --check` over the result refused for 26 reasons.
//!
//! The sweep is driven as a subprocess rather than called. `run_sweep`
//! re-executes `current_exe` to isolate each cell, and under libtest that is the
//! test binary, which would re-enter the harness rather than the scenario.
//! `CARGO_BIN_EXE_storage` is the real binary and is what K1.4 found for the same
//! reason.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use libviprs_bench::storage::archive;

/// Run the smallest real sweep and hand what it wrote to the aggregator.
///
/// `ci` is the profile that exists to prove the harness runs: one small cell, so
/// this is seconds rather than the minutes a publishable profile takes.
fn sweep_document() -> String {
    let exe = env!("CARGO_BIN_EXE_storage");
    // `--out`, because the binary writes the document to a file and keeps stdout
    // for the per-cell child protocol. A unique path per run: nothing here
    // deletes, and pids repeat in a fresh container while the target directory
    // outlives it, which has already cost this lane one confusing red.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("k13-sweep-{}-{nonce}.json", std::process::id()));
    let run = Command::new(exe)
        .args(["--profile", "ci", "--out"])
        .arg(&out)
        .output()
        .expect("the storage binary runs");
    assert!(
        run.status.success(),
        "the sweep exited {:?}\n--- stderr ---\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
    std::fs::read_to_string(&out)
        .unwrap_or_else(|e| panic!("the sweep wrote {}: {e}", out.display()))
}

/// RED against a producer whose own aggregator refuses its output, and against a
/// fixture that hides the difference.
///
/// The assertion is on the exact refusal set rather than on "no refusals",
/// because some of them are deliberately still open and a test that demanded
/// silence would either be a lie or would have to be deleted the first time one
/// reappeared. What is asserted is that the set is the one I intend, so a new
/// refusal is a failure rather than a thing somebody notices in a log.
#[test]
fn the_sweep_writes_a_document_its_own_aggregator_accepts() {
    let text = sweep_document();

    // It is a document at all, and it is the shape `Document` writes.
    let parsed: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("the sweep wrote JSON: {e}"));
    assert_eq!(parsed["family"], "libviprs-storage");
    assert!(
        parsed["provenance"].is_object(),
        "the sweep must fill its own provenance; `Document::new` leaves it null and the \
         aggregator refuses that, which it earned on every run until this was wired up"
    );
    // The reading the publish gate turns on (#100). `Document::new` leaves this
    // null too, and the importer refuses a null: a runner that skipped its own
    // first act has not looked at the machine, and a sweep that did not look
    // cannot say it had the box to itself. The core count is asserted because
    // `available_parallelism` answers on every platform this crate builds for,
    // so a zero there is a read that failed rather than a platform excuse.
    assert!(
        parsed["startingLoad"].is_object(),
        "the sweep must sample the machine before it measures anything, got {}",
        parsed["startingLoad"]
    );
    assert!(
        parsed["startingLoad"]["cores"]
            .as_u64()
            .is_some_and(|n| n > 0),
        "the starting load must carry a core count, got {}",
        parsed["startingLoad"]["cores"]
    );
    assert!(
        !parsed["cells"]
            .as_array()
            .expect("cells is an array")
            .is_empty(),
        "an empty reading is a refusal, not a result"
    );

    // Every digested block is a key the producer really writes. This is the
    // guard for the class of bug that cost this module one of its four digests:
    // `BLOCKS` looked up `runners` and `Document` emits `runner`, so the digest
    // was `sha256` of the four bytes `null` on every document ever written, and
    // the block that "never moved" could not move.
    for key in libviprs_bench::storage::integrity::digested_keys() {
        assert!(
            parsed.get(key).is_some(),
            "the `{key}` block is digested but the producer writes no such key, so its \
             digest is a constant"
        );
    }

    let refusals = archive::admit_text(&text).expect("the document canonicalises");

    // Two refusals are properties of the harness running this test rather than
    // of the producer, and excluding them is what keeps the assertion honest
    // instead of making it pass. `cargo test` builds with debug assertions on,
    // which the aggregator refuses and should; and this runs in a working tree
    // that is dirty by definition while somebody is working in it. Both are
    // correct refusals of THIS run and say nothing about the document's shape.
    // Everything else has to be gone.
    const ENVIRONMENTAL: [&str; 2] = ["debug-build", "dirty-not-allowed"];
    let mut by_code: BTreeMap<&str, usize> = BTreeMap::new();
    for refusal in &refusals {
        if ENVIRONMENTAL.contains(&refusal.code) {
            continue;
        }
        *by_code.entry(refusal.code).or_default() += 1;
    }

    // Nothing else open. The sweep now fills its own provenance, attests each
    // artefact from a walk of the real archive, and every cell meets the
    // repetition floor its own scenario declared.
    let expected: BTreeMap<&str, usize> = BTreeMap::new();
    assert_eq!(
        by_code,
        expected,
        "the producer's own document is refused by its own aggregator:\n{}",
        refusals
            .iter()
            .map(|r| format!("  {r}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// RED against an attestation that does not look.
///
/// The reviewer's warning was that the likely shortcut for all this is to stamp
/// `attested: true` and move on, and a mutation row proved my first
/// attempt at guarding it was worthless: stamping `Some(true)` in `run_sweep`
/// left every assertion I had written green. It had to, and the reason is worth
/// recording rather than hiding. In the `ci` profile every cell is legitimately
/// attested, so the document contains nothing but `true`, and no assertion over
/// a document of all-`true` can tell an observation from a constant.
///
/// So the observation is tested where it can fail. `attest_artefacts` is handed
/// a real archive and then one that is not there, and the verdict has to move.
/// A constant anywhere in that path fails this.
///
/// What this does NOT cover, stated plainly: the one line in `run_sweep` that
/// copies the verdict onto the row. A constant there is indistinguishable from a
/// correct run in which everything really is attested, and the mutation row for
/// it is published as surviving rather than quietly dropped.
#[test]
fn the_attestation_moves_when_the_archive_does() {
    use libviprs_bench::storage::cells::{Backend, Cell, Profile, Source};
    use libviprs_bench::storage::{Scratch, attest_artefacts, write_pyramid};

    let cell = Cell::new(256, 256, 256, Source::Gradient, 1);
    let plan = cell.plan().expect("the cell plans");

    let scratch = Scratch::new(None).expect("a scratch directory");
    let written = write_pyramid(Backend::PmTiles, cell, &plan, scratch.path())
        .expect("the archive is written");
    let real = vec![(Backend::PmTiles, scratch, written.output)];

    // A real archive, on its own, has no second backend to agree with, so the
    // byte half cannot be satisfied and the verdict is honestly negative. That
    // is the point: the verdict is a function of what was there.
    let alone = attest_artefacts(cell, &plan, Profile::Ci, &real);
    assert_eq!(alone.len(), 1);
    assert!(
        !alone[0].1.is_attested(),
        "one backend cannot agree with itself about a tile's bytes, and claiming it can \
         is the label-shaped answer: {:?}",
        alone[0].1.reasons()
    );
    assert!(
        alone[0]
            .1
            .reasons()
            .iter()
            .any(|r| r.contains("coordinates")),
        "the refusal has to name the byte half: {:?}",
        alone[0].1.reasons()
    );

    // An archive that is not there at all cannot be observed, and that is a
    // different refusal from one that was observed and disagreed.
    let missing = vec![(
        Backend::PmTiles,
        Scratch::new(None).expect("a scratch directory"),
        std::path::PathBuf::from("/nonexistent/archive.pmtiles"),
    )];
    let unobserved = attest_artefacts(cell, &plan, Profile::Ci, &missing);
    assert!(!unobserved[0].1.is_attested());
    assert!(
        unobserved[0]
            .1
            .reasons()
            .iter()
            .any(|r| r.contains("no entries")),
        "an unreadable archive is unobserved, not small: {:?}",
        unobserved[0].1.reasons()
    );

    // And an empty artefact list attests nothing rather than everything.
    assert!(attest_artefacts(cell, &plan, Profile::Ci, &[]).is_empty());
}
