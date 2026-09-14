//! The scenarios and cells the old sweep never had.
//!
//! The sweep on `main` walked four cells at 93, 1373, 5469 and 6 root entries,
//! one thread count taken from `available_parallelism`, one source, one codec,
//! and a `read_cold` row that rolled a file open, a header read, a ranged read,
//! a gzip inflate, four varint passes and a lookup into a single number. Two of
//! those holes are already filled in the engine repository by libviprs#1022
//! (the brink cell and the six-phase cold split) and the rest are filled here.
//!
//! # What each module is for
//!
//! * [`open`] carries the cold open, split into the six phases it is made of,
//!   re-homed from `libviprs/tests/pmtiles_benchmarks.rs`. The phases have to
//!   reconcile with the combined row or they are measuring something else, and
//!   the guard that checks that refuses to run on a cell whose root is too
//!   small for the reconciliation to mean anything.
//! * [`first_lookup`] is the other half of the old `read_cold` row: a fresh
//!   process per repetition, so the open it measures is genuinely cold.
//! * [`decode_root`] re-measures the varint loop the cold split attributes most
//!   of the open to, instead of quoting it from somebody's micro-benchmark.
//! * [`plan_order`] and [`tileid_order`] walk the same coordinates two ways.
//!   PMTiles is a Hilbert curve by construction and a plan is a row-major walk,
//!   so those are different orders over one set and the archive's byte order is
//!   only one of them.
//! * [`concurrent_curve`] is a ladder rather than a point. The knee is at four
//!   threads on arm64 and at eight on native x86_64 (libviprs#1024), so a sweep
//!   that measured only `available_parallelism` puts the whole move in the
//!   wrong place.
//! * [`requests`] counts range reads and bytes through a counting
//!   [`RangeReader`](libviprs::pmtiles::RangeReader). That is the quantity the
//!   remote-storage question turns on, and the crate has no HTTP range reader,
//!   so counting is the only honest way to answer it.
//! * [`replicate`] measures one cell first and last in every sweep and
//!   publishes its own spread as the in-run noise floor.
//!
//! # Where the cells and the sources live
//!
//! In [`Cell`] and [`Source`] below, which is not where they belong. K1.2
//! (libviprs-bench#65) owns `cells.rs` and the cell table is its file. Its API
//! had not landed when I wrote this, so the brink cell, the `noise` source and
//! the `flat` dedupe guard live here for now and move across at compose time.
//! Nothing else in this module cares which file they come from.
//!
//! # Nothing here asserts a timing
//!
//! Every function in this module reports a shape: a count, an order, an
//! outcome, a reconciliation. The measured p99 noise floor on an *idle* host is
//! 74.5% across a free replicate pair of one cell at one commit, so a test that
//! compares two timings is a coin toss and a test that compares two timings on
//! a host running four other lanes is not even that. The calibrated sweeps
//! happen in K2.3 and K2.5.

use std::path::Path;

use libviprs::planner::{Layout, PyramidPlan, PyramidPlanner, TileCoord};
use libviprs::pmtiles::zxy_to_tileid;
use libviprs::{PixelFormat, Raster};

pub mod concurrent_curve;
pub mod counting;
pub mod decode_root;
pub mod first_lookup;
pub mod open;
pub mod plan_order;
pub mod replicate;
pub mod requests;
pub mod tileid_order;

// ---------------------------------------------------------------------------
// The writer's cutoff
// ---------------------------------------------------------------------------

/// `ROOT_ONLY_MAX_ENTRIES` from `src/pmtiles/writer.rs` in the engine.
///
/// A copy, because the writer's constant is private. `the_writers_cutoff_is_
/// the_number_this_crate_copied` reads the engine's source and fails when the
/// copy drifts, the same way the engine's own benchmark guard does.
pub const ROOT_ONLY_MAX_ENTRIES: u64 = 16_384;

