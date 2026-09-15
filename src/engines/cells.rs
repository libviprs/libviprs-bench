//! What an `engines` sweep walks: the three engines, the canvases and the
//! thread budgets, and which of them a profile takes.
//!
//! A *cell* here is `(canvas, tile size, thread budget)`, and every engine is
//! measured in each one. That is the comparison: the same plan, the same tiles,
//! the same sink, three different ways of getting there. The facet key is the
//! number of tiles the plan holds, asked of the planner rather than declared,
//! which is the same rule [`crate::storage::cells`] follows and for the same
//! reason: a chart ordered by megapixels puts a small canvas at a small tile
//! size in the wrong place, and the archive's shape follows from the entry
//! count.
//!
//! The thread budget is part of the cell and never mixed into one series with
//! another budget. A single-threaded row and an all-cores row measure different
//! things and the old sweep's own comment says so (issue #156); putting the
//! budget in the cell key is what makes that structural rather than a rule
//! somebody has to remember.

use libviprs::planner::{Layout, PyramidPlan, PyramidPlanner};

use crate::harness::Engine;

/// The tile edge every cell uses. One value, because the engines comparison is
/// about the engine and a second tile size would double the sweep to answer a
/// question the `storage` family already answers.
pub const TILE_SIZE: u32 = 256;

/// Floor on the streaming / mapreduce memory budget, raised per canvas width by
/// [`crate::streaming_budget_for`] so the worst-case tile-aligned strip always
/// fits under the strict `BudgetPolicy::Error` those engines run with.
///
/// The same 4 MB the `scalability` sweep uses, so a cell measured here and a
/// point measured there are the same work.
pub const STREAMING_BUDGET_FLOOR: u64 = 4_000_000;

/// The three engines, in pipeline order.
pub const ENGINES: [Engine; 3] = [Engine::Monolithic, Engine::Streaming, Engine::MapReduce];

/// The raster behind every cell.
///
/// `gradient`, named in the document so a reader is never left to infer it. The
/// workload is [`crate::gradient_raster`], a synthetic gradient sized to the
/// `43551_California_South.pdf` page's 1.42:1 aspect, and it is NOT a rasterised
/// blueprint: the fixture is not committed. The name travels with every row so
/// nobody reads these numbers as real-content ones.
pub const SOURCE: &str = "gradient";

/// One cell of the sweep: a canvas at a thread budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineCell {
    pub width: u32,
    pub height: u32,
    pub tile_px: u32,
    /// The thread budget every engine in this cell runs at.
    pub concurrency: usize,
}

impl EngineCell {
    pub fn new(width: u32, height: u32, concurrency: usize) -> EngineCell {
        EngineCell {
            width,
            height,
            tile_px: TILE_SIZE,
            concurrency,
        }
    }

    /// `<width>x<height>@<tile>+c<threads>`, the cell's name in the document.
    pub fn spec(&self) -> String {
        format!(
            "{}x{}@{}+c{}",
            self.width, self.height, self.tile_px, self.concurrency
        )
    }

    /// The pyramid plan for this cell.
    ///
    /// `Layout::DeepZoom`, which is what [`crate::harness::run_single_cell`]
    /// plans with. A cell that planned one layout and was measured under
    /// another would publish a tile count for a pyramid nobody built.
    pub fn plan(&self) -> Option<PyramidPlan> {
        Some(
            PyramidPlanner::new(self.width, self.height, self.tile_px, 0, Layout::DeepZoom)
                .ok()?
                .plan(),
        )
    }

    /// How many tiles the planner really lays out, asked of the planner.
    ///
    /// This is the facet key. It is never the engine's `tiles_produced`: that
    /// is a *measurement* and it is one of the invariants, so deriving the key
    /// the chart sorts on from it would make the axis move whenever the
    /// measurement did.
    pub fn planned_tiles(&self) -> Option<u32> {
        let plan = self.plan()?;
        Some(
            plan.levels
                .iter()
                .map(|level| u64::from(level.rows) * u64::from(level.cols))
                .sum::<u64>() as u32,
        )
    }

    /// Megapixels, for the human-facing progress line only. Never a facet key.
    pub fn megapixels(&self) -> f64 {
        f64::from(self.width) * f64::from(self.height) / 1_000_000.0
    }

    /// The effective streaming / mapreduce budget for this canvas.
    pub fn budget_bytes(&self) -> u64 {
        crate::streaming_budget_for(STREAMING_BUDGET_FLOOR, self.width, self.tile_px, 3)
    }

    /// The child spec that measures one engine in this cell.
    pub fn spec_for(&self, engine: Engine) -> crate::harness::CellSpec {
        crate::harness::CellSpec {
            engine,
            width: self.width,
            height: self.height,
            concurrency: self.concurrency,
            tile_size: self.tile_px,
            budget_bytes: self.budget_bytes(),
        }
    }
}

/// Which sweep a run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Cheap enough that a pull request runs it. Never published.
    Ci,
    /// The published sweep.
    Full,
    /// `Full` plus the two canvases whose monolithic peak needs more than a
    /// default container has. Opt-in.
    Xl,
}

