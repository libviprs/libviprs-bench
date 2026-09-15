//! The replicate control's own dispersion (libviprs-bench #84).
//!
//! Every test here names, in a comment above it, the wrong implementation it
//! goes red against. The wrong implementation is mostly the same one: the
//! estimator this file replaces, which measured the control cell twice and
//! published the gap between the two as the run's noise floor.
//!
//! Three full storage captures of the same cell on attested native hosts, none
//! carrying a condition warning, reported floors of 3.46%, 36.89% and 21.31%.
//! None of them was wrong. A gap between two points has no dispersion of its
//! own, so nothing in any of the three documents could say which was the
//! outlier, and the runs were incomparable in a way nothing flagged.

use std::collections::BTreeMap;

use libviprs_bench::storage::cells::{self, Backend, Cell, Profile, Source};
use libviprs_bench::storage::document::{
    CellLabels, CellReport, Document, DocumentCell, InvariantBlock, MachineLoad, Replicate,
};
use libviprs_bench::storage::scenarios::replicate::{
    self, COVERAGE, ESTIMATOR, MIN_REPLICATE_REPS,
};
use libviprs_bench::storage::scenarios::{Direction, Isolation, MetricSpec, Outcome, Unit, Warmup};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn control() -> Cell {
    Cell::new(2048, 2048, 256, Source::Gradient, 93)
}

const LOOKUPS: MetricSpec = MetricSpec {
    name: "lookups_per_s",
    unit: Unit::PerSecond,
    direction: Direction::HigherIsBetter,
};

/// One row of the control cell, carrying `samples` for one placement.
fn placement_row(cell: Cell, scenario: &'static str, samples: Vec<f64>) -> DocumentCell {
    let reps = samples.len().max(1) as u32;
    DocumentCell::from_report(CellReport {
        labels: CellLabels::storage(Backend::PmTiles, cell),
        scenario,
        metric: LOOKUPS,
        isolation: Isolation::ProcessPerScenario,
        oversubscribed: None,
        warmup: Some(Warmup::ONE_DISCARDED_PASS),
        discarded_warmup: vec![],
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

/// A document whose control cell was placed once per value in `medians`.
///
/// One sample per row, so the row's median is exactly the value handed in and
/// the test is about the estimator rather than about the summary statistics.
fn doc_with_placements(medians: &[f64]) -> Document {
    let mut doc = Document::new(Profile::Full, "2026-09-15T00:00:00.000Z".to_string());
    for median in medians {
        doc.push(placement_row(control(), "read_concurrent@4", vec![*median]));
    }
    doc
}

/// The floor, drift and residual the document publishes for the one metric.
fn published(block: &Replicate) -> (f64, f64, f64) {
    let key = "pmtiles.read_concurrent@4.lookups_per_s";
    let pick = |value: &serde_json::Value| {
        value
            .get(key)
            .and_then(serde_json::Value::as_f64)
            .unwrap_or_else(|| panic!("the document publishes {key}: {value}"))
    };
    (
        pick(&block.spread_pct),
        pick(&block.drift_pct),
        pick(&block.residual_pct),
    )
}

/// A measurement map for [`replicate::block`].
fn measurement(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), *value))
        .collect()
}

/// splitmix64, so a simulation in a test is the same simulation on every host.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f64 {
        // (0, 1], never 0, because the log below would take it to infinity.
        ((self.next_u64() >> 11) as f64 + 1.0) / ((1u64 << 53) as f64 + 1.0)
    }

    /// Box-Muller, one draw per call. Two uniforms per normal is not the fast
    /// way and does not need to be.
    fn normal(&mut self) -> f64 {
        let (u1, u2) = (self.unit(), self.unit());
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    fn placements(&mut self, n: usize, sigma: f64) -> Vec<f64> {
        (0..n).map(|_| 100.0 + sigma * self.normal()).collect()
    }
}