/// The largest flat root the writer will actually emit, which is one less.
///
/// The writer decides with `if plan.entry_count < ROOT_ONLY_MAX_ENTRIES`, a
/// **strict** comparison, so 16384 entries already spill into leaves and 16383
/// is the biggest flat root there is. libviprs#1021 said 16384 and put its
/// extrapolated peak there. It is one entry out, which changes nothing about
/// the argument and everything about what a cell claiming to sit on the cutoff
/// is allowed to come out as.
pub const LARGEST_FLAT_ROOT: u64 = ROOT_ONLY_MAX_ENTRIES - 1;

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// What fills the raster a cell is generated from.
///
/// Compressibility is the axis the old sweep never had. Its tiles landed at
/// about 95 KB and 6.1 KB by accident of tile size, and both numbers are a
/// property of the gradient rather than of the backends, so an archive that
/// wins on one might lose on the other and nothing in the sweep could see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// The existing deterministic ramp. Compresses like imagery.
    Gradient,
    /// A seeded xorshift fill. Incompressible, so it is the archive's worst
    /// copy case and the tree's largest files.
    Noise,
    /// A solid colour. It exists only as a dedupe guard in the tests and never
    /// produces a published row: every neighbouring tile shares a payload, so
    /// the writer's run-length encoding collapses the root and the cell stops
    /// being the cell it is labelled as. A source planning 16369 tiles comes
    /// out as a root of 261 entries.
    Flat,
    /// The same ramp with its moduli rounded to 256, which makes it repeat
    /// every 256 pixels on both axes.
    ///
    /// Never published either. It is here as the positive control for
    /// [`collapses_at`]: a guard that refuses a source whose period divides the
    /// tile size has to be shown refusing one, and the honest way to show that
    /// is with a source the guard really does refuse rather than with a solid
    /// fill, which collapses for a second reason as well.
    ///
    /// It is not invented for the test. `libviprs_bench::gradient_raster` in
    /// `src/lib.rs`, which the `engines` family tiles, is
    /// `(x % 256, y % 256, (x * 7 + y * 13) % 256)` and has exactly this shape.
    PeriodicGradient,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::Gradient => "gradient",
            Self::Noise => "noise",
            Self::Flat => "flat",
            Self::PeriodicGradient => "periodic-gradient",
        }
    }

    /// Whether a sweep may publish a row measured from this source.
    ///
    /// `flat` may not. It is a control that proves the entry count is a
    /// measurement of the archive rather than a restatement of the tile count,
    /// and a row from it would be labelled with a cell whose regime it does not
    /// have.
    pub fn publishes_rows(self) -> bool {
        !matches!(self, Self::Flat | Self::PeriodicGradient)
    }

    /// The raster a cell of this source generates from.
    pub fn raster(self, width: u32, height: u32) -> Raster {
        match self {
            Self::Gradient => gradient(width, height),
            Self::Noise => noise(width, height),
            Self::Flat => flat(width, height),
            Self::PeriodicGradient => periodic_gradient(width, height),
        }
    }
}

/// Every source a sweep may walk, publishable or not.
pub const SOURCES: [Source; 4] = [
    Source::Gradient,
    Source::Noise,
    Source::Flat,
    Source::PeriodicGradient,
];

/// The deterministic RGB gradient the engine's harness already uses, ported
/// line for line.
///
/// From `tests/common/pmtiles_bench.rs` at `libviprs` `origin/main`, read with
/// `git show origin/main:tests/common/pmtiles_bench.rs` rather than out of a
/// working tree, because the checkout sitting next to this one is parked on a
/// branch that predates the whole `pmtiles` module and does not contain that
/// file at all.
///
/// Copied rather than imported because the engine keeps it in a test-only
/// module reached through `#[path]`, and K2.5 is the lane that decides whether
/// it becomes a `pub` item over there. It has to be the same function, moduli
/// and all: the `storage` family exists to re-home that harness, and a sweep
/// measuring a different source produces numbers that are comparable neither
/// with the published sweep nor with the engine's own guards.
///
/// The three moduli are **prime** and none of them divides 256, and that is the
/// load-bearing part. The x channel repeats every 251 pixels, the y channel
/// every 241, and the third every 239 on both axes, so the smallest tile that
/// could make two neighbouring tiles byte-identical is 251 * 241 * 239 pixels
/// wide. No tile size any sweep uses comes near it, so this source never
/// collapses under the writer's run-length encoding.
pub fn gradient(width: u32, height: u32) -> Raster {
    let mut data = vec![0u8; width as usize * height as usize * 3];
    for y in 0..height {
        for x in 0..width {
            let off = (y as usize * width as usize + x as usize) * 3;
            data[off] = (x % 251) as u8;
            data[off + 1] = (y % 241) as u8;
            data[off + 2] = ((x * 7 + y * 13) % 239) as u8;
        }
    }
    Raster::new(width, height, PixelFormat::Rgb8, data).expect("a gradient raster is well formed")
}

