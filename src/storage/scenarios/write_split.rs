//! The write, split into the two halves `generate` has always been
//! (libviprs#1136).
//!
//! `generate` is one number over the PNG encode, every `add_tile` and
//! `finish`. Against the directory backend the archive loses by 1.15x to 1.83x
//! across the published cells, and the fix for a slow encode, a slow payload
//! table and a slow finalize are three different pieces of work that one
//! number cannot tell apart. The read side was taken apart for exactly this
//! reason in [`super::open`] and the write side never was, so every other
//! issue in the epic would be sized against a guess.
//!
//! # Two phases, walked through the public API
//!
//! `EngineBuilder::run` calls `TileSink::finish` itself, so there is no seam
//! between ingestion and finalization to time from outside. There is one
//! inside the public trait, though: [`DeferFinish`] is an ordinary
//! `TileSink` that forwards `write_tile` to the sink underneath and answers
//! `finish` with `Ok(())`, and it answers the engine's bookkeeping hooks by
//! naming the inner sink rather than by reimplementing them. The engine runs
//! its whole pass against it, the clock stops, and then the real `finish` is
//! called and timed on its own.
//!
//! Nothing on the product's hot path changes to be measured: no writer is
//! instrumented, no engine flag is added, and the sink underneath is the one
//! `write_pyramid` builds. What makes this a measurement rather than a
//! rearrangement is that the two halves reconcile with the combined row, and
//! that the archive the hand walk leaves behind is byte for byte the archive
//! the combined pass leaves behind.
//!
//! # The phases have to add up, and on some cells they cannot
//!
//! [`reconciles`] is the check. [`reconciliation_is_meaningful`] is the part
//! worth reading twice, and it is the write side's version of the read side's
//! too-small-a-root guard. A split that never measured the finalize at all is
//! the ingest on its own, so on a cell where the finalize is three percent of
//! the pass that failure reconciles too, and a green check there means
//! nothing. The guard asks that counterfactual outright rather than being
//! loosened until every cell passes.
//!
//! Most of the published cells are refused by it, and that is the honest
//! answer rather than a gap. The 21851-tile cell is 3.3% finalize on the
//! archive and about 0.0% on the tree, so its sum cannot tell a measured
//! finalize from an unmeasured one. What holds on every cell is the rest: the
//! hand walk's artefact hashes to the same digest as the combined pass's, and
//! the wrapper refuses a pass in which the engine did not ask to finish
//! exactly once. The reconciliation runs where it can fail, and what it proves
//! is the method.
//!
//! Two things put a cell under it. The directory backend is one. Everything
//! `FsSink::finish` does is conditional and this sweep meets none of the
//! conditions: it canonicalises a dedupe layout only when dedupe is on and it
//! is off by default, writes a DZI sidecar only for a DeepZoom plan and a
//! properties sidecar only for Zoomify or IIIF while these cells are XYZ,
//! re-hashes the tree only under `ChecksumMode::Verify`, and writes a manifest
//! only when one was asked for. So the tree's finish is a few microseconds,
//! and the answer to which half of `generate` the directory backend spends its
//! time in is "all of it, in ingestion". An unoptimised build is the other:
//! debug slows the encode about thirty times and leaves the disk-bound
//! finalize alone, so the PMTiles finalize falls from over half a pass to
//! three percent of one.
//!
//! # What the heap numbers are
//!
//! [`crate::storage::heap`] says which quantity in full. In one line: live
//! heap, whole process, over what was live when the phase started, which is
//! the basis `libviprs/tests/pmtiles_bounded_memory.rs` measures 72 bytes a
//! distinct payload on and **not** the basis `src/pmtiles/writer.rs`'s RSS
//! table implies 105.8 on.
//!
//! The two phases share one window, so the peaks are nested: the ingest peak
//! is the high-water mark up to the last `add_tile` and the finalize peak is
//! the high-water mark over the whole pass. The writer's own documentation
//! claims "the two numbers above and below `finish` are now the same", and
//! these two rows are that claim as a pair of published figures.
//!
//! # Why the reconciliation goes through the registry
//!
//! [`super::open::split_pass`] times its split and its combined row in one
//! loop, and this module has no equivalent. The difference is what the issue
//! asks for: the read-side phases are not rows, and these two are. So the
//! reconciliation in `tests/storage_scenarios.rs` sums what
//! `scenario_named("generate_ingest")` and `scenario_named("generate_finalize")`
//! publish against what `scenario_named("generate")` publishes, which is the
//! same arithmetic over the things a reader will actually see.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use libviprs::planner::PyramidPlan;
use libviprs::sink::{SinkError, Tile, TileFormat, TileSink};
use libviprs::sink_pmtiles::PmTilesSink;
use libviprs::{EngineBuilder, FsSink, Raster};