/// How often two independent captures of one quiet host disagree by more than
/// `factor`, for a floor resting on `n` placements.
fn disagreement(rng: &mut Rng, n: usize, factor: f64, trials: usize) -> f64 {
    let mut over = 0usize;
    for _ in 0..trials {
        let a = replicate::dispersion(&rng.placements(n, 5.0)).map(|d| d.floor_pct);
        let b = replicate::dispersion(&rng.placements(n, 5.0)).map(|d| d.floor_pct);
        let (Some(a), Some(b)) = (a, b) else { continue };
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        if lo <= 0.0 || hi / lo > factor {
            over += 1;
        }
    }
    over as f64 / trials as f64
}

/// The same question for the estimator this file replaces: two measurements,
/// and the gap between them as a percentage of the smaller.
fn two_point_disagreement(rng: &mut Rng, factor: f64, trials: usize) -> f64 {
    let mut over = 0usize;
    for _ in 0..trials {
        let gap = |rng: &mut Rng| {
            let pair = rng.placements(2, 5.0);
            replicate::spread_pct(pair[0], pair[1])
        };
        let first = gap(rng);
        let second = gap(rng);
        let (Some(a), Some(b)) = (first, second) else {
            continue;
        };
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        if lo <= 0.0 || hi / lo > factor {
            over += 1;
        }
    }
    over as f64 / trials as f64
}

// ---------------------------------------------------------------------------
// The schedule
// ---------------------------------------------------------------------------

/// RED against the schedule this replaces, which put the control at the two
/// ends and nowhere else: two placements whatever the sweep's length, so the
/// floor could never have dispersion of its own however long the run got.
#[test]
fn the_control_is_placed_once_per_measured_cell_and_never_twice_in_a_row() {
    let rest = Profile::Full.measured_cells();
    let walked = Profile::Full.cells();

    assert_eq!(
        replicate::placements(&walked, control()),
        rest.len() + 1,
        "the full sweep walks {} measured cells, so it holds {} placements",
        rest.len(),
        rest.len() + 1
    );
    assert_eq!(replicate::placements(&walked, control()), 6);
    assert!(
        replicate::placements(&walked, control()) >= MIN_REPLICATE_REPS,
        "a sweep that cannot hold {MIN_REPLICATE_REPS} placements publishes no floor at all"
    );
    assert!(
        !replicate::has_adjacent_placements(&walked, control()),
        "two placements back to back see none of the drift across the sweep, so they raise \
         the count without widening the window"
    );
    assert!(replicate::measured_first_and_last(&walked, control()));
    // The measured cells keep their order and none of them is lost.
    let measured: Vec<Cell> = walked.iter().copied().filter(|c| *c != control()).collect();
    assert_eq!(measured, rest);

    // xl adds one measured cell, so it holds one more placement. The count is
    // derived from the schedule, never typed beside it.
    let xl = Profile::Xl.cells();
    assert_eq!(
        replicate::placements(&xl, control()),
        Profile::Xl.measured_cells().len() + 1
    );
    assert_eq!(replicate::placements(&xl, control()), 7);
    assert!(!replicate::has_adjacent_placements(&xl, control()));

    // ci has no control and gets no placements, which is why it publishes no
    // floor.
    assert_eq!(Profile::Ci.measured_cells().len(), 0);
    assert!(cells::replicate_cell(Profile::Ci).is_none());
}

/// RED against an `engines` schedule that kept the two-ended arrangement while
/// `storage` moved, which would have left one family's floor on two points and
/// the other's on six with nothing saying so.
#[test]
fn the_engines_sweep_places_its_control_the_same_way() {
    use libviprs_bench::engines::cells::Profile as EngineProfile;

    let walked = EngineProfile::Full.cells();
    let control = EngineProfile::Full
        .replicate_cell()
        .expect("the full engines profile has a control");
    let measured = walked.iter().filter(|c| **c != control).count();

    assert_eq!(replicate::placements(&walked, control), measured + 1);
    assert!(
        replicate::placements(&walked, control) >= MIN_REPLICATE_REPS,
        "the engines sweep walks {measured} measured cells and cannot publish a floor from \
         fewer than {MIN_REPLICATE_REPS} placements"
    );
    assert!(!replicate::has_adjacent_placements(&walked, control));
    assert!(replicate::measured_first_and_last(&walked, control));
}

// ---------------------------------------------------------------------------
// The estimator
// ---------------------------------------------------------------------------

