//! The `engines` family: monolithic against streaming against mapreduce, with
//! repetitions, provenance and a document that can be archived.
//!
//! # What this replaces
//!
//! `scalability` measures each `(engine, canvas, thread budget)` **once** and
//! writes a bare JSON array of sixty rows. The array records no commit, no
//! toolchain, no platform, no emulation verdict and no dispersion, so a page
//! that charts it as a time series invents regressions that never happened, and
//! nothing downstream can tell a real move from the noise floor. The published
//! PMTiles numbers this epic exists to retire have exactly that shape and were
//! most likely taken under Rosetta; nothing in them says so.
//!
//! So this family emits the same document the `storage` family does, through
//! the same structs: `schemaVersion`, `family`, `runner`, `profile`,
//! `startedAt`/`finishedAt`, `provenance`, `measurement`, `cells`,
//! `invariants`, `integrity` and a `runId` derived from the document rather
//! than the clock. Same envelope, same refusal rules, same four digests. The
//! vocabulary differs and that is all that differs: a cell's `backend` is an
//! engine, its `cell` key carries a thread budget, and its six metric series
//! are the columns the engines comparison has always had.
//!
//! # Repetitions, and the peak RSS they are only honest about in a child
//!
//! Every `(engine, cell)` takes a discarded warm-up and then N timed
//! repetitions, each in its own child process, through
//! [`crate::harness::spawn_single_cell`]. That is not tidiness either. The old
//! sweep ran all three engines in one process and read
//! `getrusage(RUSAGE_SELF).ru_maxrss`, a monotonic process-wide high-water
//! mark, so whichever engine peaked highest set the watermark and every engine
//! measured after it reported that number as its own: the first full capture
//! has byte-identical peak RSS for all three engines in twenty of twenty
//! groups, to seven decimal places. A child per repetition makes the watermark
//! a per-run peak, taken by the parent through `wait4`, on one basis for every
//! engine.
//!
//! The engine order rotates between repetitions, so slow drift over a cell's
//! wall-clock window hits the three roughly equally rather than penalising
//! whichever ran last.
//!
//! # The invariants, and the one that is not
//!
//! `tiles_produced` is exact, reproduces, and is the same for all three
//! engines, so it is an invariant with an equality verdict rather than a
//! timing. So are the three counters walked off the real sink directory:
//! `output_bytes`, `filesystem_entries` and `directories`.
//!
//! `allocated_bytes` is measured and **not** published, and the reason is not
//! that it wobbled here. It did not: 24 fresh children at `1024x720` and 12 at
//! `4096x2880`, three engines each, all reported one value. The reason is what
//! the number is. It is `st_blocks * 512`, so it answers to the filesystem's
//! allocator rather than to the engine, and the `storage` family's first full
//! capture caught it differing between two repetitions of one generation and
//! refused those two cells. A field that can move for a reason the engine has
//! no part in is not a claim about the engine, and this family publishes an
//! invariant as a claim with an equality verdict: one that can be falsified by
//! a block size is a refusal waiting for a different filesystem.
//!
//! [`crate::ArtefactFacts`] carries it regardless, so the judgement can be
//! re-measured rather than re-argued.
//!
//! # This family does not need libvips
//!
//! Nothing here touches the comparison, so it builds and runs with no cargo
//! features at all.

pub mod attest;
pub mod cells;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::RunMetrics;
use crate::harness::{self, Engine};
use crate::storage::document::{
    CellLabels, CellReport, Document, DocumentCell, InvariantBlock, MachineLoad, Measurement,
    MeasurementSpec, Reps,
};
use crate::storage::scenarios::{Direction, Invariants, Isolation, MetricSpec, RepFacts, Unit};
use crate::storage::scenarios::{Outcome, Warmup};
use crate::storage::{agreed, stats};

use cells::{ENGINES, EngineCell, Profile, SOURCE};

/// The family name an importer matches on before it reads anything else.
pub const FAMILY: &str = "libviprs-engines";

/// The one runner this family has.
pub const RUNNER: &str = "libviprs-engines";

/// The file a sweep writes, inside the family's own report directory.
pub const DOCUMENT_NAME: &str = "engines-results.json";