use super::super::cells::{Backend, Cell, Profile};
use super::super::document::generate_reps;
use super::super::heap;
use super::super::{Scratch, archive_shape, artefact_digest, occupancy, raster};
use super::{
    Direction, Invariants, Isolation, MetricSpec, RepFacts, Scenario, ScenarioContext, ScenarioRun,
    Series, Skip, Unit, Warmup,
};

/// The two halves of a write, in the order they happen.
pub const WRITE_PHASES: [&str; 2] = ["generate_ingest", "generate_finalize"];

/// How far the two phases may drift from the combined row before the split is
/// measuring something else.
///
/// 20%, and deliberately tighter than the read side's 25%. That allowance is
/// wide for a structural reason this split does not have: the cold split runs
/// two opens per iteration against a combined row that runs one, so a real
/// difference is built into it. Here the split runs one generation and the
/// combined row runs one generation, and the only thing between them is
/// dispersion. Measured drifts on this protocol, medians over seven
/// interleaved repetitions: -7.7% to +5.3% over five runs on an arm64 laptop
/// with another job on it, +0.8% on an x86_64 CI runner, and -0.5% to -3.9%
/// across the four `(backend, source)` combinations of the 21851-tile cell.
///
/// The two numbers this sits between pull opposite ways. Too wide and the
/// check stops catching anything; too tight and it reds on dispersion, which
/// is the failure nobody investigates and everybody reruns. 20% is about two
/// and a half times the worst drift measured, and the counterfactual on the
/// cell the check runs on is about two and a half times the other side of it.
///
/// One thing it deliberately does not have to absorb. In a sweep the two
/// phases arm the counting allocator and `generate` does not, so the
/// reconciliation arms across all three itself and the drift is a difference
/// between two measurements made the same way rather than partly an artefact
/// of instrumenting one side.
pub const RECONCILIATION_ALLOWANCE_PCT: f64 = 20.0;

// ---------------------------------------------------------------------------
// Reconciliation
// ---------------------------------------------------------------------------

/// How far the summed phases sit from the combined row, as a percentage of the
/// combined row. Negative means the split came out lower.
pub fn drift_pct(split_ms: f64, combined_ms: f64) -> f64 {
    (split_ms - combined_ms) / combined_ms * 100.0
}

/// Whether the summed phases reconcile with the combined row.
pub fn reconciles(split_ms: f64, combined_ms: f64) -> bool {
    drift_pct(split_ms, combined_ms).abs() <= RECONCILIATION_ALLOWANCE_PCT
}

/// What share of the pass the finalize is.
pub fn finalize_share_pct(ingest_ms: f64, finalize_ms: f64) -> f64 {
    let split = ingest_ms + finalize_ms;
    if split <= 0.0 {
        return 0.0;
    }
    finalize_ms / split * 100.0
}