/// The same ramp with power-of-two moduli, which makes it repeat every 256
/// pixels.
///
/// The control that proves [`collapses_at`] can fire, and a copy of a shape that
/// is already in this crate: `libviprs_bench::gradient_raster` is
/// `(x % 256, y % 256, (x * 7 + y * 13) % 256)`.
pub fn periodic_gradient(width: u32, height: u32) -> Raster {
    let mut data = vec![0u8; width as usize * height as usize * 3];
    for y in 0..height {
        for x in 0..width {
            let off = (y as usize * width as usize + x as usize) * 3;
            data[off] = (x % 256) as u8;
            data[off + 1] = (y % 256) as u8;
            data[off + 2] = ((x * 7 + y * 13) % 256) as u8;
        }
    }
    Raster::new(width, height, PixelFormat::Rgb8, data)
        .expect("a periodic gradient raster is well formed")
}

/// A seeded xorshift fill: the incompressible source.
///
/// The point is the codec, not the pixels. A PNG of this is within a few
/// percent of its raw size, so the archive has to copy what the tree has to
/// copy and the comparison stops being a comparison of two compressors.
///
/// `xorshift64star`, seeded off the pixel index so the raster is identical on
/// every host and every architecture and a run is reproducible. Not
/// `rand`: this crate does not depend on it and a benchmark source is exactly
/// the thing that must not move between runs.
pub fn noise(width: u32, height: u32) -> Raster {
    let pixels = (width as usize) * (height as usize);
    let mut data = Vec::with_capacity(pixels * 3);
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    for _ in 0..pixels {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let word = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        data.push(word as u8);
        data.push((word >> 16) as u8);
        data.push((word >> 32) as u8);
    }
    Raster::new(width, height, PixelFormat::Rgb8, data).expect("a noise raster is well formed")
}

/// A solid colour: the dedupe guard, never a published row.
pub fn flat(width: u32, height: u32) -> Raster {
    let data = vec![0x40u8; (width as usize) * (height as usize) * 3];
    Raster::new(width, height, PixelFormat::Rgb8, data).expect("a flat raster is well formed")
}

// ---------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------

/// What shape of PMTiles directory a cell is supposed to come out as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regime {
    /// Every entry lives in a flat root and there are no leaf directories.
    Root,
    /// The entries spilled past the cutoff, so the root is a handful of leaf
    /// pointers and every lookup goes through a leaf.
    Leaves,
}

/// One cell of the sweep: a canvas at a tile size, filled from a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub width: u32,
    pub height: u32,
    pub tile_size: u32,
    pub source: Source,
    /// What the archive is *expected* to come out as. Never what it is: the
    /// regime is checked by opening the archive and asking it, and a cell whose
    /// archive disagrees is a cell that moved, not a test that is wrong.
    pub regime: Regime,
}

impl Cell {
    pub fn spec(&self) -> String {
        format!(
            "{}x{}@{}/{}",
            self.width,
            self.height,
            self.tile_size,
            self.source.label()
        )
    }

    pub fn plan(&self) -> PyramidPlan {
        PyramidPlanner::new(self.width, self.height, self.tile_size, 0, Layout::Xyz)
            .expect("a plan is valid")
            .plan()
    }

    /// How many tiles the planner plans, which is **not** how many entries the
    /// root holds unless no two neighbouring tiles share a payload.
    pub fn planned_tiles(&self) -> u64 {
        self.plan()
            .levels
            .iter()
            .map(|level| u64::from(level.cols) * u64::from(level.rows))
            .sum()
    }

    pub fn with_source(self, source: Source) -> Self {
        Self { source, ..self }
    }
}

