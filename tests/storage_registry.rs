//! The sweep really walks what the family declares (libviprs-bench#67).
//!
//! The gap this file exists to close: `open`, `first_lookup`, `decode_root`,
//! `read_tileid_order`, the `read_concurrent@T` curve and `requests` were
//! written, tested and merged as library functions that `registry()` never
//! returned, so `storage --profile full` wrote three scenarios over three cells
//! and nothing said so. The brink cell and the `noise` source went the same way:
//! defined in `cells.rs`, referenced only from a test. Each lane pointed at the
//! other and the compose landed neither half.
//!
//! Nothing here asserts a timing. It asserts that a row exists, that it is
//! keyed the way the page will read it, and that the document's two declared
//! blocks are filled.

use std::collections::BTreeSet;

use libviprs_bench::storage::cells::{Backend, Cell, Profile, Source};
use libviprs_bench::storage::document::Document;
use libviprs_bench::storage::{registry, scenario_named};

/// The `storage` binary cargo just built.
///
/// The sweep has to be driven through it rather than by calling `run_sweep` in
/// process: `run_sweep` re-executes `current_exe` once per scenario and reads
/// back a child's JSON, and inside an integration test `current_exe` is the
/// test binary, which answers libtest's argv and not the child protocol's. A
/// sweep called in process therefore produces a document with no cells at all,
/// which is exactly what it did the first time this test ran.
const STORAGE: &str = env!("CARGO_BIN_EXE_storage");

