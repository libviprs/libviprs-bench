//! The `storage` document: what it can say, and what it must not be able to
//! say.
//!
//! Every test here names, in a comment, the wrong implementation it goes red
//! against. A test that stays green under that mutation is not the test.

use libviprs_bench::storage::cells::{Backend, Cell, Profile, SEED, Source};
use libviprs_bench::storage::document::{
    CELL_FIELDS, CellLabels, CellReport, DOCUMENT_FIELDS, Document, DocumentCell, FAMILY,
    InvariantBlock, MachineLoad, SCHEMA_VERSION,
};
use libviprs_bench::storage::scenarios::{
    Direction, Invariants, Isolation, MetricSpec, Outcome, RepFacts, Unit, Warmup,
};
use libviprs_bench::storage::{agreed, stats};
use serde_json::Value;

fn cell() -> Cell {
    Cell::new(2048, 2048, 256, Source::Gradient, 93)
}

const P50: MetricSpec = MetricSpec {
    name: "p50",
    unit: Unit::Microseconds,
    direction: Direction::LowerIsBetter,
};

/// A row built from `samples`, with nothing measured that was not measured.
fn row(samples: Vec<f64>, invariants: InvariantBlock) -> DocumentCell {
    let reps = samples.len().max(1) as u32;
    DocumentCell::from_report(CellReport {
        labels: CellLabels::storage(Backend::PmTiles, cell()),
        scenario: "read_random",
        metric: P50,
        isolation: Isolation::ProcessPerScenario,
        oversubscribed: None,
        warmup: Some(Warmup::ONE_DISCARDED_PASS),
        discarded_warmup: vec![41.0],
        reps_declared: reps,
        min_reps: reps,
        samples,
        outcome: Outcome::Ok,
        reason: None,
        invariants,
        machine_load: MachineLoad::unknown(),
        timer: None,
    })
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

// ---------------------------------------------------------------------------

/// RED against a single-shot cell that copies its one value into `median`,
/// which is the shape the harness in the engine repository publishes today:
/// one process per cell, once, and a field called `median` holding it.
#[test]
fn a_cell_carries_reps_samples_and_a_median_of_them() {
    let samples = vec![120.0, 118.5, 131.0, 119.25, 122.0, 140.5, 117.0];
    let reps = samples.len();
    let built = row(samples.clone(), InvariantBlock::default());

    assert_eq!(
        built.samples.len(),
        reps,
        "a cell carries one sample per timed repetition, not a single value"
    );
    assert_eq!(built.samples, samples, "in the order they ran");
    assert_eq!(built.reps as usize, reps);

    let median = stats::median(&samples).expect("a median of seven samples");
    assert_eq!(
        built.median,
        Some(median),
        "the field called median holds the median of the samples beside it"
    );

    let ci = built.ci95.expect("a cell carries an interval");
    assert!(
        ci[0] <= median && median <= ci[1],
        "the bootstrap interval {ci:?} should contain the median {median}"
    );
    assert!(built.iqr.is_some(), "and its spread");
    assert!(built.cov.is_some());

    // The control that stops this passing by accident: one sample is allowed,
    // it just cannot pretend to be seven. Its interval collapses onto itself.
    let single = row(vec![120.0], InvariantBlock::default());
    assert_eq!(single.samples.len(), 1);
    assert_eq!(single.median, Some(120.0));
    assert_eq!(single.ci95, Some([120.0, 120.0]));
}

/// RED against a zero-filled row: `InvariantBlock` folding a `None` through
/// `unwrap_or(0)`.
///
/// A real `filesystem_entries: 0` is exactly the failure this rule exists to
/// expose. Lower is better on that column, an archive really costs `1`, so a
/// broken measurement that published a zero published the best possible score
/// on the one column this whole comparison is about.
#[test]
fn unmeasured_columns_are_null_never_zero() {
    // Built the way the runner builds it, out of what a scenario observed,
    // rather than out of a default block. A `From` that folded a `None`
    // through `unwrap_or(0)` is invisible to a test that never calls it, and
    // the mutation table caught exactly that: this test was green under the
    // zero-filling mutation until it went through this conversion.
    let nothing_measured = InvariantBlock::from(&Invariants::default());
    let built = row(vec![10.0, 11.0, 12.0], nothing_measured);
    let json = parse(&serde_json::to_string(&built).expect("a row serialises"));
    let invariants = &json["invariants"];

    for field in [
        "outputBytes",
        "allocatedBytes",
        "filesystemEntries",
        "directories",
        "tilesProduced",
        "artefactDigest",
        "rootEntries",
        "leaves",
        "requests",
        "requestBytes",
        "peakRssMb",
        "heapPeakBytes",
    ] {
        assert!(
            invariants.get(field).is_some(),
            "{field} is absent; a missing key and a null are different claims"
        );
        assert!(
            invariants[field].is_null(),
            "{field} was not measured and reads {}, not null",
            invariants[field]
        );
    }

    // A cell with no samples has no statistics either, and it says so rather
    // than publishing a median of zero, which is the fastest possible cell.
    let empty = row(Vec::new(), InvariantBlock::from(&Invariants::default()));
    let json = parse(&serde_json::to_string(&empty).expect("a row serialises"));
    for field in [
        "median",
        "min",
        "max",
        "iqr",
        "cov",
        "ci95",
        "p95OfSamples",
        "tail",
    ] {
        assert!(
            json[field].is_null(),
            "{field} of a cell with no samples reads {}, not null",
            json[field]
        );
    }

    // The positive control. Without it a producer that nulled everything
    // unconditionally would pass: a measured zero has to survive as a zero,
    // because a zero-byte artefact is a real observation and a hole is not.
    let measured = row(
        vec![10.0],
        InvariantBlock::from(&Invariants {
            filesystem_entries: Some(0),
            output_bytes: Some(0),
            ..Invariants::default()
        }),
    );
    let json = parse(&serde_json::to_string(&measured).expect("a row serialises"));
    assert_eq!(json["invariants"]["filesystemEntries"], Value::from(0u64));
    assert_eq!(json["invariants"]["outputBytes"], Value::from(0u64));
}

/// RED against a `BTreeMap` alphabetising the keys, which is what
/// `serde_json::Map` is without the `preserve_order` feature.
///
/// Field order is the producer's statement about what the document is: the
/// identity first, then the declared measurement, then the data. A consumer
/// that reads it back through an alphabetising map cannot see that order, and
/// cannot round trip the file either.
#[test]
fn the_document_round_trips_with_field_order() {
    let mut doc = Document::new(Profile::Ci, "2026-09-13T00:00:00.000Z".to_string());
    doc.push(row(vec![10.0, 11.0, 12.0], InvariantBlock::default()));
    doc.rebuild_invariant_table();
    doc.finished_at = Some("2026-09-13T00:01:00.000Z".to_string());

    let text = doc.to_json();
    let parsed = parse(&text);

    assert_eq!(
        keys(&parsed),
        DOCUMENT_FIELDS.to_vec(),
        "the document's keys came back in another order"
    );
    assert_eq!(
        keys(&parsed["cells"][0]),
        CELL_FIELDS.to_vec(),
        "a cell's keys came back in another order"
    );

    // `schemaVersion` before `cells` is alphabetically backwards, which is the
    // whole point: an alphabetising map cannot produce this order by accident.
    let at = |k: &str| DOCUMENT_FIELDS.iter().position(|f| *f == k).unwrap();
    assert!(at("schemaVersion") < at("cells"));
    assert!(at("measurement") < at("cells"));

    // The order survives a second trip, and the re-serialisation is a fixed
    // point: parse it again and you get the same text back, byte for byte.
    let text2 = serde_json::to_string_pretty(&parsed).expect("the parsed document re-serialises");
    let parsed2 = parse(&text2);
    assert_eq!(keys(&parsed2), DOCUMENT_FIELDS.to_vec());
    assert_eq!(keys(&parsed2["cells"][0]), CELL_FIELDS.to_vec());
    assert_eq!(
        serde_json::to_string_pretty(&parsed2).expect("re-serialises"),
        text2,
        "the re-serialisation is not a fixed point"
    );

    // Through its own type, not only through `Value`. Every field survives
    // exactly, floats included.
    //
    // The floats used to be checked with a tolerance, because `serde_json`'s
    // number reader is not correctly rounded by default and `cov` here is
    // `1.0 / 11.0`, which needs seventeen significant digits and came back one
    // ULP out. `Cargo.toml` now turns on serde_json's `float_roundtrip`, which
    // closes that, so this is byte identity as the test that pinned the defect
    // said it should become once the defect was gone. The canary for the
    // feature is `the_json_reader_is_correctly_rounded` in
    // `tests/storage_provenance_k13.rs`: take the feature away and it goes red
    // in one line, and this assertion goes red with it.
    let back: Document = serde_json::from_str(&text).expect("the document deserialises");
    assert_eq!(back.schema_version, doc.schema_version);
    assert_eq!(back.family, doc.family);
    assert_eq!(back.profile, doc.profile);
    assert_eq!(back.started_at, doc.started_at);
    assert_eq!(back.finished_at, doc.finished_at);
    assert_eq!(back.measurement.unit, doc.measurement.unit);
    assert_eq!(back.measurement.reps, doc.measurement.reps);
    assert_eq!(back.measurement.warmup, doc.measurement.warmup);
    assert_eq!(back.cells.len(), doc.cells.len());
    let (there, here) = (&back.cells[0], &doc.cells[0]);
    assert_eq!(there.key, here.key);
    assert_eq!(there.samples, here.samples);
    assert_eq!(there.median, here.median);
    assert_eq!(there.ci95, here.ci95);
    assert_eq!(there.tail, here.tail);
    assert_eq!(there.warmup, here.warmup);
    assert_eq!(there.discarded_warmup, here.discarded_warmup);
    assert_eq!(there.invariants, here.invariants);
    assert_eq!(there.low_confidence_reasons, here.low_confidence_reasons);
    let (a, b) = (there.cov.expect("a cov"), here.cov.expect("a cov"));
    assert_eq!(
        a.to_bits(),
        b.to_bits(),
        "cov moved from {b} to {a}, which with a correctly rounded reader means \
         something other than the reader changed it"
    );
}

/// RED against a nearest-rank p99 at n = 64, which is the current shape and is
/// the maximum wearing a percentile's name.
///
/// `sorted[ceil(64 * 0.99) - 1]` is `sorted[63]`, the largest sample. The two
/// do not behave alike: across the free replicate pair in the committed
/// exports, p99 moved 74.5% where p50 moved 7.1%, on an idle host running
/// identical code.
#[test]
fn under_a_hundred_samples_the_row_publishes_max_not_p99() {
    let mut samples: Vec<f64> = (0..63).map(|i| 100.0 + f64::from(i) * 0.1).collect();
    samples.push(9_000.0); // one scheduling accident
    assert_eq!(samples.len(), 64);

    let tail = stats::tail(&samples).expect("a tail statistic");
    assert_eq!(
        tail.kind.as_str(),
        "max",
        "64 samples cannot support a 99th percentile"
    );
    assert_eq!(tail.value, 9_000.0, "and the statistic is the maximum");

    let built = row(samples.clone(), InvariantBlock::default());
    let json = parse(&serde_json::to_string(&built).expect("a row serialises"));
    assert_eq!(
        json["tail"]["statistic"], "max",
        "the document has to publish the name the statistic earned"
    );
    assert_eq!(json["tail"]["value"], 9_000.0);

    // The positive control. At 200 samples there is a 99th percentile to
    // estimate, it is named `p99`, and it is strictly below the maximum, so an
    // implementation that always answered `max` fails here.
    let mut many: Vec<f64> = (0..199).map(|i| 100.0 + f64::from(i) * 0.1).collect();
    many.push(9_000.0);
    let tail = stats::tail(&many).expect("a tail statistic");
    assert_eq!(tail.kind.as_str(), "p99");
    assert!(
        tail.value < 9_000.0,
        "a p99 of 200 samples is not the maximum, got {}",
        tail.value
    );
    assert!(tail.value > 100.0);
}

/// RED against a bare unversioned array, which is what a benchmark writes
/// right up until the first time a column changes meaning and nothing
/// downstream can say so.
#[test]
fn the_document_names_its_family_and_version() {
    let doc = Document::new(Profile::Full, "2026-09-13T00:00:00.000Z".to_string());
    let parsed = parse(&doc.to_json());

    assert!(
        parsed.is_object(),
        "the document is an envelope, not an array of rows"
    );
    assert_eq!(parsed["schemaVersion"], Value::from(SCHEMA_VERSION));
    assert_eq!(parsed["family"], FAMILY);
    assert_eq!(parsed["runner"], "libviprs-storage");
    assert_eq!(parsed["profile"], "full");

    // The version is numbered per family, so the family name has to travel
    // next to it: causl's schema 1 and this schema 1 are different documents.
    assert_ne!(FAMILY, "causl");
    assert!(parsed["cells"].is_array());
    assert!(parsed["invariants"].is_array());
    assert!(parsed["modelled"].is_array());

    // The slots the other lanes fill are present and null, not absent.
    assert!(parsed.get("provenance").is_some() && parsed["provenance"].is_null());
    assert!(parsed.get("integrity").is_some() && parsed["integrity"].is_null());
    assert!(parsed.get("runId").is_some() && parsed["runId"].is_null());
}

/// RED against a fold that averages, or takes the first, or takes the last.
///
/// An invariant that changes between two repetitions of one commit is a
/// defect, never noise. Averaging two disagreeing entry counts produces a
/// number no run ever observed, and it looks exactly like a measurement.
#[test]
fn an_invariant_that_disagrees_between_reps_is_refused_not_averaged() {
    let with = |entries: u64, digest: &str| RepFacts {
        invariants: Invariants {
            filesystem_entries: Some(entries),
            artefact_digest: Some(digest.to_string()),
            tiles_produced: Some(93),
            ..Invariants::default()
        },
        scratch: None,
    };

    let (agreed_ok, none) = agreed(&[with(1, "aa"), with(1, "aa"), with(1, "aa")]);
    assert!(
        none.is_empty(),
        "three agreeing reps disagree about nothing"
    );
    assert_eq!(agreed_ok.filesystem_entries, Some(1));
    assert_eq!(agreed_ok.artefact_digest.as_deref(), Some("aa"));

    let (folded, disagreements) = agreed(&[with(1, "aa"), with(2, "aa"), with(1, "aa")]);
    assert_eq!(
        folded.filesystem_entries, None,
        "a disagreeing invariant is a hole, not an average and not a vote"
    );
    assert!(
        disagreements
            .iter()
            .any(|d| d.contains("filesystem_entries")),
        "the reason has to name the field that moved, got {disagreements:?}"
    );
    assert_eq!(
        folded.artefact_digest.as_deref(),
        Some("aa"),
        "and the fields that did agree still carry their value"
    );
    assert_eq!(folded.tiles_produced, Some(93));
}

/// RED against a facet key taken from the canvas or the megapixel count.
///
/// The archive's directory shape follows from how many entries the plan has,
/// so the tile count is the regime and the canvas is not. `8192x8192@64` and
/// `8192x8192@256` are the same 67.1 megapixels and are two orders of
/// magnitude apart in tiles, which is why a chart ordered by pixels puts them
/// in the wrong places and a chart ordered by tiles does not.
#[test]
fn the_facet_key_is_the_planned_tile_count_not_the_canvas() {
    for cell in Profile::Xl.cells() {
        let planned = cell
            .planned_tiles()
            .unwrap_or_else(|| panic!("{} plans", cell.spec()));
        assert_eq!(
            cell.declared_tiles as usize,
            planned,
            "{} is declared as {} tiles and the planner lays out {planned}",
            cell.spec(),
            cell.declared_tiles
        );
    }

    let coarse = Cell::new(8192, 8192, 256, Source::Gradient, 0);
    let fine = Cell::new(8192, 8192, 64, Source::Gradient, 0);
    let megapixels = |c: &Cell| f64::from(c.width) * f64::from(c.height) / 1_000_000.0;
    assert_eq!(
        megapixels(&coarse),
        megapixels(&fine),
        "the two cells are the same canvas, so pixels cannot tell them apart"
    );
    let coarse_tiles = coarse.planned_tiles().expect("a plan");
    let fine_tiles = fine.planned_tiles().expect("a plan");
    assert!(
        fine_tiles > coarse_tiles * 10,
        "the tile count does tell them apart: {coarse_tiles} against {fine_tiles}"
    );

    // A cell parsed from its wire form asks the planner rather than trusting
    // an argv string, so a parent and its child cannot disagree by a typo.
    let parsed = Cell::parse("8192x8192@64+gradient").expect("a cell parses");
    assert_eq!(parsed.declared_tiles as usize, fine_tiles);
    assert_eq!(parsed.spec(), "8192x8192@64+gradient");
    assert_eq!(SEED, 0x5EED_1234_ABCD_0001);
}

/// A document that never ran a sweep says so, and says it in the one way the
/// importer can tell apart from a document that predates the field.
///
/// The three states are the point. Absent is a run from before #100 and keeps
/// the majority-of-noisy-cells rule. Null is a runner that knows about the field
/// and skipped its own first act, and the importer refuses it. A `MachineLoad`
/// with nothing readable in it is "I looked and this platform would not say",
/// which is an empty reading and refused as one.
///
/// Goes red against: a constructor that calls `MachineLoad::sample()` itself, so
/// every hand-built document claims a reading it never took. And against
/// `skip_serializing_if`, which would drop the key and make an unsampled
/// document indistinguishable from a 2026-09-14 one.
#[test]
fn a_document_that_never_ran_claims_no_starting_load() {
    let doc = Document::new(Profile::Full, "2026-09-15T00:00:00.000Z".to_string());
    assert!(
        doc.starting_load.is_none(),
        "the constructor filled a reading nothing took"
    );

    let parsed = parse(&doc.to_json());
    assert!(
        keys(&parsed).iter().any(|k| k == "startingLoad"),
        "the key must be emitted even when empty: dropping it makes an unsampled document \
         look like one written before the field existed, and those two are judged by \
         different rules"
    );
    assert!(
        parsed["startingLoad"].is_null(),
        "an unfilled starting load is null, got {}",
        parsed["startingLoad"]
    );

    // And it sits where the envelope says it sits: after the run's identity,
    // before how the run measured. A reader meets what the machine was doing
    // before they meet the protocol.
    let at = |k: &str| DOCUMENT_FIELDS.iter().position(|f| *f == k).unwrap();
    assert!(at("runId") < at("startingLoad"));
    assert!(at("startingLoad") < at("measurement"));
    assert!(at("startingLoad") < at("cells"));
}