/// The canvas whose root stops just under the writer's cutoff.
///
/// 4096 by 6256 pixels at a 46 pixel tile, which is a search result rather than
/// a choice: [`brink_search`] re-runs the search that found it. On a gradient
/// every tile is distinct, so its 16369 planned tiles are also 16369
/// run-length-encoded root entries, fourteen under [`LARGEST_FLAT_ROOT`].
///
/// It is pinned by opening the archive and asking `root_entries()`, never by
/// arithmetic over the planner's halving, because arithmetic over a planner
/// stops describing the brink the day the planner moves.
pub const BRINK_CANVAS: (u32, u32, u32) = (4096, 6256, 46);

/// The smoke cell, and the replicate control: 93 root entries.
pub const SMOKE_CANVAS: (u32, u32, u32) = (2048, 2048, 256);

/// The mid root-only cell: 1373 entries, 95 KB tiles, the photographic end.
pub const MID_CANVAS: (u32, u32, u32) = (8192, 8192, 256);

/// The only leaf-bearing cell: 21851 tiles behind six leaf pointers.
pub const LEAF_CANVAS: (u32, u32, u32) = (8192, 8192, 64);

fn cell(canvas: (u32, u32, u32), source: Source, regime: Regime) -> Cell {
    Cell {
        width: canvas.0,
        height: canvas.1,
        tile_size: canvas.2,
        source,
        regime,
    }
}

/// The cell whose root sits at the top of the open-cost ramp.
pub fn brink_cell(source: Source) -> Cell {
    cell(BRINK_CANVAS, source, Regime::Root)
}

/// The cell on the far side of the cliff, where every lookup goes through a
/// leaf directory.
pub fn leaf_cell(source: Source) -> Cell {
    cell(LEAF_CANVAS, source, Regime::Leaves)
}

/// The smoke cell, measured first and last in every sweep as the drift control.
pub fn smoke_cell(source: Source) -> Cell {
    cell(SMOKE_CANVAS, source, Regime::Root)
}

/// The mid root-only cell.
pub fn mid_cell(source: Source) -> Cell {
    cell(MID_CANVAS, source, Regime::Root)
}

/// Re-run the search that picked [`BRINK_CANVAS`], and answer with the cell it
/// finds and the tiles it plans.
///
/// Tile sizes from 16 pixels, because below that a tile stops resembling
/// anything anybody ships. Canvases up to 4096 on the short edge and twice that
/// on the long one, so the brink cell costs less to generate than the sweep's
/// biggest existing cell rather than more. Heights come from a binary search,
/// which is sound because a plan's tile count never falls as the canvas grows.
///
/// It is arithmetic, and arithmetic is exactly what must not be trusted to pin
/// the cell. This says which cell to build; the archive says what it came out
/// as.
pub fn brink_search() -> (Cell, u64) {
    let mut best: Option<(Cell, u64)> = None;
    for tile_size in 16u32..=256 {
        for width in [256u32, 512, 1024, 2048, 4096] {
            let fits = |height: u32| {
                cell(
                    (width, height, tile_size),
                    Source::Gradient,
                    Regime::Root,
                )
                .planned_tiles()
                    <= LARGEST_FLAT_ROOT
            };
            if !fits(width) {
                continue;
            }
            let (mut low, mut high) = (width, 2 * width);
            while low < high {
                let mid = low + (high - low).div_ceil(2);
                if fits(mid) {
                    low = mid;
                } else {
                    high = mid - 1;
                }
            }
            let found = cell((width, low, tile_size), Source::Gradient, Regime::Root);
            let tiles = found.planned_tiles();
            if best.is_none_or(|(_, most)| tiles > most) {
                best = Some((found, tiles));
            }
        }
    }
    best.expect("some cell in the search space plans a pyramid")
}

// ---------------------------------------------------------------------------
// Coordinates and tile ids
// ---------------------------------------------------------------------------

/// Every coordinate the plan covers, in level then row then column order.
///
/// This is the order a writer walks and the order the old `read_sequential`
/// scenario read in. It is a row-major walk of each level, and the archive's
/// bytes are in Hilbert order, so the two are different walks over one set.
pub fn plan_coordinates(plan: &PyramidPlan) -> Vec<TileCoord> {
    let mut out = Vec::new();
    for level in &plan.levels {
        for row in 0..level.rows {
            for col in 0..level.cols {
                out.push(TileCoord {
                    level: level.level,
                    col,
                    row,
                });
            }
        }
    }
    out
}

