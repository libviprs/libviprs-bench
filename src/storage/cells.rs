//! What a `storage` sweep walks: backends, sources, cells and profiles.
//!
//! A *cell* is a categorical facet, and its identity is the number of tiles
//! its plan holds. That is deliberate and it is the one ordering rule this
//! module enforces: the archive's directory shape follows from the entry
//! count, so two canvases that plan the same number of tiles are in the same
//! regime however many pixels are behind them, and a chart ordered by
//! megapixels puts 8192@64 between 8192@256 and 16384@256 where it does not
//! belong.
//!
//! The declared tile count is a claim about the planner, so
//! [`Cell::planned_tiles`] asks the planner rather than trusting the
//! declaration, and a test pins the two together.

use libviprs::planner::{Layout, PyramidPlan, PyramidPlanner, TileCoord};

/// Which storage backend a row measures.
///
/// This is the comparison. The engine is `libviprs` on both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Backend {
    /// One archive file.
    PmTiles,
    /// A tree of loose tile files under the layout's own paths.
    Directory,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::PmTiles => "pmtiles",
            Backend::Directory => "directory",
        }
    }

    pub fn parse(s: &str) -> Option<Backend> {
        match s {
            "pmtiles" => Some(Backend::PmTiles),
            "directory" => Some(Backend::Directory),
            _ => None,
        }
    }

    /// Both backends, in the order a sweep interleaves them.
    pub const ALL: [Backend; 2] = [Backend::PmTiles, Backend::Directory];
}

/// What the raster under a cell is.
///
/// `Gradient` compresses like imagery, `Noise` does not and is the archive's
/// worst copy case, `Flat` exists only as a dedupe guard in the tests and
/// never produces a published row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    Gradient,
    Noise,
    Flat,
    /// The gradient with its moduli rounded to 256, so it repeats every 256
    /// pixels on both axes.
    ///
    /// Never published. It is the positive control for [`collapses_at`], and it
    /// is not invented for the test: `libviprs_bench::gradient_raster` in
    /// `src/lib.rs` is `(x % 256, y % 256, (x * 7 + y * 13) % 256)` and has
    /// exactly this shape, so measuring this source measures what that costs.
    PeriodicGradient,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Gradient => "gradient",
            Source::Noise => "noise",
            Source::Flat => "flat",
            Source::PeriodicGradient => "periodic-gradient",
        }
    }

    pub fn parse(s: &str) -> Option<Source> {
        match s {
            "gradient" => Some(Source::Gradient),
            "noise" => Some(Source::Noise),
            "flat" => Some(Source::Flat),
            "periodic-gradient" => Some(Source::PeriodicGradient),
            _ => None,
        }
    }

    /// Whether a row measured on this source may be published.
    pub fn publishable(self) -> bool {
        !matches!(self, Source::Flat | Source::PeriodicGradient)
    }
}

/// One cell of the sweep.
///
/// `declared_tiles` is the facet key the page sorts on and the name the cell
/// is known by. It is checked against [`Cell::planned_tiles`] rather than
/// trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub width: u32,
    pub height: u32,
    pub tile_px: u32,
    pub source: Source,
    /// The tile count this cell is named by, and the facet key.
    pub declared_tiles: u32,
}

impl Cell {
    pub fn new(width: u32, height: u32, tile_px: u32, source: Source, declared_tiles: u32) -> Cell {
        Cell {
            width,
            height,
            tile_px,
            source,
            declared_tiles,
        }
    }

    /// `<width>x<height>@<tile>+<source>`, the form a parent hands a child.
    pub fn spec(&self) -> String {
        format!(
            "{}x{}@{}+{}",
            self.width,
            self.height,
            self.tile_px,
            self.source.as_str()
        )
    }

    /// Parse the form [`Cell::spec`] writes. The declared tile count is not in
    /// the spec: a child asks the planner for it, so a child and its parent
    /// cannot disagree about it by way of a typo in an argv string.
    pub fn parse(spec: &str) -> Option<Cell> {
        let (canvas, source) = spec.split_once('+')?;
        let (dims, tile) = canvas.split_once('@')?;
        let (w, h) = dims.split_once('x')?;
        let mut cell = Cell {
            width: w.parse().ok()?,
            height: h.parse().ok()?,
            tile_px: tile.parse().ok()?,
            source: Source::parse(source)?,
            declared_tiles: 0,
        };
        cell.declared_tiles = cell.planned_tiles()? as u32;
        Some(cell)
    }

    /// The pyramid plan for this cell.
    pub fn plan(&self) -> Option<PyramidPlan> {
        Some(
            PyramidPlanner::new(self.width, self.height, self.tile_px, 0, Layout::Xyz)
                .ok()?
                .plan(),
        )
    }

