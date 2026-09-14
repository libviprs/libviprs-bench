//! What a scenario is, and the seam a scenario measures through.
//!
//! A scenario is one measurable thing done to one `(backend, cell)` pair. It
//! declares how its repetitions are isolated, whether it discards a warm-up,
//! how many repetitions it wants, and which metric series it publishes; then
//! it runs and hands back one sample per repetition per series.
//!
//! # Two isolations, and why the choice is the scenario's
//!
//! [`Isolation::ProcessPerRep`] gives every repetition a process that has
//! never touched the artefact, which is what `open` and `first_lookup` mean
//! by their names. [`Isolation::ProcessPerScenario`] gives the whole scenario
//! one process and lets the repetitions happen inside it, which is what a
//! pass scenario wants: twenty passes over the same coordinate set with one
//! discarded warm-up in front of them.
//!
//! Both are stricter than what the harness this replaces did, which ran every
//! read scenario for a cell in one process on one reader, so `read_random`
//! walked a reader `read_sequential` had just warmed and the row was reported
//! as a cold-ish random read. A scenario never gets a reader; it gets a
//! [`ReaderFactory`] and asks for a fresh one, and the factory is the seam a
//! test substitutes a counting `RangeReader` at.
//!
//! # Every rep carries its own facts
//!
//! [`ScenarioRun::reps`] is per repetition, not per scenario, because
//! "seven reps agreed on the digest" is a different claim from "one artefact
//! was hashed once and copied seven times", and only the per-rep shape can
//! tell them apart.

pub mod reference;

// The scenarios and cells the old sweep never had (issue #67). Each one is a
// `Scenario` over the contract above, so nothing here constructs a reader: a
// scenario that wants a cold one asks `ReaderFactory::fresh()` again, and the
// request-counting proof works by putting a counting factory behind that seam
// rather than by reaching around it.
pub mod concurrent_curve;
pub mod counting;
pub mod decode_root;
pub mod first_lookup;
pub mod open;
pub mod plan_order;
pub mod replicate;
pub mod requests;
pub mod tileid_order;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use libviprs::planner::TileCoord;
use serde::{Deserialize, Serialize};

use super::cells::{Backend, Cell, Profile};

// ---------------------------------------------------------------------------
// Declaring a scenario
// ---------------------------------------------------------------------------

/// How a scenario's repetitions are isolated from one another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    /// A fresh child process per repetition. The scenario body runs once per
    /// process and the parent concatenates the samples.
    ProcessPerRep,
    /// One child process for the scenario; the repetitions and the warm-up
    /// happen inside it.
    ProcessPerScenario,
}

impl Isolation {
    pub fn as_str(self) -> &'static str {
        match self {
            Isolation::ProcessPerRep => "process-per-rep",
            Isolation::ProcessPerScenario => "process-per-scenario",
        }
    }
}

/// What a scenario discards before it starts measuring.
///
/// `None` on a scenario means it measures from the first repetition, which is
/// only honest when the repetition owns a fresh process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Warmup {
    /// The policy, published verbatim in the document.
    pub policy: &'static str,
    /// How many repetitions are run and thrown away.
    pub passes: u32,
}

impl Warmup {
    /// The one policy this crate has: a single discarded pass.
    pub const ONE_DISCARDED_PASS: Warmup = Warmup {
        policy: "one-discarded-pass",
        passes: 1,
    };
}

/// Which way is better on a metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    LowerIsBetter,
    HigherIsBetter,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::LowerIsBetter => "lower-is-better",
            Direction::HigherIsBetter => "higher-is-better",
        }
    }
}

/// The unit a series is in. The document never converts silently: a series
/// says what it is and the page's config decides how to display it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Milliseconds,
    Microseconds,
    PerSecond,
    Bytes,
    Count,
    Ratio,
}

impl Unit {
    pub fn as_str(self) -> &'static str {
        match self {
            Unit::Milliseconds => "ms",
            Unit::Microseconds => "us",
            Unit::PerSecond => "1/s",
            Unit::Bytes => "bytes",
            Unit::Count => "count",
            Unit::Ratio => "ratio",
        }
    }
}

/// One metric a scenario publishes a sample series for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricSpec {
    /// The suffix the document's scenario key carries, e.g. `p50` in
    /// `read_random.p50`.
    pub name: &'static str,
    pub unit: Unit,
    pub direction: Direction,
}

// ---------------------------------------------------------------------------
// Running a scenario
// ---------------------------------------------------------------------------

/// The coordinate sets a read scenario walks.
///
/// Built once per cell from the plan so every backend and every scenario in a
/// sweep addresses the same tiles, and substitutable in a test so a fabricated
/// archive can be walked without a raster behind it.
#[derive(Debug, Clone, Default)]
pub struct Coordinates {
    /// Level, row, column order.
    pub plan_order: Vec<TileCoord>,
    /// The same set, in the archive's own byte order.
    pub tileid_order: Vec<TileCoord>,
    /// The same set, seeded shuffle.
    pub random: Vec<TileCoord>,
    /// A coordinate the archive answers straight from its root directory.
    pub root_addressed: Option<TileCoord>,
    /// A coordinate the archive can only answer through a leaf directory.
    /// `None` on a cell whose archive is root-only, which is most of them.
    pub leaf_addressed: Option<TileCoord>,
}