impl Profile {
    pub fn label(self) -> &'static str {
        match self {
            Profile::Ci => "ci",
            Profile::Full => "full",
            Profile::Xl => "xl",
        }
    }

    pub fn parse(s: &str) -> Option<Profile> {
        match s {
            "ci" => Some(Profile::Ci),
            "full" => Some(Profile::Full),
            "xl" => Some(Profile::Xl),
            _ => None,
        }
    }

    pub fn from_env() -> Profile {
        std::env::var("LIBVIPRS_BENCH_PROFILE")
            .ok()
            .and_then(|v| Profile::parse(&v))
            .unwrap_or(Profile::Ci)
    }

    /// Whether a row measured under this profile may be published.
    ///
    /// `ci` proves the harness runs: one small canvas at one thread budget,
    /// three timed repetitions. Three repetitions of a 0.7 MP pyramid say
    /// nothing about how the engines scale, and a page that charted them beside
    /// a calibrated sweep would be charting a smoke test.
    pub fn publishable(self) -> bool {
        !matches!(self, Profile::Ci)
    }

    /// Timed repetitions per cell.
    ///
    /// Seven on a published profile, which is the floor the harness has
    /// declared since it grew statistics at all and the smallest sample an
    /// interquartile range means anything over. Three on `ci`, where the point
    /// is that the repetition machinery runs rather than what it measures.
    pub fn reps(self) -> u32 {
        match self {
            Profile::Ci => 3,
            Profile::Full | Profile::Xl => 7,
        }
    }

    /// Discarded warm-up passes per cell.
    ///
    /// One, always. Every repetition owns a fresh process, so the warm-up is
    /// not about a warm allocator: it is about the page cache behind the source
    /// raster and the sink directory, and the first pass of a cell pays for
    /// both.
    pub fn warmup(self) -> u32 {
        1
    }

    /// The canvases this profile walks, largest last.
    ///
    /// The `full` list is the `scalability` sweep's, unchanged, so the archived
    /// document and the sweep it replaces cover the same ground: 0.18 MP up to
    /// 100.8 MP, spanning the sub-megapixel regime where fixed setup costs
    /// dominate through the sizes where the monolithic canvas allocation is the
    /// whole story.
    ///
    /// The two largest, 188.7 MP and 280 MP, are `xl` rather than `full`. Their
    /// monolithic peak RSS measured 1.2 GB and 1.8 GB in the first capture, and
    /// with one child per repetition that is a per-child figure a 2 GB
    /// container will not give; a sweep that dies two thirds of the way in
    /// archives nothing at all. They stay one flag away rather than deleted,
    /// because the memory story is sharpest where the canvas is biggest.
    pub fn canvases(self) -> Vec<(u32, u32)> {
        let full: Vec<(u32, u32)> = vec![
            (512, 360),
            (1024, 720),
            (2048, 1440),
            (4096, 2880),
            // The full California South page at 72 DPI.
            (4608, 3240),
            (8192, 5760),
            (10000, 7000),
            (12000, 8400),
        ];
        match self {
            Profile::Ci => vec![(1024, 720)],
            Profile::Full => full,
            Profile::Xl => {
                let mut out = full;
                out.push((16384, 11520));
                out.push((20000, 14000));
                out
            }
        }
    }

    /// The thread budgets this profile measures every engine at.
    ///
    /// One and all cores, never mixed into one series. `ci` takes one thread
    /// only: an all-cores row on a two-core runner and an all-cores row on a
    /// ten-core laptop are different measurements, and the profile that exists
    /// to prove the harness runs should not depend on which.
    pub fn concurrency_levels(self) -> Vec<usize> {
        match self {
            Profile::Ci => vec![1],
            Profile::Full | Profile::Xl => {
                let ncpu = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4);
                if ncpu > 1 { vec![1, ncpu] } else { vec![1] }
            }
        }
    }

    /// The cell measured first and last in the sweep, or `None`.
    ///
    /// The smallest canvas at one thread, because the control is paid for twice
    /// and the cheapest cell is the one to pay for. `ci` has no control: it is
    /// never published, so it has no noise floor to publish either, and
    /// measuring its one cell twice would double the smoke test to say nothing.
    pub fn replicate_cell(self) -> Option<EngineCell> {
        if self == Profile::Ci {
            return None;
        }
        let (width, height) = *self.canvases().first()?;
        Some(EngineCell::new(width, height, 1))
    }

    /// Every cell this profile walks, canvas-major, with the control at both
    /// ends.
    ///
    /// First and last rather than twice in a row, because what the spread is
    /// trying to see is drift across the sweep: thermal, a neighbour waking up,
    /// the page cache filling. Two measurements back to back would see none of
    /// it and would publish a flatteringly small noise floor.
    pub fn cells(self) -> Vec<EngineCell> {
        let mut out = Vec::new();
        for concurrency in self.concurrency_levels() {
            for (width, height) in self.canvases() {
                out.push(EngineCell::new(width, height, concurrency));
            }
        }
        let Some(control) = self.replicate_cell() else {
            return out;
        };
        let rest: Vec<EngineCell> = out.into_iter().filter(|c| *c != control).collect();
        crate::storage::scenarios::replicate::schedule(control, &rest)
    }
}
