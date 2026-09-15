//! The `engines` document: one envelope, two families, and the shape it cannot
//! express.
//!
//! Every test here names, in a comment, the wrong implementation it goes red
//! against. A test that stays green under that mutation is not the test.
//!
//! Nothing in this file runs a sweep. The rows are built through
//! `engines::rows_for`, which is the function the sweep calls, so the shapes
//! asserted here are the product's shapes rather than a fixture's; the suite
//! that drives the real binary end to end is
//! `tests/engines_producer_admits.rs`, and it exists because a fixture whose
//! shape the producer never emits is how this epic lost a whole admission suite
//! once already.

use std::time::Duration;

use libviprs_bench::engines::cells::{EngineCell, Profile};
use libviprs_bench::engines::{self, METRICS};
use libviprs_bench::harness::Engine;
use libviprs_bench::storage::document::{CELL_FIELDS, DOCUMENT_FIELDS, Document, MachineLoad};
use libviprs_bench::storage::stats;
use libviprs_bench::{ArtefactFacts, RunMetrics};
use serde_json::Value;

/// One child's metrics, with every field a real child fills.
fn run(engine: Engine, wall_ms: u64, rss_bytes: u64, tiles: u64) -> RunMetrics {
    RunMetrics {
        label: format!("1024x720_c1_{}", engine.as_str()),
        width: 1024,
        height: 720,
        engine: engine.as_str().to_string(),
        measurement_path: String::new(),
        wall_time: Duration::from_millis(wall_ms),
        tracked_memory_bytes: 2_097_152,
        peak_rss_bytes: rss_bytes,
        stats: None,
        per_level_tiles: vec![16, 4, 4, 1],
        artefact: Some(ArtefactFacts {
            output_bytes: 1_438_844,
            filesystem_entries: 37,
            directories: 12,
            // Deliberately different between two runs of one engine below, to
            // prove the producer is not quietly folding it into the agreement.
            allocated_bytes: 1_474_560,
        }),
        equivalence_psnr_db: None,
        tiles_produced: tiles,
        levels_processed: 4,
        tiles_skipped: 0,
        strips: 0,
        batches: 0,
        inflight_strips: 0,
        concurrency: 1,
        memory_budget_bytes: 0,
    }
}

fn cell() -> EngineCell {
    EngineCell::new(1024, 720, 1)
}

/// The rows one engine's three repetitions earn, through the producer's path.
fn rows(runs: &[RunMetrics]) -> Vec<libviprs_bench::storage::document::DocumentCell> {
    engines::rows_for(
        cell(),
        Engine::Monolithic,
        runs,
        &[601.0],
        Profile::Ci,
        MachineLoad::unknown(),
        Some(stats::TimerProbe {
            tick_ns: 41.0,
            call_ns: 42.0,
        }),
    )
}

fn three_reps() -> Vec<RunMetrics> {
    vec![
        run(Engine::Monolithic, 510, 9_700_000, 25),
        run(Engine::Monolithic, 517, 9_800_000, 25),
        run(Engine::Monolithic, 558, 10_000_000, 25),
    ]
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).expect("the document parses")
}

fn keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect()
}

/// A document with one cell's worth of rows in it, serialised.
fn document() -> Value {
    let mut doc = Document::new_for(
        engines::FAMILY,
        engines::RUNNER,
        Profile::Ci.label(),
        "2026-09-14T12:00:00.000Z".to_string(),
        engines::measurement(Profile::Ci),
    );
    for row in rows(&three_reps()) {
        doc.push(row);
    }
    doc.rebuild_invariant_table();
    parse(&doc.to_json())
}

// ---------------------------------------------------------------------------
// One envelope
// ---------------------------------------------------------------------------