/// Whether this pass is one a reconciliation check can say anything about.
///
/// It asks the counterfactual outright: had the finalize never been measured,
/// the split would have been the ingest alone, so the check is only able to
/// fail when the ingest alone does **not** reconcile with the combined row.
///
/// This used to be a floor on the finalize's share of the pass, at 1.2 times
/// the allowance, which is the same test with the drift assumed to be zero.
/// The drift is not zero, and the proxy is calibrated on whichever machine
/// wrote it down: the cell I picked was 54% finalize on this laptop and 28.3%
/// on the CI runner, whose disk is quicker and whose cores are slower, so the
/// floor refused a pass that reconciled to within 0.8%. Asking the two
/// measured numbers needs no calibration.
///
/// `Err` carries the reason, in the shape
/// [`super::open::reconciliation_is_meaningful`] carries its own: a cell that
/// cannot reconcile still publishes its phases, it just does not claim they
/// were checked.
pub fn reconciliation_is_meaningful(ingest_ms: f64, combined_ms: f64) -> Result<(), String> {
    if !reconciles(ingest_ms, combined_ms) {
        return Ok(());
    }
    Err(format!(
        "an ingest of {ingest_ms:.2} ms on its own is within {RECONCILIATION_ALLOWANCE_PCT}% of \
         the combined row's {combined_ms:.2} ms, a drift of {:+.1}%, so a split that never \
         measured the finalize at all would reconcile here and this cell cannot tell that failure \
         from a correct split",
        drift_pct(ingest_ms, combined_ms)
    ))
}

// ---------------------------------------------------------------------------
// The seam
// ---------------------------------------------------------------------------

/// A sink that forwards everything and declines to finish.
///
/// The engine calls `finish` at the end of its pass, so this is the only place
/// the two halves can be separated without changing the engine. It records
/// that it was asked, because a split whose phase boundary is somewhere the
/// engine no longer calls is a split measuring one phase twice and nothing
/// would otherwise say so.
pub struct DeferFinish<'a> {
    inner: &'a dyn TileSink,
    asked: AtomicU32,
}

impl<'a> DeferFinish<'a> {
    pub fn new(inner: &'a dyn TileSink) -> DeferFinish<'a> {
        DeferFinish {
            inner,
            asked: AtomicU32::new(0),
        }
    }

    /// How many times the engine asked this sink to finish.
    pub fn times_asked(&self) -> u32 {
        self.asked.load(Ordering::SeqCst)
    }
}

impl TileSink for DeferFinish<'_> {
    fn write_tile(&self, tile: &Tile) -> Result<(), SinkError> {
        self.inner.write_tile(tile)
    }

    fn finish(&self) -> Result<(), SinkError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    /// The one override a transparent decorator owes: every bookkeeping hook
    /// on the trait defaults to forwarding through here, so the checkpoint
    /// root, the content format and the retry counters all reach the sink
    /// underneath without this type naming them.
    fn inner_sink(&self) -> Option<&dyn TileSink> {
        Some(self.inner)
    }
}

// ---------------------------------------------------------------------------
// One hand-walked generation
// ---------------------------------------------------------------------------

/// What one hand-walked generation cost, phase by phase.
#[derive(Debug, Clone)]
pub struct HandWalk {
    /// Everything up to and including the last `add_tile`: the sink built, the
    /// raster resampled level by level, every tile encoded and handed over.
    pub ingest: Duration,
    /// The `finish` the engine was not allowed to make.
    pub finalize: Duration,
    /// The archive file, or the root of the tree.
    pub output: PathBuf,
    pub tiles_produced: u64,
    /// Peak live heap up to the end of ingestion, over the phase's baseline.
    pub ingest_peak_bytes: Option<u64>,
    /// Live heap still held when the last tile was in: the tables ingestion
    /// retains rather than the high-water mark it reached, which is the
    /// quantity the writer's distinct-payload bound is about.
    pub ingest_live_bytes: Option<u64>,
    /// Peak live heap over the whole pass, on the same baseline as
    /// [`HandWalk::ingest_peak_bytes`], so the two are comparable and the
    /// difference between them is what finalization added.
    pub finalize_peak_bytes: Option<u64>,
}