/// One tile lookup, whatever is behind it.
///
/// Narrower than `libviprs::pyramid_reader::PyramidReader` on purpose:
/// `PmTilesPyramidReader::from_reader` takes only a
/// `Reader<FileRangeReader>`, so a counting `RangeReader` cannot be dressed
/// as one. This trait is what a scenario needs and it is implementable over
/// anything, which is what makes the request-counting proof possible.
pub trait TileReader: Send + Sync {
    fn tile(&self, coord: TileCoord) -> Result<Option<Vec<u8>>, String>;
}

/// Where a scenario gets a reader.
///
/// Every call builds one that has never served a lookup. A scenario that
/// wants a warm reader calls [`ReaderFactory::fresh`] once and keeps it; a
/// scenario that wants a cold one calls again. Nothing hands a scenario a
/// reader someone else used.
pub trait ReaderFactory: Send + Sync {
    fn fresh(&self) -> Result<Arc<dyn TileReader>, String>;
}

/// Everything a scenario is given.
pub struct ScenarioContext<'a> {
    pub backend: Backend,
    pub cell: Cell,
    pub profile: Profile,
    pub seed: u64,
    /// Where a repetition that needs its own scratch space may make one.
    /// `None` means the system temporary directory.
    pub scratch_root: Option<&'a Path>,
    /// The artefact read scenarios read: the archive file, or the root of the
    /// tree. `None` for `generate`, which makes one per repetition.
    pub artefact: Option<&'a Path>,
    pub coords: &'a Coordinates,
    pub readers: &'a dyn ReaderFactory,
}

/// The facts one repetition observed about the artefact it touched.
///
/// Every field is an `Option` and an unmeasured one is `None`, which reaches
/// the document as `null`. A `filesystem_entries: 0` is a smaller number than
/// the `1` a real archive costs, on the one column this whole comparison
/// exists to move, so a broken measurement that published a zero would
/// publish a win.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invariants {
    pub output_bytes: Option<u64>,
    pub allocated_bytes: Option<u64>,
    pub filesystem_entries: Option<u64>,
    pub directories: Option<u64>,
    pub tiles_produced: Option<u64>,
    pub artefact_digest: Option<String>,
    pub root_entries: Option<u64>,
    pub leaves: Option<u64>,
    pub requests: Option<u64>,
    pub request_bytes: Option<u64>,
}

/// What one repetition left behind.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepFacts {
    pub invariants: Invariants,
    /// The scratch directory this repetition built into, where it built one.
    /// Two repetitions that report the same path did not regenerate.
    pub scratch: Option<PathBuf>,
}

/// One metric's samples: exactly one per timed repetition.
#[derive(Debug, Clone)]
pub struct Series {
    pub metric: MetricSpec,
    pub samples: Vec<f64>,
}

/// What a scenario hands back.
#[derive(Debug, Clone)]
pub struct ScenarioRun {
    /// Primary series first.
    pub series: Vec<Series>,
    /// One entry per timed repetition, in order.
    pub reps: Vec<RepFacts>,
    /// The primary-metric values of the discarded warm-up passes, kept so the
    /// document can prove they are not in `samples`.
    pub discarded_warmup: Vec<f64>,
    pub peak_rss_bytes: Option<u64>,
    pub heap_peak_bytes: Option<u64>,
}

/// Why a cell is not `ok`.
///
/// K1.3 owns the aggregator that refuses on these; the taxonomy is here
/// because the runner is what produces them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Skipped,
    Failed,
    Refused,
    Timeout,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Skipped => "skipped",
            Outcome::Failed => "failed",
            Outcome::Refused => "refused",
            Outcome::Timeout => "timeout",
        }
    }
}

/// A cell that produced no samples, and why. A non-`ok` outcome without a
/// reason is refused downstream.
#[derive(Debug, Clone)]
pub struct Skip {
    pub outcome: Outcome,
    pub reason: String,
}

impl Skip {
    pub fn skipped(reason: impl Into<String>) -> Skip {
        Skip {
            outcome: Outcome::Skipped,
            reason: reason.into(),
        }
    }

    pub fn failed(reason: impl Into<String>) -> Skip {
        Skip {
            outcome: Outcome::Failed,
            reason: reason.into(),
        }
    }
}

/// One measurable thing done to one `(backend, cell)` pair.
pub trait Scenario: Send + Sync {
    /// The name the document keys on, e.g. `read_random` or
    /// `read_concurrent@4`.
    fn name(&self) -> String;

    fn isolation(&self) -> Isolation;

    /// `None` when the scenario measures from its first repetition, which is
    /// only honest under [`Isolation::ProcessPerRep`].
    fn warmup(&self) -> Option<Warmup>;

    /// Timed repetitions on this profile.
    fn reps(&self, profile: Profile) -> u32;

    /// Below this the cell is published low-confidence rather than dropped.
    fn min_reps(&self, profile: Profile) -> u32 {
        self.reps(profile)
    }

    /// The metric the row is ranked on.
    fn primary(&self) -> MetricSpec;

    /// Every series the scenario publishes, primary first.
    fn series(&self) -> Vec<MetricSpec> {
        vec![self.primary()]
    }

    /// Whether the parent has to generate an artefact before this scenario
    /// can run.
    fn needs_artefact(&self) -> bool {
        true
    }

    /// Run the declared warm-up and then `reps` timed repetitions, in this
    /// process.
    fn run(&self, ctx: &ScenarioContext<'_>, reps: u32) -> Result<ScenarioRun, Skip>;
}