/// RED against a second document type for the second family.
///
/// This is the whole shape of the lane in one assertion: the `engines` document
/// carries the storage document's top-level keys, in the storage document's
/// order, and its cells carry the storage document's cell keys, in that order.
/// Fork `Document` and one of the two lists moves.
///
/// Against the declared constants and not against a storage document built
/// here, because the constants are what `tests/storage_document.rs` holds the
/// storage side to. Both families being checked against one list is the point.
#[test]
fn the_engines_document_is_the_same_envelope_the_storage_one_is() {
    let doc = document();
    assert_eq!(
        keys(&doc),
        DOCUMENT_FIELDS.to_vec(),
        "the engines document has to carry the same top-level keys in the same order as the \
         storage one; two families under one page cannot be two shapes"
    );
    assert_eq!(doc["family"], engines::FAMILY);
    assert_eq!(doc["runner"], engines::RUNNER);
    assert_eq!(
        doc["schemaVersion"],
        libviprs_bench::storage::document::SCHEMA_VERSION
    );
    for cell in doc["cells"].as_array().expect("cells is an array") {
        assert_eq!(
            keys(cell),
            CELL_FIELDS.to_vec(),
            "every engines cell carries the storage cell's keys in the storage cell's order"
        );
    }
}

/// RED against a family that publishes four columns, or seven.
///
/// The six are the columns the engines comparison has always had, and the two
/// derived ones are the reason the list is checked rather than eyeballed: they
/// are the easiest to drop, because they are the ones nothing else computes.
#[test]
fn a_cell_publishes_all_six_columns_with_their_units_and_directions() {
    let doc = document();
    let published: Vec<(String, String, String)> = doc["cells"]
        .as_array()
        .expect("cells")
        .iter()
        .map(|c| {
            (
                c["metric"].as_str().unwrap().to_string(),
                c["unit"].as_str().unwrap().to_string(),
                c["direction"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let expected: Vec<(String, String, String)> = METRICS
        .iter()
        .map(|m| {
            (
                m.name.to_string(),
                m.unit.as_str().to_string(),
                m.direction.as_str().to_string(),
            )
        })
        .collect();
    assert_eq!(published, expected);
    assert_eq!(
        published.len(),
        6,
        "wall, peak RSS, tracked memory, throughput, and the two derived columns"
    );
    for cell in doc["cells"].as_array().expect("cells") {
        assert_eq!(
            cell["key"],
            format!("pyramid.{}", cell["metric"].as_str().unwrap()),
            "the page sections on <scenario>.<metric>"
        );
    }
}

// ---------------------------------------------------------------------------
// Repetitions
// ---------------------------------------------------------------------------

/// RED against the sweep this replaces, which measured once and copied the one
/// value into a field called `median` with no `samples` beside it.
///
/// Also RED against a producer that publishes `samples` and then fills `median`
/// from something else: the assertion recomputes the median from the published
/// samples rather than comparing it to the input.
#[test]
fn every_cell_carries_reps_samples_a_median_of_them_and_a_confidence() {
    let doc = document();
    for cell in doc["cells"].as_array().expect("cells") {
        let name = format!("{}.{}", cell["backend"], cell["metric"]);
        let samples: Vec<f64> = cell["samples"]
            .as_array()
            .unwrap_or_else(|| panic!("{name} has a samples array"))
            .iter()
            .map(|v| v.as_f64().expect("a sample is a number"))
            .collect();
        assert_eq!(
            samples.len(),
            3,
            "{name} published {} samples for three repetitions",
            samples.len()
        );
        assert_eq!(cell["reps"], 3, "{name} declares its repetitions");
        assert_eq!(cell["minReps"], 3, "{name} declares its floor");
        assert_eq!(
            cell["median"].as_f64(),
            stats::median(&samples),
            "{name}'s median has to be the median OF THE PUBLISHED SAMPLES, or the field is a \
             single shot wearing a statistic's name"
        );
        assert!(
            cell["confidence"].as_str() == Some("high")
                || cell["confidence"].as_str() == Some("low"),
            "{name} says how much to trust it"
        );
        assert!(
            cell["iqr"].is_number() && cell["ci95"].is_array(),
            "{name} carries a spread and an interval"
        );
    }
}

/// RED against a warm-up that is kept, and against one that is claimed and
/// never run.
///
/// The document has to be able to prove the discarded pass is not in the
/// samples, which is why `discardedWarmup` carries its values rather than a
/// count.
#[test]
fn the_discarded_warmup_is_published_and_is_not_in_the_samples() {
    let doc = document();
    let wall = doc["cells"]
        .as_array()
        .expect("cells")
        .iter()
        .find(|c| c["metric"] == "wall")
        .expect("a wall row");
    let discarded: Vec<f64> = wall["discardedWarmup"]
        .as_array()
        .expect("discardedWarmup is an array")
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert_eq!(discarded, vec![601.0]);
    let samples: Vec<f64> = wall["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    for value in &discarded {
        assert!(
            !samples.contains(value),
            "a discarded pass that is also a sample was not discarded"
        );
    }
    assert_eq!(wall["warmup"]["policy"], "one-discarded-pass");
    assert_eq!(wall["isolation"], "process-per-rep");
}

/// RED against derived columns computed from the medians instead of per
/// repetition.
///
/// A ratio of two medians is not the median of the ratio, and only the
/// per-repetition form has a spread at all. The check is arithmetic rather than
/// stylistic: each derived sample has to equal the ratio of THAT repetition's
/// inputs, which a summary-derived implementation cannot satisfy for more than
/// one of them.
#[test]
fn the_derived_columns_are_derived_once_per_repetition() {
    let runs = three_reps();
    let built = rows(&runs);
    let by = |name: &str| -> Vec<f64> {
        built
            .iter()
            .find(|c| c.metric == name)
            .unwrap_or_else(|| panic!("a {name} row"))
            .samples
            .clone()
    };
    let tps = by("tiles_per_second");
    let tps_mb = by("tiles_per_second_per_mb");
    let cost = by("resource_cost");
    assert_eq!(tps.len(), runs.len());

    for (i, metrics) in runs.iter().enumerate() {
        let secs = metrics.wall_time.as_secs_f64();
        let rss_mb = metrics.peak_rss_mb();
        let tiles = metrics.tiles_produced as f64;
        assert_eq!(tps[i], tiles / secs, "tiles_per_second of repetition {i}");
        assert_eq!(
            tps_mb[i],
            (tiles / secs) / rss_mb,
            "tiles_per_second_per_mb of repetition {i}"
        );
        assert_eq!(
            cost[i],
            (rss_mb * secs) / tiles,
            "resource_cost of repetition {i}"
        );
    }

    // And the positive control the arithmetic above needs: the repetitions have
    // to differ, or every implementation passes.
    assert!(
        tps_mb.windows(2).any(|w| w[0] != w[1]),
        "the fixture's repetitions must differ, or this test cannot see a summary-derived \
         implementation"
    );
}

/// RED against the old `to_point`, which wrote `0.0` whenever a denominator was
/// zero.
///
/// Higher is better on two of these columns, so a hole written as a zero is a
/// hole that reads as the worst possible score on one column and, on
/// `resource_cost`, as the BEST possible score. The right answer is no sample:
/// the cell then falls short of its own floor and says so in
/// `lowConfidenceReasons`.
#[test]
fn a_derived_column_with_no_denominator_publishes_no_sample_rather_than_a_zero() {
    let mut runs = three_reps();
    // A child whose RSS could not be read. `wait4` gave nothing and the
    // self-report was zero, which is what `peak_rss_bytes: 0` means.
    runs[1].peak_rss_bytes = 0;
    let built = rows(&runs);

    // BOTH derived columns, because both divide by the peak RSS and a test that
    // checked one of them let a mutation through: publishing `resource_cost` as
    // `0.0` where there is no denominator survived this test until the mutation
    // row caught it. Lower is better on resource cost, so a hole written as a
    // zero there is the BEST possible score.
    for metric in ["tiles_per_second_per_mb", "resource_cost"] {
        let row = built
            .iter()
            .find(|c| c.metric == metric)
            .unwrap_or_else(|| panic!("a {metric} row"));
        assert_eq!(
            row.samples.len(),
            2,
            "{metric}: the repetition with no RSS contributes no sample: {:?}",
            row.samples
        );
        assert!(
            !row.samples.contains(&0.0),
            "{metric}: a zero here reads as a measurement rather than as a hole"
        );
        assert_eq!(
            row.reps, 3,
            "{metric}: the cell still declares three repetitions"
        );
        assert_eq!(row.confidence, "low", "{metric}");
        assert!(
            row.low_confidence_reasons
                .iter()
                .any(|r| r.contains("minReps")),
            "{metric} says why: {:?}",
            row.low_confidence_reasons
        );
    }

    // And the control: the columns that do not divide by the RSS keep all three
    // samples, so the assertion above is about the denominator rather than about
    // the repetition being dropped everywhere.
    for metric in ["wall", "tracked_memory_mb", "tiles_per_second"] {
        let row = built
            .iter()
            .find(|c| c.metric == metric)
            .unwrap_or_else(|| panic!("a {metric} row"));
        assert_eq!(
            row.samples.len(),
            3,
            "{metric} needs no RSS to be computable"
        );
    }
}

// ---------------------------------------------------------------------------
// Invariants
// ---------------------------------------------------------------------------

/// RED against publishing `tiles_produced` as a timing, and against a producer
/// that averages an invariant that moved.
///
/// The verdict is equality: every repetition agreed, or the field is `null` and
/// the cell is refused with the field named. An invariant that differs between
/// two repetitions of one commit is a defect rather than a delta, so there is
/// nothing to average.
#[test]
fn tiles_produced_is_an_invariant_with_an_equality_verdict() {
    let built = rows(&three_reps());
    for cell in &built {
        assert_eq!(
            cell.invariants.tiles_produced,
            Some(25),
            "every row carries the cell's tile count as an exact invariant"
        );
        assert_eq!(cell.outcome, "ok");
        assert_eq!(cell.reason, None);
    }

    // The same cell with one repetition that produced a different number of
    // tiles. Nothing is averaged: the field goes null, the cell is refused and
    // the reason names the field.
    let mut moved = three_reps();
    moved[2].tiles_produced = 24;
    let refused = rows(&moved);
    for cell in &refused {
        assert_eq!(
            cell.invariants.tiles_produced, None,
            "a tile count that moved is not a tile count"
        );
        assert_eq!(cell.outcome, "refused");
        assert!(
            cell.reason
                .as_deref()
                .is_some_and(|r| r.contains("tiles_produced")),
            "the refusal names the field that moved: {:?}",
            cell.reason
        );
    }
}

/// RED against publishing `allocated_bytes`.
///
/// It is `st_blocks * 512`, so it answers to the filesystem's allocator rather
/// than to the engine: the `storage` family's first full capture caught it
/// differing between two repetitions of one generation and the aggregator
/// refused those two cells rather than averaging them. A field that moves for a
/// reason the engine has no part in is not a claim about the engine.
///
/// The fixture makes the point structurally: its three repetitions all report
/// the same `allocated_bytes`, so a producer that DID publish it would be green
/// on agreement and still wrong. What is asserted is that the field is absent
/// from the row and from the flat table, which no amount of agreement can make
/// true.
#[test]
fn allocated_bytes_is_measured_and_not_published_as_an_invariant() {
    let runs = three_reps();
    assert!(
        runs.iter()
            .all(|r| r.artefact.is_some_and(|a| a.allocated_bytes == 1_474_560)),
        "the walk records it, which is the half that is not in question"
    );
    let built = rows(&runs);
    for cell in &built {
        assert_eq!(
            cell.invariants.allocated_bytes, None,
            "a filesystem allocator's answer is not one of this family's invariants"
        );
    }

    let doc = document();
    let names: Vec<&str> = doc["invariants"]
        .as_array()
        .expect("the flat table")
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"tiles_produced")
            && names.contains(&"output_bytes")
            && names.contains(&"filesystem_entries")
            && names.contains(&"directories"),
        "the four that do reproduce are in the table: {names:?}"
    );
    assert!(
        !names.contains(&"allocated_bytes"),
        "and the one that answers to the allocator is not: {names:?}"
    );
}

// ---------------------------------------------------------------------------
// The run id
// ---------------------------------------------------------------------------

/// RED against a `runId` with `SystemTime::now()` in it.
///
/// Archiving the same document twice would then produce two entries, so an
/// idempotent re-archive would silently double a series and the page would draw
/// one flat line as two points. The id is a function of the document's own
/// evidence and of nothing else, so stamping it twice gives the same answer and
/// moving the evidence moves it.
#[test]
fn the_run_id_is_derived_from_the_document_and_never_from_the_clock() {
    let mut doc = Document::new_for(
        engines::FAMILY,
        engines::RUNNER,
        Profile::Ci.label(),
        "2026-09-14T12:00:00.000Z".to_string(),
        engines::measurement(Profile::Ci),
    );
    doc.provenance = Some(serde_json::json!({
        "library": { "commit": "809ee8014d002518ce55edaceba698ca7a8b8a79" },
        "os": "linux",
        "arch": "aarch64",
        "cpuModel": "Apple M2 Pro",
        "node": { "rustc": "rustc 1.97.0" },
        "filesystem": { "fsType": "overlay" },
        "emulated": false,
    }));

    doc.stamp_run_id();
    let first = doc.run_id.clone().expect("the evidence is all there");
    doc.stamp_run_id();
    assert_eq!(
        Some(&first),
        doc.run_id.as_ref(),
        "stamping the same evidence twice has to give the same id, or re-archiving one run \
         files it twice"
    );
    assert!(
        first.starts_with("20260914T120000Z-809ee8014d002518ce55edaceba698ca7a8b8a79-"),
        "the id is the started-at, the measured commit and an environment bucket: {first}"
    );

    // Move one field the bucket is taken over, and the id has to move with it.
    // Two runs on different architectures are not comparable and must not be
    // filed as one.
    doc.provenance.as_mut().unwrap()["arch"] = serde_json::json!("x86_64");
    doc.stamp_run_id();
    assert_ne!(
        doc.run_id.as_ref(),
        Some(&first),
        "an id that survives a change of architecture is not identifying the run"
    );

    // And without the evidence there is no id, rather than an id derived around
    // a hole, which would collide with every other run missing the same field.
    let mut blind = Document::new_for(
        engines::FAMILY,
        engines::RUNNER,
        Profile::Ci.label(),
        "2026-09-14T12:00:00.000Z".to_string(),
        engines::measurement(Profile::Ci),
    );
    blind.stamp_run_id();
    assert_eq!(blind.run_id, None);
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

/// RED against a `ci` profile that is published, and against a `full` profile
/// that measures one repetition.
#[test]
fn only_a_repeated_profile_is_publishable() {
    assert!(!Profile::Ci.publishable());
    assert!(Profile::Full.publishable());
    assert!(Profile::Xl.publishable());
    assert_eq!(Profile::Ci.reps(), 3);
    assert_eq!(Profile::Full.reps(), 7);
    assert_eq!(Profile::Xl.reps(), 7);
    for profile in [Profile::Ci, Profile::Full, Profile::Xl] {
        assert_eq!(
            profile.warmup(),
            1,
            "{} discards a pass before it measures",
            profile.label()
        );
        assert!(
            !profile.cells().is_empty(),
            "{} walks something",
            profile.label()
        );
    }
    // `xl` is `full` plus the two canvases whose monolithic peak needs a
    // container nobody has by default, never a different sweep.
    let full = Profile::Full.canvases();
    let xl = Profile::Xl.canvases();
    assert_eq!(xl[..full.len()], full[..]);
    assert_eq!(xl.len(), full.len() + 2);
}

/// RED against a facet key taken from the engine's own `tiles_produced`.
///
/// The key the chart sorts on has to be a property of the plan. Deriving it
/// from a measurement would make the axis move whenever the measurement did,
/// and `tiles_produced` is one of the invariants this family publishes, so the
/// two would be the same number arriving by two routes that can disagree.
#[test]
fn the_facet_key_is_asked_of_the_planner() {
    let cell = EngineCell::new(1024, 720, 1);
    let planned = cell.planned_tiles().expect("the cell plans");
    assert!(planned > 0);
    let plan = cell.plan().expect("the cell plans");
    let counted: u64 = plan
        .levels
        .iter()
        .map(|l| u64::from(l.rows) * u64::from(l.cols))
        .sum();
    assert_eq!(u64::from(planned), counted);
    assert_eq!(cell.spec(), "1024x720@256+c1");
    assert_ne!(
        EngineCell::new(1024, 720, 1).spec(),
        EngineCell::new(1024, 720, 8).spec(),
        "the thread budget is part of the cell, so a one-thread row and an all-cores row are \
         never the same cell"
    );
}

// ---------------------------------------------------------------------------
// The dirt, which has to travel with every number
// ---------------------------------------------------------------------------

/// RED against the producer as it stood before this lane, in BOTH families.
///
/// `storage::archive` refuses a document whose `provenance.allowDirty` is true
/// and whose cells do not each carry `dirty: true`, because a reader quoting one
/// cell would otherwise not know. Nothing filled the field, so a run with the
/// flag set was refused for `dirty-not-stamped`, which is the exact rule the flag
/// exists to satisfy. I found it by setting the variable and reading the refusal:
/// "provenance.allowDirty is true but 18 of 18 cells do not carry dirty: true".
///
/// The stamp is taken from the provenance and not from the flag, so a run that
/// declares `allowDirty` on a clean tree does not acquire a caveat it has not
/// earned.
#[test]
fn a_dirty_tree_stamps_the_caveat_onto_every_cell_and_a_clean_one_does_not() {
    let build = |harness_dirty: bool, library_dirty: bool| -> Document {
        let mut doc = Document::new_for(
            engines::FAMILY,
            engines::RUNNER,
            Profile::Ci.label(),
            "2026-09-14T12:00:00.000Z".to_string(),
            engines::measurement(Profile::Ci),
        );
        for row in rows(&three_reps()) {
            doc.push(row);
        }
        doc.provenance = Some(serde_json::json!({
            "allowDirty": true,
            "dirty": harness_dirty,
            "library": { "dirty": library_dirty },
        }));
        doc.stamp_dirty_from_provenance();
        doc
    };

    for (harness, library) in [(true, false), (false, true), (true, true)] {
        let doc = build(harness, library);
        assert!(
            doc.cells.iter().all(|c| c.dirty == Some(true)),
            "harness dirty {harness}, library dirty {library}: every cell has to carry the \
             caveat, or the aggregator refuses the run for the rule --allow-dirty exists to \
             satisfy"
        );
    }

    let clean = build(false, false);
    assert!(
        clean.cells.iter().all(|c| c.dirty.is_none()),
        "a clean tree earns no caveat, whatever the flag says"
    );

    // And nothing to read the provenance from leaves it alone rather than
    // guessing.
    let mut unstamped = Document::new_for(
        engines::FAMILY,
        engines::RUNNER,
        Profile::Ci.label(),
        "2026-09-14T12:00:00.000Z".to_string(),
        engines::measurement(Profile::Ci),
    );
    for row in rows(&three_reps()) {
        unstamped.push(row);
    }
    unstamped.stamp_dirty_from_provenance();
    assert!(unstamped.cells.iter().all(|c| c.dirty.is_none()));
}

// ---------------------------------------------------------------------------
// The noise floor
// ---------------------------------------------------------------------------

/// RED against a sweep with no control, and against one that measures the
/// control twice in a row.
///
/// First and last rather than back to back, because what the spread is trying
/// to see is drift across the sweep: thermal, a neighbour waking up, the page
/// cache filling. Two measurements in a row would see none of it and would
/// publish a flatteringly small noise floor.
#[test]
fn a_publishable_profile_opens_and_closes_on_its_control_cell() {
    for profile in [Profile::Full, Profile::Xl] {
        let control = profile
            .replicate_cell()
            .unwrap_or_else(|| panic!("{} has a control", profile.label()));
        let cells = profile.cells();
        assert_eq!(
            cells.first(),
            Some(&control),
            "{} opens on its control",
            profile.label()
        );
        assert_eq!(
            cells.last(),
            Some(&control),
            "{} closes on it",
            profile.label()
        );
        assert_eq!(
            cells.iter().filter(|c| **c == control).count(),
            2,
            "{}: twice, not three times; the control is paid for and the rest of the sweep \
             must not measure it a third time in its natural position",
            profile.label()
        );
        assert!(
            cells.len() > 3,
            "{} has a sweep between the two ends",
            profile.label()
        );
    }

    // `ci` is never published, so it has no noise floor to publish either, and
    // measuring its one cell twice would double the smoke test to say nothing.
    assert_eq!(Profile::Ci.replicate_cell(), None);
    assert_eq!(Profile::Ci.cells().len(), 1);
}

/// RED against a spread computed from the wrong pair.
///
/// Two `engines` cells can share a tile count and a source and differ only in
/// their thread budget, so the block has to find its two measurements by the
/// control's own cell key. A filter on `(scale, source)`, which is what the
/// storage family used before this lane, would pick up the other thread budget
/// and publish a spread between two different measurements as drift.
#[test]
fn the_replicate_block_is_the_spread_between_the_two_ends_of_the_sweep() {
    use libviprs_bench::storage::scenarios::replicate::block_for_cell;

    let control = EngineCell::new(1024, 720, 1);
    // Same canvas, same source, same tile count, different thread budget. This
    // is the row the spread must not be computed against.
    let decoy = EngineCell::new(1024, 720, 8);
    assert_eq!(control.planned_tiles(), decoy.planned_tiles());

    let mut doc = Document::new_for(
        engines::FAMILY,
        engines::RUNNER,
        Profile::Full.label(),
        "2026-09-14T12:00:00.000Z".to_string(),
        engines::measurement(Profile::Full),
    );
    let push = |doc: &mut Document, cell: EngineCell, wall_ms: u64| {
        let runs = vec![
            run(Engine::Monolithic, wall_ms, 9_700_000, 25),
            run(Engine::Monolithic, wall_ms, 9_700_000, 25),
            run(Engine::Monolithic, wall_ms, 9_700_000, 25),
        ];
        for row in engines::rows_for(
            cell,
            Engine::Monolithic,
            &runs,
            &[],
            Profile::Full,
            MachineLoad::unknown(),
            None,
        ) {
            doc.push(row);
        }
    };
    push(&mut doc, control, 500);
    push(&mut doc, decoy, 9_000);
    push(&mut doc, control, 550);

    let block = block_for_cell(&doc, &control.spec()).expect("two ends make a block");
    assert_eq!(block.cell, control.spec());
    assert_eq!(block.replicate_reps, 2);
    let spread = block.spread_pct.as_object().expect("a spread object");
    let wall = spread
        .get("monolithic.pyramid.wall")
        .and_then(|v| v.as_f64())
        .expect("the wall column has a spread");
    // 500 and 550, as a percentage of the smaller.
    assert!(
        (wall - 10.0).abs() < 1e-9,
        "the spread is between the two ends, not against the cell in the middle: {wall}"
    );

    // And a control measured once has no spread, rather than a spread of zero,
    // which would read as a perfectly quiet host.
    let mut once = Document::new_for(
        engines::FAMILY,
        engines::RUNNER,
        Profile::Full.label(),
        "2026-09-14T12:00:00.000Z".to_string(),
        engines::measurement(Profile::Full),
    );
    push(&mut once, control, 500);
    assert_eq!(block_for_cell(&once, &control.spec()), None);
}