/// RED against the first-versus-last estimator. It reads these two orderings of
/// one set of measurements as 40% and 0%, which is the defect stated as
/// arithmetic: the floor depended on where in the sweep the excursion landed
/// rather than on how big it was.
#[test]
fn the_floor_is_the_same_number_whatever_order_the_placements_arrived_in() {
    let at_the_end = [100.0, 100.0, 100.0, 100.0, 140.0];
    let in_the_middle = [100.0, 140.0, 100.0, 100.0, 100.0];

    // The estimator this replaces, spelled out so the contrast is a number and
    // not a claim.
    assert_eq!(
        replicate::spread_pct(at_the_end[0], at_the_end[4]),
        Some(40.0)
    );
    assert_eq!(
        replicate::spread_pct(in_the_middle[0], in_the_middle[4]),
        Some(0.0)
    );

    let a = replicate::dispersion(&at_the_end).expect("five placements are a floor");
    let b = replicate::dispersion(&in_the_middle).expect("five placements are a floor");
    assert!(
        (a.floor_pct - b.floor_pct).abs() < 1e-9,
        "the floor moved with the order: {} against {}",
        a.floor_pct,
        b.floor_pct
    );
    assert!(
        a.floor_pct > 40.0,
        "one placement 40% off four others is not a 40% floor, it is wider: {}",
        a.floor_pct
    );
    // And the trend is the half that does depend on the order.
    assert!(a.drift_pct > 20.0, "a late excursion is a rising trend");
    assert!(
        b.drift_pct.abs() < a.drift_pct.abs(),
        "an excursion in the middle is not a trend"
    );
}

/// RED against a block that publishes one number. The noisier of the two
/// captures is a host shedding load through the sweep, not a host with wide
/// scatter, and a single figure cannot say which. These are its real endpoints:
/// `directory.read_concurrent@4.lookups_per_s` opened the sweep at 198 700
/// lookups a second and closed it at 573 600.
#[test]
fn a_sweep_that_drifts_publishes_the_drift_beside_the_floor() {
    let n = 6;
    let (first, last) = (198_700.0_f64, 573_600.0_f64);
    let ramp: Vec<f64> = (0..n)
        .map(|i| first + (last - first) * i as f64 / (n as f64 - 1.0))
        .collect();

    let found = replicate::dispersion(&ramp).expect("six placements are a floor");
    // A straight ramp has no scatter left once the trend is out, and the drift
    // is the whole first-to-last change as a percent of the centre.
    let centre = (ramp[2] + ramp[3]) / 2.0;
    let expected = (last - first) / centre * 100.0;
    assert!(
        (found.drift_pct - expected).abs() < 0.5,
        "the published drift is {} and the run really moved {expected}%",
        found.drift_pct
    );
    assert!(found.drift_pct > 90.0);
    assert!(
        found.residual_pct < 1.0,
        "a straight line has no scatter around itself, and this says {}",
        found.residual_pct
    );
    assert!(
        found.floor_pct > 50.0,
        "the floor still covers the drift, because a run that drifted cannot grade a tight \
         delta: {}",
        found.floor_pct
    );

    // The mirror case: the same two values with no trend between them. The
    // arrangement is symmetric about the middle of the sweep, so the fitted
    // slope is exactly zero and every bit of the movement is scatter. A block
    // that published one number could not tell this run from the ramp above.
    let scattered = [first, last, first, first, last, first];
    let quiet = replicate::dispersion(&scattered).expect("six placements are a floor");
    assert!(
        quiet.drift_pct.abs() < 1e-9,
        "a symmetric arrangement has no trend, and this reads one of {}",
        quiet.drift_pct
    );
    assert!(
        quiet.residual_pct > 50.0,
        "with no trend to remove, the scatter is everything: {}",
        quiet.residual_pct
    );
    assert!(
        quiet.residual_pct > found.residual_pct * 10.0,
        "the ramp and the zigzag move by the same amount, and the residual is what \
         separates them: {} against {}",
        quiet.residual_pct,
        found.residual_pct
    );
}