/// The PMTiles tile id a coordinate addresses, which is the archive's byte
/// order.
///
/// `None` for a coordinate PMTiles cannot address, which is the same answer
/// [`PmTilesPyramidReader`](libviprs::pyramid_reader::PmTilesPyramidReader)
/// gives: not an error, just a tile this pyramid does not have.
pub fn tile_id(coord: TileCoord) -> Option<u64> {
    let z = u8::try_from(coord.level).ok()?;
    zxy_to_tileid(z, coord.col, coord.row).ok()
}

// ---------------------------------------------------------------------------
// Outcomes, and where a number came from
// ---------------------------------------------------------------------------

/// Whether a published quantity was observed or declared.
///
/// The directory backend's request count is one object per tile, and nobody
/// measured that: there is no range reader under a `std::fs::read`. It is a
/// declaration, it is correct, and it must never render as a measurement beside
/// the archive's observed counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Observed,
    Declared,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Declared => "declared",
        }
    }

    pub fn is_declared(self) -> bool {
        matches!(self, Self::Declared)
    }
}

/// What happened to one arm of a sweep.
///
/// A cell the host cannot measure is `Skipped` with a reason and stays in the
/// document. Dropping it reads as a missing measurement, which is a different
/// and worse claim than a declined one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Skipped { reason: String },
}

impl Outcome {
    pub fn skipped(reason: impl Into<String>) -> Self {
        Self::Skipped {
            reason: reason.into(),
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Ok => None,
            Self::Skipped { reason } => Some(reason),
        }
    }
}

// ---------------------------------------------------------------------------
// Asking an archive what it came out as
// ---------------------------------------------------------------------------

/// What an archive's root really holds: total entries, and how many of them are
/// leaf pointers.
///
/// The one function in this module that answers the brink question, and it
/// answers it by opening the archive. Every other route to that number is
/// arithmetic over a planner.
pub fn root_shape(archive: &Path) -> (u64, u64) {
    use libviprs::pyramid_reader::PmTilesPyramidReader;

    let reader = PmTilesPyramidReader::try_open(archive).expect("the archive opens for reading");
    let root = reader.reader().root_entries();
    let leaves = root.iter().filter(|entry| entry.is_leaf()).count();
    (root.len() as u64, leaves as u64)
}

/// Which regime an archive is actually in, from its root shape.
pub fn observed_regime(archive: &Path) -> Regime {
    let (_, leaves) = root_shape(archive);
    if leaves == 0 { Regime::Root } else { Regime::Leaves }
}

// ---------------------------------------------------------------------------
// The random walk
// ---------------------------------------------------------------------------

/// The seed every shuffle in this family uses.
///
/// One constant, written down, because a random walk whose order moves between
/// runs is a different amount of work each time and nothing downstream could
/// tell that from a regression.
pub const SEED: u64 = 0x5EED_1234_ABCD_0001;

/// `splitmix64`, eight lines and no dependency.
#[derive(Debug, Clone, Copy)]
pub struct Splitmix(u64);

impl Splitmix {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }
}

/// The same coordinates in a seeded shuffle.
///
/// This is `read_random`'s order, and it is also the order the concurrent curve
/// splits into chunks, which is what makes the T=1 rung the same work as
/// `read_random` rather than a second coordinate set that happens to be the
/// same length.
pub fn random_order(coords: &[TileCoord], seed: u64) -> Vec<TileCoord> {
    let mut out = coords.to_vec();
    let mut rng = Splitmix::new(seed);
    for index in (1..out.len()).rev() {
        let swap = rng.below(index + 1);
        out.swap(index, swap);
    }
    out
}


// ---------------------------------------------------------------------------
// A source's period, which is not a detail
// ---------------------------------------------------------------------------