/// The one scenario this family has: plan a pyramid and write every tile.
///
/// Named and resolved rather than left implicit, because the aggregator refuses
/// a document whose `resolved.scenarios` says `"all"`: `"all"` means a
/// different set on every day the suite grows, so two runs that both say it are
/// not comparable.
pub const SCENARIO: &str = "pyramid";

/// The seed the bootstrap resamples with, shared with the `storage` family so
/// an interval computed here and one computed there mean the same thing.
pub const SEED: u64 = crate::storage::cells::SEED;

/// The environment variable that lets a dirty tree archive, with the dirt
/// stamped onto every cell.
pub const ALLOW_DIRTY_VAR: &str = "BENCH_ALLOW_DIRTY";

// ---------------------------------------------------------------------------
// The six columns
// ---------------------------------------------------------------------------

/// Wall-clock time for the whole pyramid.
pub const WALL: MetricSpec = MetricSpec {
    name: "wall",
    unit: Unit::Milliseconds,
    direction: Direction::LowerIsBetter,
};

/// Peak resident set size of the child that ran this engine, and nothing else.
pub const PEAK_RSS_MB: MetricSpec = MetricSpec {
    name: "peak_rss_mb",
    unit: Unit::Megabytes,
    direction: Direction::LowerIsBetter,
};

/// The engine's own accounting of the raster buffers it held.
///
/// A different basis from `peak_rss_mb` and never compared against it: this is
/// what the engine thinks it is holding, that is what the operating system
/// charged the process. The two are separate columns for that reason and have
/// been since issue #153.
pub const TRACKED_MEMORY_MB: MetricSpec = MetricSpec {
    name: "tracked_memory_mb",
    unit: Unit::Megabytes,
    direction: Direction::LowerIsBetter,
};

/// Tiles per second. Tiles, never pixels.
pub const TILES_PER_SECOND: MetricSpec = MetricSpec {
    name: "tiles_per_second",
    unit: Unit::PerSecond,
    direction: Direction::HigherIsBetter,
};

/// Throughput per mebibyte of peak RSS: memory efficiency.
pub const TILES_PER_SECOND_PER_MB: MetricSpec = MetricSpec {
    name: "tiles_per_second_per_mb",
    unit: Unit::PerSecondPerMegabyte,
    direction: Direction::HigherIsBetter,
};

/// Mebibyte-seconds of peak RSS per tile: resource cost.
pub const RESOURCE_COST: MetricSpec = MetricSpec {
    name: "resource_cost",
    unit: Unit::MegabyteSecondsPerTile,
    direction: Direction::LowerIsBetter,
};

/// Every column a cell publishes, in the order the document carries them.
pub const METRICS: [MetricSpec; 6] = [
    WALL,
    PEAK_RSS_MB,
    TRACKED_MEMORY_MB,
    TILES_PER_SECOND,
    TILES_PER_SECOND_PER_MB,
    RESOURCE_COST,
];

/// Where a sweep's document lands under a report root.
///
/// Derived from the family's own report directory rather than spelled out, so
/// this family cannot drift out of the `report/<family>/` layout every other
/// family follows.
pub fn default_output_path(report_root: &Path) -> PathBuf {
    crate::family::Family::Engines
        .report_dir(report_root)
        .join(DOCUMENT_NAME)
}

/// The declared measurement block for a profile.
pub fn measurement(profile: Profile) -> Measurement {
    let reps = Reps::of(&[(SCENARIO, profile.reps())]);
    Measurement::probed_from(MeasurementSpec {
        unit: "fresh-process-per-repetition",
        isolation: "subprocess-per-repetition",
        min_reps: reps.clone(),
        reps,
        seed: SEED,
        warmup: Some(Warmup::ONE_DISCARDED_PASS),
        // The source raster is built inside each child and the sink directory
        // is fresh per child, so what the page cache holds between repetitions
        // is the tile files of the previous one. The warm-up discards the first
        // pass for that reason; what state the cache is in after it is not
        // something this sweep controls, and saying "warm" would be a claim.
        page_cache: "warm-unknown",
    })
}