/// Write one pyramid with the finish held back, and time the two halves.
///
/// The raster is built before the clock starts, exactly as
/// [`crate::storage::write_pyramid`] builds its own before its own, so the two
/// numbers cover the same work. The sink construction is inside the clock, for
/// the same reason.
pub fn hand_walk(
    backend: Backend,
    cell: Cell,
    plan: &PyramidPlan,
    into: &Path,
) -> Result<HandWalk, String> {
    let source = raster(cell.source, cell.width, cell.height);
    let armed = heap::arm();
    let started = Instant::now();
    match backend {
        Backend::PmTiles => {
            let archive = into.join("pyramid.pmtiles");
            let sink = PmTilesSink::builder(&archive)
                .plan(plan.clone())
                .tile_format(TileFormat::Png)
                .build()
                .map_err(|e| format!("the archive sink does not build: {e}"))?;
            walk(&sink, &source, plan, &armed, started, archive)
        }
        Backend::Directory => {
            let root = into.join("tree");
            let sink = FsSink::new(&root, plan.clone()).with_format(TileFormat::Png);
            walk(&sink, &source, plan, &armed, started, root)
        }
    }
}

/// The half of [`hand_walk`] that does not depend on which sink it is.
fn walk<S: TileSink>(
    sink: &S,
    source: &Raster,
    plan: &PyramidPlan,
    armed: &heap::Armed,
    started: Instant,
    output: PathBuf,
) -> Result<HandWalk, String> {
    // `run_collect` rather than `run`, because the wrapper has to survive the
    // pass: what it knows is whether the engine asked it to finish, and a
    // `run` that drops it takes the answer with it.
    let (result, deferring) = EngineBuilder::new(source, plan.clone(), DeferFinish::new(sink))
        .run_collect()
        .map_err(|e| format!("the run failed: {e}"))?;
    let ingest = started.elapsed();
    let ingest_peak_bytes = armed.peak_bytes();
    let ingest_live_bytes = armed.live_bytes();

    // The phase boundary is the engine's own `finish` call, so a pass that did
    // not make one is a pass this split cannot divide. Silently, otherwise:
    // the ingest number would quietly become the whole generation and the
    // finalize number would be an unfinished sink finishing.
    if deferring.times_asked() != 1 {
        return Err(format!(
            "the engine asked the sink to finish {} times and this split's phase boundary is the \
             one time it does, so the ingest phase is not what it says it is",
            deferring.times_asked()
        ));
    }

    let at = Instant::now();
    sink.finish()
        .map_err(|e| format!("the sink does not finish: {e}"))?;
    let finalize = at.elapsed();

    Ok(HandWalk {
        ingest,
        finalize,
        output,
        tiles_produced: result.tiles_produced,
        ingest_peak_bytes,
        ingest_live_bytes,
        finalize_peak_bytes: armed.peak_bytes(),
    })
}

// ---------------------------------------------------------------------------
// The two scenarios
// ---------------------------------------------------------------------------

/// Milliseconds for one phase, the unit and direction `generate.wall` already
/// uses, so the three rows sit on one axis.
///
/// The only series either phase publishes. `generate` also carries
/// `tiles_per_s`, and neither half of it should: a rate over the ingest alone
/// invites a reader to compare it with the whole pass's rate as though the two
/// answered the same question, and a finalize has no per-tile rate worth the
/// name. The generation rate belongs to the generation.
pub const WALL: MetricSpec = MetricSpec {
    name: "wall",
    unit: Unit::Milliseconds,
    direction: Direction::LowerIsBetter,
};

/// Which half of the write a scenario publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Ingest,
    Finalize,
}

/// `generate_ingest`: the encode and every `add_tile`, with the finish held
/// back.
pub struct GenerateIngest;

/// `generate_finalize`: the `finish` on its own, after an ingestion this
/// scenario runs and does not publish.
///
/// The ingestion is paid for again rather than shared with
/// [`GenerateIngest`], and it has to be: each repetition of each phase owns a
/// process, which is what makes the phase a cold measurement rather than the
/// second half of somebody else's warm one. `open` and `first_lookup` pay for
/// the same open twice for exactly this reason.
pub struct GenerateFinalize;

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Ingest => WRITE_PHASES[0],
            Phase::Finalize => WRITE_PHASES[1],
        }
    }

    fn wall_ms(self, walked: &HandWalk) -> f64 {
        match self {
            Phase::Ingest => walked.ingest.as_secs_f64() * 1000.0,
            Phase::Finalize => walked.finalize.as_secs_f64() * 1000.0,
        }
    }

    fn heap_peak(self, walked: &HandWalk) -> Option<u64> {
        match self {
            Phase::Ingest => walked.ingest_peak_bytes,
            Phase::Finalize => walked.finalize_peak_bytes,
        }
    }
}