/// Run a `ci` sweep and read the document back.
///
/// A directory per CALL, not per process. libtest runs the tests in this binary
/// on parallel threads, so a path keyed only on the pid is one path shared by
/// every test that wants a sweep: the first one to finish removes the directory
/// while the second one's `storage` child is still writing into it, the child
/// exits 1, and the failure reads as "the storage binary exited exit status: 1"
/// with nothing to say why. It only shows up under load, which is where I found
/// it (#75).
fn ci_document() -> Document {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("k14-registry-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let out = dir.join("storage-results.json");
    let status = std::process::Command::new(STORAGE)
        .arg("--out")
        .arg(&out)
        .env("LIBVIPRS_BENCH_PROFILE", "ci")
        .status()
        .expect("the storage binary runs");
    assert!(status.success(), "the storage binary exited {status}");
    let text = std::fs::read_to_string(&out).expect("the sweep wrote a document");
    let doc = serde_json::from_str(&text).expect("the document parses");
    let _ = std::fs::remove_dir_all(&dir);
    doc
}

/// Every scenario name the `storage` family publishes.
///
/// A literal list on purpose. The point of this test is that dropping a
/// scenario out of `registry()` fails by name here rather than by a quietly
/// shorter document, so the list cannot be derived from the thing it checks.
const DECLARED: [&str; 12] = [
    "generate",
    "open",
    "first_lookup",
    "decode_root",
    "read_plan_order",
    "read_tileid_order",
    "read_random",
    "read_concurrent@1",
    "read_concurrent@2",
    "read_concurrent@4",
    "read_concurrent@8",
    "requests",
];

/// `registry()` returns every scenario the family declares.
///
/// RED against `registry()` as it stands before this change, which returns
/// `reference::all()` and therefore three of these twelve. The nine missing
/// ones are named in the failure.
#[test]
fn the_registry_returns_every_scenario_the_family_declares() {
    let present: BTreeSet<String> = registry().iter().map(|s| s.name()).collect();
    let declared: BTreeSet<String> = DECLARED.iter().map(|s| s.to_string()).collect();

    let missing: Vec<&String> = declared.difference(&present).collect();
    assert!(
        missing.is_empty(),
        "the registry is missing {} of the {} scenarios the family declares: {missing:?}",
        missing.len(),
        DECLARED.len()
    );
    let extra: Vec<&String> = present.difference(&declared).collect();
    assert!(
        extra.is_empty(),
        "the registry returns {extra:?}, which this list does not declare; add them here so the \
         page's config and this test stay one list"
    );

    // And every one is reachable by name, because that is how the child process
    // resolves the scenario its parent asked for.
    for name in DECLARED {
        assert!(
            scenario_named(name).is_some(),
            "`{name}` is in the registry but `scenario_named` cannot find it"
        );
    }
}

/// The full profile walks the brink cell and both published sources.
///
/// RED against a cell table that is three gradient cells. The brink cell is the
/// peak of the open-cost ramp and the whole reason libviprs#1022 exists; a
/// sweep without it brackets the worst case again instead of measuring it.
#[test]
fn the_full_profile_walks_the_brink_cell_and_the_noise_source() {
    let cells = Profile::Full.cells();
    let scales: BTreeSet<u32> = cells.iter().map(|c| c.declared_tiles).collect();
    assert!(
        scales.contains(&16369),
        "the full profile's scales are {scales:?} and none of them is the brink cell's 16369"
    );
    assert!(
        cells.iter().any(|c| c.source == Source::Noise),
        "the full profile walks only {:?}, so compressibility is still not an axis",
        cells
            .iter()
            .map(|c| c.source.as_str())
            .collect::<BTreeSet<_>>()
    );
    assert!(
        cells.iter().any(|c| c.source == Source::Gradient),
        "the gradient has to stay, or the history is not comparable"
    );
    // No cell may be labelled with a tile count its planner does not produce,
    // and none may come from a source that collapses at its tile size.
    for cell in &cells {
        assert_eq!(
            cell.planned_tiles().map(|t| t as u32),
            Some(cell.declared_tiles),
            "{} is labelled {} tiles and plans {:?}",
            cell.spec(),
            cell.declared_tiles,
            cell.planned_tiles()
        );
        libviprs_bench::storage::cells::source_suits_the_cell(cell)
            .unwrap_or_else(|why| panic!("the full profile carries a cell that collapses: {why}"));
    }
}

/// The replicate cell is scheduled through the whole of a full sweep.
///
/// RED against a schedule that measures it once, and against the one this
/// replaced, which measured it exactly twice however long the sweep got. Its
/// dispersion is the only in-run noise floor the document has, and a floor
/// resting on two points has no dispersion of its own: two captures of the same
/// cell on the same host reported 3.46% and 36.89% and neither document could
/// say which was the outlier (#84).
#[test]
fn the_full_profile_schedules_the_replicate_cell_throughout() {
    use libviprs_bench::storage::scenarios::replicate;

    let schedule = Profile::Full.cells();
    let control = libviprs_bench::storage::cells::replicate_cell(Profile::Full)
        .expect("a full sweep declares a replicate control");
    assert_eq!(
        schedule.first(),
        Some(&control),
        "the sweep does not open on the replicate cell"
    );
    assert_eq!(
        schedule.last(),
        Some(&control),
        "the sweep does not close on the replicate cell"
    );
    assert_eq!(
        replicate::placements(&schedule, control),
        Profile::Full.measured_cells().len() + 1,
        "the control is placed before the first measured cell and after every one of them"
    );
    assert!(
        replicate::placements(&schedule, control) >= replicate::MIN_REPLICATE_REPS,
        "a sweep that holds fewer than {} placements publishes no floor at all",
        replicate::MIN_REPLICATE_REPS
    );
    assert!(
        !replicate::has_adjacent_placements(&schedule, control),
        "two placements back to back see none of the drift across the sweep"
    );
    // The ci profile proves the harness runs and is never published, so it
    // carries no control.
    assert!(libviprs_bench::storage::cells::replicate_cell(Profile::Ci).is_none());
}

/// A `ci` sweep writes a row for every scenario, backend and cell it declares.
///
/// This is the one that cannot be satisfied by a list: it runs the sweep and
/// reads the document back. RED against a registry that returns three
/// scenarios, because the document then has no `open.p50` key at all.
#[test]
fn a_ci_sweep_writes_a_row_for_every_scenario_backend_and_cell() {
    let doc = ci_document();
    let keys: BTreeSet<(String, String, u32)> = doc
        .cells
        .iter()
        .map(|c| (c.scenario.clone(), c.backend.clone(), c.scale))
        .collect();

    let mut missing = Vec::new();
    for scenario in Profile::Ci.scenario_names() {
        for cell in Profile::Ci.cells() {
            for backend in Backend::ALL {
                let want = (
                    scenario.clone(),
                    backend.as_str().to_string(),
                    cell.declared_tiles,
                );
                if !keys.contains(&want) {
                    missing.push(want);
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "the ci document is missing {} rows; the first few are {:?}. The document carries {:?}",
        missing.len(),
        missing.iter().take(6).collect::<Vec<_>>(),
        keys.iter()
            .map(|(s, _, _)| s.clone())
            .collect::<BTreeSet<_>>()
    );

    // Peak RSS reaches the read rows. It used to be filtered to
    // `Isolation::ProcessPerRep`, which dropped it from every pass scenario,
    // and the pass scenarios are where a leaf cache would show. `wait4` gives
    // the child's `ru_maxrss` under either isolation, so the filter was
    // discarding a real measurement.
    let read_rows_with_rss = doc
        .cells
        .iter()
        .filter(|c| c.scenario.starts_with("read_") && c.outcome == "ok")
        .filter(|c| c.invariants.peak_rss_mb.is_some())
        .count();
    let read_rows = doc
        .cells
        .iter()
        .filter(|c| c.scenario.starts_with("read_") && c.outcome == "ok")
        .count();
    assert!(
        read_rows > 0 && read_rows_with_rss == read_rows,
        "{read_rows_with_rss} of {read_rows} ok read rows carry peakRssMb; the pass scenarios are \
         where a leaf cache would show and dropping it there is dropping a measurement"
    );

    // Every row says what happened to it. A non-`ok` outcome without a reason
    // is refused downstream, and a skipped row is a declined measurement rather
    // than a missing one.
    for cell in &doc.cells {
        if cell.outcome != "ok" {
            assert!(
                cell.reason.as_deref().is_some_and(|r| !r.is_empty()),
                "{}/{} on {} is `{}` with no reason",
                cell.scenario,
                cell.backend,
                cell.scale,
                cell.outcome
            );
        }
    }
}

/// The two declared models reach the document, marked as models.
///
/// RED against a `Document.modelled` that stays empty, which is what it does
/// today: `model.rs` computes the remote and sync costs and nothing puts them
/// anywhere a reader can see.
#[test]
fn a_sweep_publishes_the_declared_models_with_their_parameters() {
    let doc = ci_document();
    assert!(
        !doc.modelled.is_empty(),
        "the document carries no modelled quantities, so the remote-storage question the whole \
         comparison is about never reaches a reader"
    );
    let names: BTreeSet<&str> = doc.modelled.iter().map(|m| m.name.as_str()).collect();
    assert!(
        names.contains("remote_cost_ms") && names.contains("sync_cost_ms"),
        "the document models {names:?}"
    );
    for entry in &doc.modelled {
        assert_eq!(entry.unit, "ms");
        let model = entry.model.as_object().unwrap_or_else(|| {
            panic!(
                "{} publishes no model block, so its parameters are invisible",
                entry.name
            )
        });
        assert!(
            !model.is_empty(),
            "{} publishes an empty model block",
            entry.name
        );
    }
}

/// A `ci` sweep stays cheap enough that nobody is tempted to skip it.
///
/// Not a timing assertion about the machine: a count of the work the profile
/// declares. `ci` exists to prove the harness runs, so it walks one cell and
/// the cheap end of the scenario list, and this fails if either grows.
#[test]
fn the_ci_profile_stays_the_size_it_says_it_is() {
    assert_eq!(Profile::Ci.cells().len(), 1);
    let names = Profile::Ci.scenario_names();
    assert!(
        names.len() <= 8,
        "the ci profile walks {} scenarios: {names:?}",
        names.len()
    );
    assert!(
        !names.iter().any(|n| n.starts_with("read_concurrent@")),
        "the thread ladder is a full-profile scenario; ci walks {names:?}"
    );
    // Whatever ci walks has to be a subset of what the registry has.
    let present: BTreeSet<String> = registry().iter().map(|s| s.name()).collect();
    for name in &names {
        assert!(
            present.contains(name),
            "ci declares `{name}` and the registry has no such scenario"
        );
    }
    let _ = Cell::new(1, 1, 1, Source::Gradient, 0);
}

// ---------------------------------------------------------------------------
// A scenario body may not panic on an I/O condition
// ---------------------------------------------------------------------------

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use libviprs::planner::TileCoord;
use libviprs_bench::storage::scenarios::{
    Coordinates, ReaderFactory, ScenarioContext, TileReader, concurrent_curve,
};

/// A reader that answers `n` lookups and then fails the way a disk does.
struct FailsAfter {
    remaining: AtomicUsize,
}

impl TileReader for FailsAfter {
    fn tile(&self, _coord: TileCoord) -> Result<Option<Vec<u8>>, String> {
        if self.remaining.fetch_sub(1, Ordering::SeqCst) == 0 {
            return Err("the device reported an I/O error".to_string());
        }
        Ok(Some(vec![0u8; 16]))
    }
}

struct FailingFactory {
    after: usize,
}

impl ReaderFactory for FailingFactory {
    fn fresh(&self) -> Result<Arc<dyn TileReader>, String> {
        Ok(Arc::new(FailsAfter {
            remaining: AtomicUsize::new(self.after),
        }))
    }
}

/// A factory that cannot open anything at all.
struct RefusesToOpen;

impl ReaderFactory for RefusesToOpen {
    fn fresh(&self) -> Result<Arc<dyn TileReader>, String> {
        Err("the archive is not there".to_string())
    }
}

fn coords_of(n: usize) -> Coordinates {
    let all: Vec<TileCoord> = (0..n as u32)
        .map(|col| TileCoord {
            level: 8,
            col,
            row: 0,
        })
        .collect();
    Coordinates {
        plan_order: all.clone(),
        tileid_order: all.clone(),
        random: all.clone(),
        root_addressed: all.first().copied(),
        leaf_addressed: None,
    }
}

/// A lookup that fails becomes a `Skip`, never a panic.
///
/// RED against every scenario body as written before this change: they were
/// full of `expect("a lookup succeeds")` on I/O, which is a recoverable
/// condition wearing a panic. Harmless while nothing called them, and a
/// mid-sweep abort the moment `registry()` grew: the child dies, and
/// `spawn_scenario` reports "the child's output does not parse", which names
/// the wrong problem and hides the real one.
///
/// `concurrent_curve` is the sharpest case because its lookups run inside a
/// scoped thread, so the panic crossed a join before it reached the parent.
#[test]
fn a_failing_lookup_is_a_skip_and_never_a_panic() {
    let coords = coords_of(64);
    let cell = Profile::Ci.cells()[0];
    let factory = FailingFactory { after: 8 };
    let ctx = ScenarioContext {
        backend: Backend::PmTiles,
        cell,
        profile: Profile::Ci,
        seed: 1,
        scratch_root: None,
        artefact: None,
        coords: &coords,
        readers: &factory,
    };

    for threads in [1usize, 2, 4] {
        let reader = factory.fresh().expect("the fixture hands out a reader");
        let outcome = concurrent_curve::run_arm(reader.as_ref(), &coords.random, threads);
        assert!(
            outcome.is_err(),
            "at T={threads} a failing reader produced a successful arm"
        );
        let why = outcome.unwrap_err();
        assert!(
            why.contains("I/O error"),
            "at T={threads} the failure lost the reason: {why}"
        );
    }

    // And through the scenario surface, which is what the sweep calls.
    for scenario in registry() {
        if scenario.name() == "generate" {
            continue;
        }
        let result = scenario.run(&ctx, 2);
        match result {
            Ok(_) => {}
            Err(skip) => assert!(
                !skip.reason.is_empty(),
                "`{}` skipped without a reason",
                scenario.name()
            ),
        }
    }
}

/// A factory that cannot open anything is a `Skip` too.
///
/// RED against a body that unwrapped `fresh()`, which every read scenario in
/// this lane did.
#[test]
fn a_reader_that_will_not_open_is_a_skip_and_never_a_panic() {
    let coords = coords_of(16);
    let cell = Profile::Ci.cells()[0];
    let factory = RefusesToOpen;
    let ctx = ScenarioContext {
        backend: Backend::PmTiles,
        cell,
        profile: Profile::Ci,
        seed: 1,
        scratch_root: None,
        artefact: None,
        coords: &coords,
        readers: &factory,
    };
    for scenario in registry() {
        if scenario.name() == "generate" {
            continue;
        }
        let name = scenario.name();
        match scenario.run(&ctx, 1) {
            Ok(_) => panic!("`{name}` produced samples from a factory that opens nothing"),
            Err(skip) => assert!(!skip.reason.is_empty(), "`{name}` skipped without a reason"),
        }
    }
}

// ---------------------------------------------------------------------------
// Merging per-repetition children
// ---------------------------------------------------------------------------

use libviprs_bench::storage::{WireRun, WireSeries, merge};

fn wire(series: Vec<(&str, Vec<f64>)>) -> WireRun {
    WireRun {
        series: series
            .into_iter()
            .map(|(metric, samples)| WireSeries {
                metric: metric.to_string(),
                unit: "us".to_string(),
                direction: "lower-is-better".to_string(),
                samples,
            })
            .collect(),
        reps: Vec::new(),
        discarded_warmup: Vec::new(),
        peak_rss_bytes: None,
        heap_peak_bytes: None,
        outcome: "ok".to_string(),
        reason: None,
    }
}

/// Repetitions are merged by metric name, not by position.
///
/// RED against `acc.series[i].samples.extend(one.series[i].samples)`. `ReadPass`
/// already picks `p99` or `max` at run time depending on how many lookups a pass
/// made, so two children of one scenario can disagree about what their second
/// series is. Pairing by index then files one metric's samples under another
/// metric's label and nothing downstream can see it.
#[test]
fn per_rep_children_are_merged_by_metric_not_by_position() {
    let first = wire(vec![("p50", vec![1.0]), ("p99", vec![10.0])]);
    let second = wire(vec![("p99", vec![20.0]), ("p50", vec![2.0])]);

    let merged = merge(Some(first), second);
    let by_metric: Vec<(String, Vec<f64>)> = merged
        .series
        .iter()
        .map(|s| (s.metric.clone(), s.samples.clone()))
        .collect();

    let p50 = by_metric
        .iter()
        .find(|(m, _)| m == "p50")
        .expect("p50 survived");
    let p99 = by_metric
        .iter()
        .find(|(m, _)| m == "p99")
        .expect("p99 survived");
    assert_eq!(
        p50.1,
        vec![1.0, 2.0],
        "p50 collected {:?}, so a reordered child's samples landed in the wrong series",
        p50.1
    );
    assert_eq!(p99.1, vec![10.0, 20.0]);
    assert_eq!(merged.series.len(), 2, "merging invented a series");
}

/// A metric that appears partway through is carried and named.
///
/// RED against a merge that silently appends it as a new series, which leaves a
/// series shorter than `reps` and no way to tell that from a dropped sample.
#[test]
fn a_series_that_appears_partway_through_is_named_in_the_reason() {
    let first = wire(vec![("p50", vec![1.0]), ("max", vec![9.0])]);
    let second = wire(vec![("p50", vec![2.0]), ("p99", vec![99.0])]);

    let merged = merge(Some(first), second);
    let reason = merged
        .reason
        .expect("a disagreement about series has to reach the reason");
    assert!(
        reason.contains("p99"),
        "the reason does not name the series that appeared: {reason}"
    );
    // And nothing was concatenated into the wrong place on the way.
    let p50 = merged
        .series
        .iter()
        .find(|s| s.metric == "p50")
        .expect("p50 survived");
    assert_eq!(p50.samples, vec![1.0, 2.0]);
    let max = merged
        .series
        .iter()
        .find(|s| s.metric == "max")
        .expect("max survived");
    assert_eq!(max.samples, vec![9.0]);
}

// ---------------------------------------------------------------------------
// The ignored tests are actually run by something
// ---------------------------------------------------------------------------

/// Every `#[ignore]`d test in the storage suites is run by a CI job.
///
/// RED against the state before this change: `the_brink_cell_sits_under_the_
/// root_cutoff_and_the_leaf_cell_over_it` and
/// `the_cold_split_accounts_for_the_whole_combined_row` are both `#[ignore]`,
/// `ci.yml` runs `cargo test --lib --tests` in both of its cells, and neither
/// passes `--ignored`. So the two assertions that open an archive and read what
/// it came out as were guarded by tests nothing executed, which is the same
/// colour as a test that passes.
///
/// This is a grep guard and it is the honest shape for the claim: the thing
/// being asserted is that a workflow exists and names the command, and no unit
/// test can observe GitHub running it.
#[test]
fn every_ignored_storage_test_is_run_by_a_ci_job() {
    let suite = include_str!("storage_scenarios.rs");
    let ignored: Vec<&str> = suite
        .lines()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with("#[ignore"))
        .filter_map(|(i, _)| {
            suite
                .lines()
                .skip(i)
                .take(6)
                .find(|l| l.trim_start().starts_with("fn "))
        })
        .collect();
    assert!(
        !ignored.is_empty(),
        "this guard found no `#[ignore]`d tests, so it is watching nothing"
    );

    let workflows = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut runs_ignored = Vec::new();
    for entry in std::fs::read_dir(&workflows).expect("the workflow directory is readable") {
        let path = entry.expect("a workflow entry").path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.contains("--ignored") && text.contains("storage_scenarios") {
            runs_ignored.push(path);
        }
    }
    assert!(
        !runs_ignored.is_empty(),
        "{} of the storage suite's tests are `#[ignore]`d ({ignored:?}) and no workflow under \
         .github/workflows runs `--ignored` against `storage_scenarios`, so nothing executes them",
        ignored.len()
    );
}

/// The replicate block is computed from every placement in a sweep.
///
/// A hand-built document rather than a real full sweep, because a full sweep is
/// minutes and this is arithmetic over rows. RED against a `Document.replicate`
/// that stays `None`, which is what it was two lanes ago: `run_sweep` never set
/// it, so the only in-run noise floor the family has never reached a reader.
/// Also RED against the first-versus-last block this replaced, which would read
/// these six placements as the gap between 8.0 and 9.6 and ignore the four in
/// between (#84).
#[test]
fn the_replicate_block_is_computed_from_every_placement_in_the_sweep() {
    use libviprs_bench::storage::cells::replicate_cell;
    use libviprs_bench::storage::scenarios::replicate::block_for;

    let control = replicate_cell(Profile::Full).expect("a full sweep has a control");
    let mut doc = Document::new(Profile::Full, "1970-01-01T00:00:00.000Z".to_string());

    let row = |median: f64| {
        let mut cell = doc_cell_stub();
        cell.scale = control.declared_tiles;
        cell.source = control.source.as_str().to_string();
        cell.backend = "pmtiles".to_string();
        cell.key = "read_random.p50".to_string();
        cell.scenario = "read_random".to_string();
        cell.median = Some(median);
        cell
    };
    // Six placements, with a cell from somewhere else in the sweep between two
    // of them. That one must not be mistaken for a placement: its own `cell`
    // spec moves with its scale, because a real row's spec is what names it and
    // the block matches on that. Two `engines` cells can share a tile count and
    // a source and differ only in their thread budget, and a filter that could
    // not tell those apart would compute a floor across two different
    // measurements and call it drift (#75).
    doc.push(row(8.0));
    let mut other = row(999.0);
    other.scale = 1373;
    other.cell = "8192x8192@256+gradient".to_string();
    doc.push(other);
    for median in [8.6, 8.1, 8.9, 8.3, 9.6] {
        doc.push(row(median));
    }

    let block = block_for(&doc, Profile::Full).expect("six placements make a block");
    assert_eq!(block.replicate_reps, 6);
    assert_eq!(block.cell, control.spec());
    assert_eq!(
        block
            .estimator
            .as_ref()
            .expect("a published floor names its estimator")
            .reps,
        6
    );
    let spread = block.spread_pct.as_object().expect("a spread object");
    let value = spread
        .get("pmtiles.read_random.p50")
        .and_then(|v| v.as_f64())
        .expect("the control's metric has a floor");
    // The six placements have a standard deviation of 0.598 about a centre of
    // 8.45, and six placements carry a multiplier of 2.777.
    assert!(
        (value - 19.65).abs() < 0.2,
        "the floor over all six placements is 19.65%, and the block says {value}"
    );
    // The estimator this replaced would have read the two ends, 8.0 and 9.6, as
    // a flat 20% and thrown the middle four away.
    assert!(
        (value - 20.0).abs() > 0.05,
        "the block is still reading the two ends and calling it a floor: {value}"
    );
    let drift = block
        .drift_pct
        .as_object()
        .expect("a drift object")
        .get("pmtiles.read_random.p50")
        .and_then(|v| v.as_f64())
        .expect("the control's metric has a drift");
    assert!(
        drift > 5.0,
        "these six placements rise across the sweep and the block reads the trend as {drift}"
    );

    // Too few placements is a refusal rather than a narrower floor, and `ci`
    // has no control at all.
    let mut once = Document::new(Profile::Full, "1970-01-01T00:00:00.000Z".to_string());
    once.push(row(8.0));
    assert!(block_for(&once, Profile::Full).is_none());
    let mut twice = Document::new(Profile::Full, "1970-01-01T00:00:00.000Z".to_string());
    twice.push(row(8.0));
    twice.push(row(9.6));
    assert!(block_for(&twice, Profile::Full).is_none());
    assert!(block_for(&doc, Profile::Ci).is_none());
}

/// A `DocumentCell` with every field at its default, for the arithmetic tests.
fn doc_cell_stub() -> libviprs_bench::storage::document::DocumentCell {
    serde_json::from_value(serde_json::json!({
        "backend": "pmtiles",
        "scale": 93,
        "source": "gradient",
        "cell": "2048x2048@256+gradient",
        "scenario": "read_random",
        "metric": "p50",
        "key": "read_random.p50",
        "unit": "us",
        "direction": "lower-is-better",
        "isolation": "process-per-scenario",
        "warmup": null,
        "discardedWarmup": [],
        "reps": 1,
        "minReps": 1,
        "outcome": "ok",
        "reason": null,
        "samples": [1.0],
        "median": 1.0,
        "min": null,
        "max": null,
        "iqr": null,
        "cov": null,
        "ci95": null,
        "ciHalfWidthPct": null,
        "p95OfSamples": null,
        "tail": null,
        "timerSaturated": null,
        "steadyState": null,
        "confidence": "high",
        "lowConfidenceReasons": [],
        "machineLoad": {"cores": null, "loadAvg1m": null, "contentionPerCore": null, "quiet": null},
        "invariants": {}
    }))
    .expect("the stub matches the document's cell shape")
}