// ---------------------------------------------------------------------------
// One repetition, as six numbers and a set of facts
// ---------------------------------------------------------------------------

/// The six columns of one repetition, derived from that repetition alone.
///
/// Derived per repetition and never from the medians. A ratio of two medians is
/// not the median of the ratio, and only the per-repetition form has a spread
/// at all: a `tiles_per_second_per_mb` computed once from two summary numbers
/// would publish a point estimate with an interval borrowed from somewhere
/// else.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RepColumns {
    pub wall_ms: f64,
    pub peak_rss_mb: f64,
    pub tracked_memory_mb: f64,
    pub tiles_per_second: Option<f64>,
    pub tiles_per_second_per_mb: Option<f64>,
    pub resource_cost: Option<f64>,
}

impl RepColumns {
    /// The columns one child's metrics earn.
    ///
    /// A derived column whose denominator is zero is `None` and contributes no
    /// sample, rather than the `0.0` the old sweep published. Zero is a real
    /// value on every one of these columns and on two of them it is the best
    /// possible score, so a hole written as a zero is a hole nobody can see.
    pub fn of(metrics: &RunMetrics) -> RepColumns {
        let wall_ms = metrics.wall_time_ms();
        let secs = metrics.wall_time.as_secs_f64();
        let peak_rss_mb = metrics.peak_rss_mb();
        let tiles = metrics.tiles_produced;
        let tiles_per_second = (secs > 0.0).then(|| tiles as f64 / secs);
        RepColumns {
            wall_ms,
            peak_rss_mb,
            tracked_memory_mb: metrics.tracked_memory_mb(),
            tiles_per_second,
            tiles_per_second_per_mb: tiles_per_second
                .filter(|_| peak_rss_mb > 0.0)
                .map(|tps| tps / peak_rss_mb),
            resource_cost: (tiles > 0 && peak_rss_mb > 0.0)
                .then(|| (peak_rss_mb * secs) / tiles as f64),
        }
    }

    /// This repetition's value for one column.
    pub fn get(&self, metric: &MetricSpec) -> Option<f64> {
        match metric.name {
            "wall" => Some(self.wall_ms),
            "peak_rss_mb" => Some(self.peak_rss_mb),
            "tracked_memory_mb" => Some(self.tracked_memory_mb),
            "tiles_per_second" => self.tiles_per_second,
            "tiles_per_second_per_mb" => self.tiles_per_second_per_mb,
            "resource_cost" => self.resource_cost,
            _ => None,
        }
    }
}

/// What one repetition left behind, as the shared `agreed` walk reads it.
///
/// `allocated_bytes` is deliberately not filled. See the module header: it is
/// `st_blocks * 512`, it answers to the filesystem's allocator rather than to
/// the engine, and the `storage` family's first full capture caught it
/// differing between two repetitions of one generation. Publishing it would
/// refuse cells for something the engine had no part in.
pub fn rep_facts(metrics: &RunMetrics) -> RepFacts {
    RepFacts {
        invariants: Invariants {
            output_bytes: metrics.artefact.map(|a| a.output_bytes),
            allocated_bytes: None,
            filesystem_entries: metrics.artefact.map(|a| a.filesystem_entries),
            directories: metrics.artefact.map(|a| a.directories),
            tiles_produced: Some(metrics.tiles_produced),
            artefact_digest: None,
            root_entries: None,
            leaves: None,
            requests: None,
            request_bytes: None,
        },
        scratch: None,
    }
}