/// How many pixels a source runs for before it repeats on both axes, when it
/// repeats at all.
///
/// It matters because the writer counts **run-length-encoded entries**. A
/// level's tile ids are one contiguous range, so if every tile of a level is
/// byte-identical the writer merges the lot into a single entry and the cell
/// stops being the cell its tile count says it is.
///
/// * [`Source::Gradient`] is `(x % 251, y % 241, (x * 7 + y * 13) % 239)`. Three
///   primes, none of them a factor of any tile size anybody uses, so the
///   smallest repeat is `251 * 241 * 239 = 14_457_349` pixels.
/// * [`Source::PeriodicGradient`] is the same ramp at 256, so it repeats every
///   256 pixels.
/// * [`Source::Flat`] repeats every pixel.
/// * [`Source::Noise`] is a seeded xorshift walk over the whole raster, so it
///   never repeats inside a canvas anybody can allocate. `None`.
pub fn period_px(source: Source) -> Option<u32> {
    match source {
        Source::Gradient => Some(251 * 241 * 239),
        Source::PeriodicGradient => Some(256),
        Source::Flat => Some(1),
        Source::Noise => None,
    }
}

/// Whether a tile size makes every tile of a level byte-identical for this
/// source.
///
/// It does exactly when the tile is a whole number of the source's periods
/// wide. Then tile `(i, j)` covers `x` in `[t*i, t*i + t)`, the source runs
/// through the same values for every `i`, and the same on the other axis, so
/// every tile at that level is the same bytes and the level collapses to one
/// entry.
///
/// Measured, on this crate's own archives, by
/// `the_measured_root_entries_of_every_source`:
///
/// | cell | planned tiles | gradient | noise | periodic gradient | flat |
/// |---|---|---|---|---|---|
/// | 1024x1024@256 | 29 | 29 | 29 | 11 | 11 |
/// | 1024x1024@128 | 92 | 92 | 92 | 74 | 11 |
/// | 1024x1024@64 | 347 | 347 | 347 | 329 | 11 |
/// | 1024x1024@46 | 728 | 728 | 728 | 728 | 80 |
/// | 2048x2048@256 | 93 | 93 | 93 | 12 | 12 |
///
/// The engine's gradient pays one entry a tile everywhere, which is what the
/// open-cost ramp's x axis rests on. The periodic ramp collapses to one entry a
/// level wherever 256 divides the tile, and to 74 and 329 at 128 and 64, where
/// the identical tiles exist but their ids are not adjacent in Hilbert order.
pub fn collapses_at(source: Source, tile_size: u32) -> bool {
    period_px(source).is_some_and(|period| tile_size % period == 0)
}

/// Whether a cell's declared shape survives the source it is filled from.
///
/// The open-cost ramp is a function of **root entries**, and a cell's tile count
/// is only the same number while no run of neighbouring tiles shares a payload.
/// A source whose period divides the tile size shares every payload in the
/// level, so such a cell sits at one entry per level however many tiles it
/// plans, and a table that names it by its tile count is naming a point that is
/// not on the ramp.
///
/// With the engine's gradient this never fires on any cell in the family, which
/// is the point: it is a regression test rather than a refusal anybody hits, and
/// `the_ported_gradient_does_not_collapse_at_any_tile_size_the_sweep_uses`
/// proves it can still fire by handing it a source that does collapse.
pub fn source_suits_the_cell(cell: &Cell) -> Result<(), String> {
    if let Some(period) = period_px(cell.source)
        && collapses_at(cell.source, cell.tile_size)
    {
        return Err(format!(
            "{} fills a {} pixel tile from a source whose period is {period} pixels on both axes, \
             so every tile of a level is the same bytes and the writer's run-length encoding \
             collapses the level to one entry; the cell plans {} tiles and its root will hold \
             about one entry per level",
            cell.spec(),
            cell.tile_size,
            cell.planned_tiles()
        ));
    }
    Ok(())
}

/// The cells a sweep may publish rows for.
///
/// Everything except the sources that exist only as controls. It is filtered
/// here, once, rather than by every consumer remembering to, because the failure
/// mode is a row labelled with a cell whose regime it does not have and nothing
/// downstream can see that.
pub fn publishable(cells: &[Cell]) -> Vec<Cell> {
    cells
        .iter()
        .copied()
        .filter(|cell| cell.source.publishes_rows())
        .collect()
}