/// RED against a fixed 1.96 multiplier. The whole point of publishing the
/// estimator is that a floor resting on few points is wider, and a constant
/// hides exactly that.
#[test]
fn the_floor_widens_when_it_rests_on_fewer_points() {
    assert_eq!(replicate::prediction_multiplier(1), None);
    let two = replicate::prediction_multiplier(2).expect("two points have a multiplier");
    assert!(
        two > 15.0,
        "two points bound nothing and the arithmetic should say so: {two}"
    );

    let mut previous = two;
    for n in 3..=64 {
        let k = replicate::prediction_multiplier(n).expect("a multiplier");
        assert!(
            k < previous,
            "the multiplier did not fall from {n} - 1 to {n}"
        );
        assert!(
            k > 1.959,
            "it must never fall below the normal quantile: {k}"
        );
        previous = k;
    }
    // The two counts this suite actually publishes.
    let six = replicate::prediction_multiplier(6).expect("a multiplier");
    assert!((six - 2.777).abs() < 0.01, "six placements give {six}");
    let sixteen = replicate::prediction_multiplier(16).expect("a multiplier");
    assert!((sixteen - 2.194).abs() < 0.01, "sixteen give {sixteen}");
}

/// RED against using the series expansion everywhere. It is 4.33 at one degree
/// of freedom where the true value is 12.706, so a two-point floor would come
/// out three times narrower than it is.
#[test]
fn the_t_table_and_its_continuation_agree_at_the_seam() {
    assert_eq!(replicate::t_975(0), None);
    assert_eq!(replicate::t_975(1), Some(12.706));
    let last_tabulated = replicate::t_975(30).expect("tabulated");
    let first_computed = replicate::t_975(31).expect("computed");
    assert!(
        (last_tabulated - first_computed).abs() / last_tabulated < 0.005,
        "the table and its continuation disagree at the seam: {last_tabulated} then \
         {first_computed}"
    );
    // And the continuation really converges.
    assert!((replicate::t_975(100_000).expect("computed") - 1.96).abs() < 0.001);
}

/// RED against `MIN_REPLICATE_REPS = 2`, and against any estimator built on a
/// pair. This is the number the change rests on, so it is measured rather than
/// asserted: two independent floors of one quiet host, and how often they
/// disagree.
///
/// The two-point row is the control. It has to fail the same check, or the
/// check cannot tell a good estimator from a bad one.
#[test]
fn two_floors_of_one_host_agree_far_more_often_than_two_points_did() {
    let trials = 20_000;
    let mut rng = Rng(0x5eed_1234_abcd_0084);

    let two_point_10x = two_point_disagreement(&mut rng, 10.0, trials);
    let two_point_3x = two_point_disagreement(&mut rng, 3.0, trials);
    assert!(
        two_point_10x > 0.10,
        "the estimator this replaces disagreed with itself by 10x on {:.1}% of pairs, and the \
         capture that opened this issue was one of them; if this control passes, the simulation \
         is not exercising the failure",
        two_point_10x * 100.0
    );
    assert!(two_point_3x > 0.30);

    let n = replicate::placements(&Profile::Full.cells(), control());
    let ten_x = disagreement(&mut rng, n, 10.0, trials);
    let three_x = disagreement(&mut rng, n, 3.0, trials);
    assert!(
        ten_x < 0.005,
        "a floor from {n} placements still disagrees with itself by 10x on {:.2}% of pairs",
        ten_x * 100.0
    );
    assert!(
        three_x < 0.10,
        "a floor from {n} placements disagrees by 3x on {:.1}% of pairs",
        three_x * 100.0
    );
    assert!(
        ten_x * 20.0 < two_point_10x,
        "the new estimator must be at least an order of magnitude steadier, and it is \
         {ten_x} against {two_point_10x}"
    );
}

// ---------------------------------------------------------------------------
// The block
// ---------------------------------------------------------------------------