/// Turn one engine's repetitions in one cell into the six rows they earned.
///
/// `discarded` is the primary column of each thrown-away warm-up pass, carried
/// so a reader can check the values are not also in `samples`.
#[allow(clippy::too_many_arguments)]
pub fn rows_for(
    cell: EngineCell,
    engine: Engine,
    runs: &[RunMetrics],
    discarded: &[f64],
    profile: Profile,
    load: MachineLoad,
    timer: Option<stats::TimerProbe>,
) -> Vec<DocumentCell> {
    let facts: Vec<RepFacts> = runs.iter().map(rep_facts).collect();
    let (invariants, disagreements) = agreed(&facts);
    let mut block = InvariantBlock::from(&invariants);
    // The peak RSS is a scalar on the cell as well as a series, exactly as it
    // is in a storage document: under one child per repetition it is the
    // largest single repetition, taken as the maximum across the children.
    block.peak_rss_mb = runs
        .iter()
        .map(RunMetrics::peak_rss_mb)
        .fold(None::<f64>, |acc, v| Some(acc.map_or(v, |a| a.max(v))));

    let columns: Vec<RepColumns> = runs.iter().map(RepColumns::of).collect();

    // An invariant that differs between two repetitions of one commit is a
    // defect, never noise, so the cell is refused rather than averaged. A cell
    // that produced nothing at all failed.
    let outcome = if runs.is_empty() {
        Outcome::Failed
    } else if disagreements.is_empty() {
        Outcome::Ok
    } else {
        Outcome::Refused
    };
    let reason = if runs.is_empty() {
        Some("no repetition produced metrics".to_string())
    } else if disagreements.is_empty() {
        None
    } else {
        Some(disagreements.join("; "))
    };

    let labels = CellLabels {
        backend: engine.as_str().to_string(),
        scale: cell.planned_tiles().unwrap_or(0),
        source: SOURCE.to_string(),
        cell: cell.spec(),
    };

    METRICS
        .iter()
        .map(|metric| {
            DocumentCell::from_report(CellReport {
                labels: labels.clone(),
                scenario: SCENARIO,
                metric: *metric,
                isolation: Isolation::ProcessPerRep,
                warmup: Some(Warmup::ONE_DISCARDED_PASS),
                discarded_warmup: discarded.to_vec(),
                reps_declared: profile.reps(),
                min_reps: profile.reps(),
                samples: columns.iter().filter_map(|c| c.get(metric)).collect(),
                outcome,
                reason: reason.clone(),
                invariants: block.clone(),
                machine_load: load,
                timer,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The sweep
// ---------------------------------------------------------------------------

/// Run a whole sweep and build the document.
///
/// Every repetition of every engine is a fresh child of this process, spawned
/// through [`harness::spawn_single_cell`], which reaps it with `wait4` and
/// takes the child's own `ru_maxrss` as the authoritative per-run peak. Nothing
/// here measures in-process, and that is the fix for the process-wide watermark
/// the old sweep published as three different engines' memory.
pub fn run_sweep(profile: Profile) -> Document {
    let exe = harness::current_exe();
    let started = crate::storage::now_iso();
    let mut doc = Document::new_for(
        FAMILY,
        RUNNER,
        profile.label(),
        started,
        measurement(profile),
    );
    let timer = Some(stats::probe_timer());
    let reps = profile.reps();
    let warmup = profile.warmup();

    for cell in profile.cells() {
        if cell.plan().is_none() {
            eprintln!("engines: {} does not plan, skipped", cell.spec());
            continue;
        }
        eprintln!(
            "engines: {} ({:.1} MP), {reps} repetitions after {warmup} discarded",
            cell.spec(),
            cell.megapixels()
        );
        let load = MachineLoad::sample();

        // The warm-up first, one pass per engine, kept only as the values the
        // document has to prove are NOT in `samples`.
        let mut discarded: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
        for _ in 0..warmup {
            for engine in ENGINES {
                if let Some(m) = harness::spawn_single_cell(&exe, cell.spec_for(engine)) {
                    discarded
                        .entry(engine.as_str())
                        .or_default()
                        .push(RepColumns::of(&m).wall_ms);
                }
            }
        }

        // Then the timed repetitions, engine-inner so drift hits the three
        // roughly equally, with the order rotated so none of them is always
        // last in the window.
        let mut collected: BTreeMap<&'static str, Vec<RunMetrics>> = BTreeMap::new();
        for rep in 0..reps.max(1) {
            for engine in rotated(rep as usize) {
                match harness::spawn_single_cell(&exe, cell.spec_for(engine)) {
                    Some(m) => collected.entry(engine.as_str()).or_default().push(m),
                    None => eprintln!(
                        "engines: {} {} produced no metrics in repetition {}",
                        engine.as_str(),
                        cell.spec(),
                        rep + 1
                    ),
                }
            }
        }

        let group: Vec<(Engine, Vec<RunMetrics>)> = ENGINES
            .iter()
            .filter_map(|engine| {
                collected
                    .get(engine.as_str())
                    .map(|runs| (*engine, runs.clone()))
            })
            .collect();
        let verdicts = attest::attest_group(&group);
        for (engine, verdict) in &verdicts {
            for reason in verdict.reasons() {
                eprintln!("engines: {} is not attested: {reason}", cell.spec());
                let _ = engine;
            }
        }

        for (engine, runs) in &group {
            let verdict = verdicts
                .iter()
                .find(|(e, _)| e == engine)
                .map(|(_, v)| v.is_attested());
            for mut row in rows_for(
                cell,
                *engine,
                runs,
                discarded.get(engine.as_str()).map_or(&[][..], |v| &v[..]),
                profile,
                load,
                timer,
            ) {
                // From the observation, never from the cell. A constant here is
                // the exact failure `attest` exists to prevent.
                row.attested = verdict;
                doc.push(row);
            }
        }
    }

    doc.rebuild_invariant_table();
    doc.finished_at = Some(crate::storage::now_iso());
    doc.provenance = Some(sweep_provenance(profile, &doc));
    // The dirt travels with every number or the run is refused for the rule
    // `--allow-dirty` exists to satisfy. Nothing filled this until #75.
    doc.stamp_dirty_from_provenance();
    // After the provenance, because the id is derived from it.
    doc.stamp_run_id();
    doc
}

/// The engine order for one repetition: rotated, so no engine is always the one
/// measured last in a cell's wall-clock window.
fn rotated(rep: usize) -> Vec<Engine> {
    let n = ENGINES.len();
    (0..n).map(|i| ENGINES[(i + rep) % n]).collect()
}

/// The `provenance` block, filled from the environment the sweep really ran in.
///
/// `Document::new_for` leaves it `None` and the aggregator refuses a document
/// without it, which is the correct refusal. The scratch directory probed here
/// is the one the engines' sink really writes into, so the recorded `fsType`
/// describes where the tiles went rather than where this function happened to
/// look.
fn sweep_provenance(profile: Profile, doc: &Document) -> serde_json::Value {
    let scratch = crate::engine_sink_root();
    let _ = std::fs::create_dir_all(&scratch);
    let provenance = crate::provenance::Provenance::capture_for_document(&scratch);
    for warning in provenance.document_provenance_warnings() {
        eprintln!("{warning}");
    }
    for warning in provenance.measurement_condition_warnings() {
        eprintln!("{warning}");
    }
    let allow_dirty = std::env::var(ALLOW_DIRTY_VAR).is_ok();
    let mut scales: Vec<u64> = profile
        .cells()
        .iter()
        .filter_map(|c| c.planned_tiles())
        .map(u64::from)
        .collect();
    scales.sort_unstable();
    scales.dedup();
    provenance.to_document_block(
        &serde_json::json!({
            "argv": std::env::args().collect::<Vec<_>>(),
            "command": "engines",
            "cwd": std::env::current_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
            "env": {
                ALLOW_DIRTY_VAR: std::env::var(ALLOW_DIRTY_VAR).ok(),
                "RUSTFLAGS": std::env::var("RUSTFLAGS").ok(),
                "BENCH_DAEMON_ARCH": std::env::var("BENCH_DAEMON_ARCH").ok(),
                "TMPDIR": std::env::var("TMPDIR").ok(),
            },
            "resolved": {
                "profile": profile.label(),
                "reps": doc.measurement.reps,
                "scenarios": [SCENARIO],
                "scales": scales,
                "engines": ENGINES.iter().map(|e| e.as_str()).collect::<Vec<_>>(),
                "concurrency": profile.concurrency_levels(),
                "cells": profile.cells().iter().map(|c| c.spec()).collect::<Vec<_>>(),
                "tileSize": cells::TILE_SIZE,
                "streamingBudgetFloorBytes": cells::STREAMING_BUDGET_FLOOR,
            },
        }),
        allow_dirty,
    )
}