    /// How many tiles the planner really lays out, asked of the planner.
    pub fn planned_tiles(&self) -> Option<usize> {
        Some(coordinates(&self.plan()?).len())
    }
}

/// Every coordinate a plan covers, in level then row then column order.
pub fn coordinates(plan: &PyramidPlan) -> Vec<TileCoord> {
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

/// Which sweep a run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Cheap enough that a pull request runs it, never published.
    Ci,
    /// The published sweep.
    Full,
    /// `Full` plus the 16384 canvas, opt-in.
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

    /// The cells this profile walks, in facet order.
    ///
    /// K1.4 adds the 16263 brink cell and the `noise` source rows; what is
    /// here is the set the old harness already measured, re-expressed on the
    /// tile-count facet.
    pub fn cells(self) -> Vec<Cell> {
        match self {
            Profile::Ci => vec![Cell::new(2048, 2048, 256, Source::Gradient, 93)],
            Profile::Full => vec![
                Cell::new(2048, 2048, 256, Source::Gradient, 93),
                Cell::new(8192, 8192, 256, Source::Gradient, 1373),
                Cell::new(8192, 8192, 64, Source::Gradient, 21851),
            ],
            Profile::Xl => {
                let mut cells = Profile::Full.cells();
                cells.push(Cell::new(16384, 16384, 256, Source::Gradient, 5469));
                cells
            }
        }
    }

    /// How many lookups one pass performs.
    pub fn read_samples(self) -> usize {
        match self {
            Profile::Ci => 512,
            Profile::Full | Profile::Xl => 20_000,
        }
    }

    /// Whether a run on this profile may be archived and charted.
    pub fn publishable(self) -> bool {
        !matches!(self, Profile::Ci)
    }
}

/// The seed every deterministic choice in a sweep comes from.
pub const SEED: u64 = 0x5EED_1234_ABCD_0001;

// ---------------------------------------------------------------------------
// A source's period, and the cells that live around the writer's cutoff
// (issue #67)
// ---------------------------------------------------------------------------

/// `ROOT_ONLY_MAX_ENTRIES` from `src/pmtiles/writer.rs` in the engine.
///
/// A copy, because the writer's constant is private.
/// `the_writers_cutoff_is_the_number_this_crate_copied` reads the engine's
/// source and fails when the copy drifts.
pub const ROOT_ONLY_MAX_ENTRIES: u64 = 16_384;

/// The largest flat root the writer will actually emit, which is one less.
///
/// The writer decides with `if plan.entry_count < ROOT_ONLY_MAX_ENTRIES`, a
/// **strict** comparison, so 16384 entries already spill into leaves and 16383
/// is the biggest flat root there is. libviprs#1021 says 16384 and puts its
/// extrapolated peak there; PR #1022 corrected it.
pub const LARGEST_FLAT_ROOT: u64 = ROOT_ONLY_MAX_ENTRIES - 1;

/// What shape of PMTiles directory a cell is supposed to come out as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regime {
    /// Every entry lives in a flat root and there are no leaf directories.
    Root,
    /// The entries spilled past the cutoff, so the root is a handful of leaf
    /// pointers and every lookup goes through a leaf.
    Leaves,
}

/// How many pixels a source runs for before it repeats on both axes, when it
/// repeats at all.
///
/// It matters because the writer counts **run-length-encoded entries**. A
/// level's tile ids are one contiguous range, so if every tile of a level is
/// byte-identical the writer merges the lot into a single entry and the cell
/// stops being the cell its tile count says it is.
///
/// * [`Source::Gradient`] is `(x % 251, y % 241, (x * 7 + y * 13) % 239)`.
///   Three primes, none of them a factor of any tile size anybody uses, so the
///   smallest repeat is `251 * 241 * 239 = 14_457_349` pixels.
/// * [`Source::PeriodicGradient`] is the same ramp at 256.
/// * [`Source::Flat`] repeats every pixel.
/// * [`Source::Noise`] is a seeded walk over the whole raster, so it never
///   repeats inside a canvas anybody can allocate. `None`.
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
pub fn collapses_at(source: Source, tile_px: u32) -> bool {
    period_px(source).is_some_and(|period| tile_px % period == 0)
}

/// Whether a cell's declared shape survives the source it is filled from.
///
/// With the engine's gradient this never fires on any cell in the family, which
/// is the point: it is a regression test rather than a refusal anybody hits.
pub fn source_suits_the_cell(cell: &Cell) -> Result<(), String> {
    if let Some(period) = period_px(cell.source)
        && collapses_at(cell.source, cell.tile_px)
    {
        return Err(format!(
            "{} fills a {} pixel tile from a source whose period is {period} pixels on both axes, \
             so every tile of a level is the same bytes and the writer's run-length encoding \
             collapses the level to one entry",
            cell.spec(),
            cell.tile_px
        ));
    }
    Ok(())
}