/// RED against a block built from fewer placements than it can describe. Two
/// points publish a gap with no dispersion of its own; four leave two degrees
/// of freedom once a trend is fitted, which is not enough to call the leftover
/// anything.
#[test]
fn a_floor_resting_on_too_few_placements_is_refused_rather_than_published() {
    assert_eq!(MIN_REPLICATE_REPS, 5);
    for n in 0..MIN_REPLICATE_REPS {
        let values: Vec<f64> = (0..n).map(|i| 100.0 + i as f64).collect();
        assert!(
            replicate::dispersion(&values).is_none(),
            "{n} placements produced a floor"
        );
    }
    assert!(replicate::dispersion(&[100.0, 101.0, 102.0, 103.0, 104.0]).is_some());

    let two = [measurement(&[("p50", 8.0)]), measurement(&[("p50", 8.4)])];
    let refusal = replicate::block(&control(), &two)
        .expect_err("a floor over two placements is not a dispersion");
    assert!(refusal.contains(&control().spec()));
    assert!(refusal.contains(&MIN_REPLICATE_REPS.to_string()));

    // And the document side refuses the same shape rather than publishing a
    // narrower floor from what it has.
    assert!(
        replicate::block_for_cell(&doc_with_placements(&[100.0, 140.0]), &control().spec())
            .is_none()
    );
    assert!(
        replicate::block_for_cell(
            &doc_with_placements(&[100.0, 110.0, 120.0, 130.0]),
            &control().spec()
        )
        .is_none()
    );
}

/// RED against `block_for_cell` as it was: it took the control's FIRST and LAST
/// row per metric and dropped everything between them. On these six placements
/// the two ends agree exactly, so the old route publishes a floor of zero on a
/// control that moved by half.
#[test]
fn the_document_floor_reads_every_placement_and_not_just_the_two_ends() {
    let medians = [100.0, 150.0, 90.0, 140.0, 95.0, 100.0];
    assert_eq!(
        replicate::spread_pct(medians[0], medians[5]),
        Some(0.0),
        "the two ends agree, which is exactly what made the old estimator publish a zero here"
    );

    let doc = doc_with_placements(&medians);
    let block =
        replicate::block_for_cell(&doc, &control().spec()).expect("six placements publish a floor");
    let (floor, drift, residual) = published(&block);

    assert_eq!(block.replicate_reps, 6);
    assert!(
        floor > 30.0,
        "a control that swung between 90 and 150 has no 0% floor, and this says {floor}"
    );
    assert!(
        drift.abs() < 25.0,
        "there is no trend in these six, and this reads one of {drift}"
    );
    assert!(
        residual > 30.0,
        "with no trend to remove the residual is the floor, and this says {residual}"
    );
}

/// RED against a document that publishes a floor without saying what produced
/// it. A reader holding one document cannot otherwise tell a floor from six
/// placements from a floor from two, which is the failure that made the two
/// captures incomparable with nothing flagging it.
#[test]
fn the_block_says_which_estimator_its_floor_came_from() {
    let doc = doc_with_placements(&[100.0, 104.0, 99.0, 103.0, 97.0, 101.0]);
    let block = replicate::block_for_cell(&doc, &control().spec()).expect("a floor");
    let estimator = block
        .estimator
        .as_ref()
        .expect("a published floor names its estimator");

    assert_eq!(estimator.method, ESTIMATOR);
    assert_eq!(estimator.reps, 6);
    assert_eq!(estimator.reps, block.replicate_reps);
    assert_eq!(estimator.coverage, COVERAGE);
    assert_eq!(estimator.dropped_metrics, 0);
    assert!(
        (estimator.multiplier - replicate::prediction_multiplier(6).unwrap()).abs() < 1e-12,
        "the published multiplier is not the one the floor was built with"
    );

    // The block serialises with the estimator in it, in declaration order, and
    // an archived document from the two-point era still parses.
    let text = serde_json::to_string(&block).expect("the block serialises");
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("it parses back");
    let keys: Vec<&str> = parsed
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "cell",
            "replicateReps",
            "estimator",
            "spreadPct",
            "driftPct",
            "residualPct"
        ]
    );

    let archived: Replicate = serde_json::from_str(
        r#"{"cell":"2048x2048@256+gradient","replicateReps":2,"spreadPct":{"a.b":3.46}}"#,
    )
    .expect("a two-point document from the archive still parses");
    assert!(
        archived.estimator.is_none(),
        "a document with no estimator must read back as having none, never as having this one"
    );
}