/// One phase's repetitions, all of them whole regenerations.
///
/// The same expensive-and-honest shape `generate` takes: a fresh scratch
/// directory and a whole write per repetition, because a harness that wrote
/// once and timed nothing six more times would publish six samples of one
/// event and call their spread a dispersion.
fn run_phase(phase: Phase, ctx: &ScenarioContext<'_>, reps: u32) -> Result<ScenarioRun, Skip> {
    let plan = ctx
        .cell
        .plan()
        .ok_or_else(|| Skip::failed("the cell does not plan"))?;
    let mut wall = Vec::new();
    let mut facts = Vec::new();
    let mut heap_peak: Option<u64> = None;

    for _ in 0..reps.max(1) {
        let scratch = Scratch::new(ctx.scratch_root)
            .map_err(|e| Skip::failed(format!("no scratch directory: {e}")))?;
        let walked =
            hand_walk(ctx.backend, ctx.cell, &plan, scratch.path()).map_err(Skip::failed)?;
        let occupancy = occupancy(&walked.output);
        let shape = archive_shape(ctx.backend, &walked.output);
        wall.push(phase.wall_ms(&walked));
        heap_peak = match (heap_peak, phase.heap_peak(&walked)) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        facts.push(RepFacts {
            invariants: Invariants {
                output_bytes: occupancy.map(|o| o.0),
                allocated_bytes: occupancy.map(|o| o.3),
                filesystem_entries: occupancy.map(|o| o.1),
                directories: occupancy.map(|o| o.2),
                tiles_produced: Some(walked.tiles_produced),
                // The digest is the load-bearing one. A hand walk that dropped
                // a tile, encoded at a different quality or laid the archive
                // out differently would be timing a cheaper piece of work than
                // the combined row it reconciles against, and this is what
                // says it did not: the same invariants `generate` publishes,
                // on the same cell, from a pass driven a different way.
                artefact_digest: artefact_digest(&walked.output),
                root_entries: shape.map(|s| s.0),
                leaves: shape.map(|s| s.1),
                requests: None,
                request_bytes: None,
            },
            scratch: Some(scratch.path().to_path_buf()),
        });
    }

    Ok(ScenarioRun {
        series: vec![Series {
            metric: WALL,
            samples: wall,
        }],
        reps: facts,
        discarded_warmup: Vec::new(),
        peak_rss_bytes: None,
        heap_peak_bytes: heap_peak,
    })
}

/// The half of the [`Scenario`] contract both phases answer the same way.
macro_rules! write_phase {
    ($ty:ty, $phase:expr) => {
        impl Scenario for $ty {
            fn name(&self) -> String {
                $phase.name().to_string()
            }

            fn isolation(&self) -> Isolation {
                Isolation::ProcessPerRep
            }

            fn warmup(&self) -> Option<Warmup> {
                // A fresh process per repetition is the warm-up, and a
                // discarded pass would throw away the only cold write the
                // process has.
                None
            }

            fn reps(&self, profile: Profile) -> u32 {
                generate_reps(profile)
            }

            fn primary(&self) -> MetricSpec {
                WALL
            }

            fn needs_artefact(&self) -> bool {
                false
            }

            fn run(&self, ctx: &ScenarioContext<'_>, reps: u32) -> Result<ScenarioRun, Skip> {
                run_phase($phase, ctx, reps)
            }
        }
    };
}

write_phase!(GenerateIngest, Phase::Ingest);
write_phase!(GenerateFinalize, Phase::Finalize);

/// Both phases, in the order they happen.
pub fn all() -> Vec<Box<dyn Scenario>> {
    vec![Box::new(GenerateIngest), Box::new(GenerateFinalize)]
}