/// The cells a sweep may publish rows for.
pub fn publishable(cells: &[Cell]) -> Vec<Cell> {
    cells
        .iter()
        .copied()
        .filter(|cell| cell.source.publishable())
        .collect()
}

/// The canvas whose root stops just under the writer's cutoff.
///
/// 4096 by 6256 pixels at a 46 pixel tile, a search result rather than a choice:
/// [`brink_search`] re-runs the search that found it. Measured, its archive's
/// root holds 16369 entries and no leaf pointers, fourteen under
/// [`LARGEST_FLAT_ROOT`]. It is pinned by opening the archive and asking
/// `root_entries()`, never by arithmetic over the planner's halving, because
/// arithmetic over a planner stops describing the brink the day the planner
/// moves.
pub const BRINK_CANVAS: (u32, u32, u32) = (4096, 6256, 46);

/// The smoke cell, and the replicate control.
pub const SMOKE_CANVAS: (u32, u32, u32) = (2048, 2048, 256);

/// The mid root-only cell: 95 KB tiles, the photographic end.
pub const MID_CANVAS: (u32, u32, u32) = (8192, 8192, 256);

/// The only leaf-bearing cell: 21851 tiles behind six leaf pointers.
pub const LEAF_CANVAS: (u32, u32, u32) = (8192, 8192, 64);

fn at(canvas: (u32, u32, u32), source: Source) -> Cell {
    let mut cell = Cell::new(canvas.0, canvas.1, canvas.2, source, 0);
    cell.declared_tiles = cell.planned_tiles().unwrap_or(0) as u32;
    cell
}

/// The cell whose root sits at the top of the open-cost ramp.
pub fn brink_cell(source: Source) -> Cell {
    at(BRINK_CANVAS, source)
}

/// The cell on the far side of the cliff, where every lookup goes through a
/// leaf directory.
pub fn leaf_cell(source: Source) -> Cell {
    at(LEAF_CANVAS, source)
}

/// The smoke cell, measured first and last in every sweep as the drift control.
pub fn smoke_cell(source: Source) -> Cell {
    at(SMOKE_CANVAS, source)
}

/// The mid root-only cell.
pub fn mid_cell(source: Source) -> Cell {
    at(MID_CANVAS, source)
}

/// Re-run the search that picked [`BRINK_CANVAS`], and answer with the cell it
/// finds and the tiles it plans.
///
/// Tile sizes from 16 pixels, canvases up to 4096 on the short edge and twice
/// that on the long one, heights by binary search, which is sound because a
/// plan's tile count never falls as the canvas grows. It is arithmetic, and
/// arithmetic is exactly what must not be trusted to pin the cell: this says
/// which cell to build, and the archive says what it came out as.
pub fn brink_search() -> (Cell, u64) {
    let mut best: Option<(Cell, u64)> = None;
    for tile_px in 16u32..=256 {
        for width in [256u32, 512, 1024, 2048, 4096] {
            let fits = |height: u32| {
                Cell::new(width, height, tile_px, Source::Gradient, 0)
                    .planned_tiles()
                    .is_some_and(|tiles| tiles as u64 <= LARGEST_FLAT_ROOT)
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
            let found = at((width, low, tile_px), Source::Gradient);
            let tiles = u64::from(found.declared_tiles);
            if best.is_none_or(|(_, most)| tiles > most) {
                best = Some((found, tiles));
            }
        }
    }
    best.expect("some cell in the search space plans a pyramid")
}

/// What an archive's root really holds: total entries, and how many of them are
/// leaf pointers.
///
/// The one function here that answers the brink question, and it answers it by
/// opening the archive. Every other route to that number is arithmetic over a
/// planner.
pub fn root_shape(archive: &std::path::Path) -> (u64, u64) {
    use libviprs::pyramid_reader::PmTilesPyramidReader;

    let reader = PmTilesPyramidReader::try_open(archive).expect("the archive opens for reading");
    let root = reader.reader().root_entries();
    let leaves = root.iter().filter(|entry| entry.is_leaf()).count();
    (root.len() as u64, leaves as u64)
}

/// Which regime an archive is actually in, from its root shape.
pub fn observed_regime(archive: &std::path::Path) -> Regime {
    let (_, leaves) = root_shape(archive);
    if leaves == 0 {
        Regime::Root
    } else {
        Regime::Leaves
    }
}

/// Every source a sweep may walk, publishable or not.
pub const SOURCES: [Source; 4] = [
    Source::Gradient,
    Source::Noise,
    Source::Flat,
    Source::PeriodicGradient,
];