/// RED against mixing a metric the control measured fewer times into a block
/// that publishes one estimator. A floor over four points and a floor over six
/// are different statistics, so the short one is dropped and counted rather
/// than averaged in under the six-point multiplier.
#[test]
fn a_metric_measured_fewer_times_is_dropped_and_counted() {
    let mut doc = doc_with_placements(&[100.0, 104.0, 99.0, 103.0, 97.0, 101.0]);
    // A second metric the control only managed on four of the six placements.
    for median in [50.0, 52.0, 49.0, 51.0] {
        doc.push(placement_row(control(), "read_random", vec![median]));
    }

    let block = replicate::block_for_cell(&doc, &control().spec()).expect("a floor");
    let estimator = block.estimator.as_ref().expect("an estimator");
    assert_eq!(estimator.reps, 6);
    assert_eq!(estimator.dropped_metrics, 1);
    assert!(
        block
            .spread_pct
            .get("pmtiles.read_random.lookups_per_s")
            .is_none(),
        "a metric measured four times is not in a six-placement block"
    );
    assert!(
        block
            .spread_pct
            .get("pmtiles.read_concurrent@4.lookups_per_s")
            .is_some()
    );
}

/// RED against a `covered_by_noise` that reads something other than the
/// published floor. The floor is what decides regression against noise, so the
/// number the block publishes and the number the verdict uses have to be one
/// number.
#[test]
fn a_delta_inside_the_published_floor_is_noise_and_one_outside_it_is_not() {
    let measurements: Vec<BTreeMap<String, f64>> = [
        (8.0, 5.71),
        (8.4, 9.96),
        (8.1, 6.20),
        (8.3, 9.10),
        (7.9, 5.90),
        (8.2, 8.80),
    ]
    .iter()
    .map(|(p50, p99)| measurement(&[("p50_us", *p50), ("p99_us", *p99)]))
    .collect();

    let block = replicate::block(&control(), &measurements).expect("six placements are a block");
    assert_eq!(block.reps, 6);
    assert_eq!(block.estimator.reps, 6);
    assert_eq!(block.cell, control().spec());
    assert_eq!(block.spread_pct.len(), 2);
    assert_eq!(block.drift_pct.len(), 2);
    assert_eq!(block.residual_pct.len(), 2);

    let p50 = block.spread_pct["p50_us"];
    let p99 = block.spread_pct["p99_us"];
    assert!(
        p99 > p50,
        "the tail moves more than the median and the floor should say so: {p99} against {p50}"
    );
    assert!(replicate::covered_by_noise(&block, "p99_us", p99 - 0.001));
    assert!(!replicate::covered_by_noise(&block, "p99_us", p99 + 0.001));
    assert!(!replicate::covered_by_noise(&block, "p50_us", p99 - 0.001));
    assert!(
        !replicate::covered_by_noise(&block, "wall_ms", 0.0),
        "a metric with no floor is not covered by one"
    );

    // A placement missing a metric the others carry is a refusal, not a hole:
    // the placements would not be the same measurement.
    let mut short = measurements.clone();
    short[3].remove("p99_us");
    let refusal = replicate::block(&control(), &short).expect_err("the placements disagree");
    assert!(refusal.contains("p99_us"));
}

/// RED against a floor computed off a centre that can be zero or negative, and
/// against one that lets a non-finite sample through. An infinity in a noise
/// floor covers every delta, which silently disables every verdict downstream.
#[test]
fn a_floor_with_no_centre_is_not_published() {
    assert!(replicate::dispersion(&[0.0, 0.0, 0.0, 1.0, 2.0]).is_none());
    assert!(replicate::dispersion(&[-1.0, -2.0, -3.0, -4.0, -5.0]).is_none());
    assert!(replicate::dispersion(&[1.0, 2.0, f64::NAN, 4.0, 5.0]).is_none());
    assert!(replicate::dispersion(&[1.0, 2.0, f64::INFINITY, 4.0, 5.0]).is_none());
    // A control that did not move at all is a floor of zero, which is a real
    // answer and not a missing one.
    let flat = replicate::dispersion(&[7.0; 6]).expect("a flat control is still a control");
    assert_eq!(flat.floor_pct, 0.0);
    assert_eq!(flat.drift_pct, 0.0);
    assert_eq!(flat.residual_pct, 0.0);
}
