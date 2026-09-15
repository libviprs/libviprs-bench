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
    /// The full profile walks its replicate control before the first measured
    /// cell and after every one of them, so it opens and closes on the control
    /// and holds four more placements in between. It carries the brink cell at
    /// **16369** entries (measured by opening the archive, not derived: an
    /// earlier draft of this comment said 16263, which was arithmetic over a
    /// planner), and walks the `noise` source on the two 64-pixel cells so
    /// compressibility is an axis.
    pub fn cells(self) -> Vec<Cell> {
        match self {
            // The ci profile proves the harness runs. One cell, one source,
            // and no replicate control: it is never published, so it has no
            // noise floor to publish either.
            Profile::Ci => vec![Cell::new(2048, 2048, 256, Source::Gradient, 93)],
            Profile::Full => {
                // The control goes before the first cell and after every other
                // one, so the floor rests on six placements rather than on the
                // gap between two. Never two in a row: a pair taken back to
                // back sees none of the thermal, neighbour and page-cache drift
                // the control is there to catch, so it would raise the count
                // without widening the window (#84).
                crate::storage::scenarios::replicate::schedule(
                    Cell::new(2048, 2048, 256, Source::Gradient, 93),
                    &Profile::Full.measured_cells(),
                )
            }
            Profile::Xl => crate::storage::scenarios::replicate::schedule(
                Cell::new(2048, 2048, 256, Source::Gradient, 93),
                &Profile::Xl.measured_cells(),
            ),
        }
    }

    /// The cells a profile walks that are NOT the control, in facet order.
    ///
    /// Split out because the control's placements are derived from this list
    /// rather than typed alongside it: the schedule puts one before the first
    /// of these and one after each of them, so the placement count is
    /// `measured_cells().len() + 1` and cannot drift out of step with the cells
    /// by an edit to one and not the other.
    pub fn measured_cells(self) -> Vec<Cell> {
        match self {
            Profile::Ci => Vec::new(),
            Profile::Full => vec![
                Cell::new(8192, 8192, 256, Source::Gradient, 1373),
                // The brink cell: the peak of the open-cost ramp, four pixels
                // of tile under the writer's own cutoff. Without it the sweep
                // brackets the worst case instead of measuring it, which is the
                // whole of libviprs#1021.
                brink_cell(Source::Gradient),
                Cell::new(8192, 8192, 64, Source::Gradient, 21851),
                // Compressibility, the axis the old sweep never had. Both
                // 64-pixel cells, because that is where the tile payload is
                // small enough for the codec to be most of the difference.
                Cell::new(8192, 8192, 64, Source::Noise, 21851),
                brink_cell(Source::Noise),
            ],
            Profile::Xl => {
                let mut cells = Profile::Full.measured_cells();
                cells.push(Cell::new(16384, 16384, 256, Source::Gradient, 5469));
                cells
            }
        }
    }

    /// The scenarios this profile walks, by name.
    pub fn scenario_names(self) -> Vec<String> {
        scenario_names_for(self)
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
    period_px(source).is_some_and(|period| tile_px.is_multiple_of(period))
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

/// The smoke cell, measured throughout every sweep as the drift control.
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
    // Not an I/O condition: the search walks 241 tile sizes over five widths
    // and every one of them plans, so an empty result would mean the loop above
    // was edited into doing nothing.
    best.expect("some cell in the search space plans a pyramid")
}

/// What an archive's root really holds: total entries, and how many of them are
/// leaf pointers.
///
/// The one function here that answers the brink question, and it answers it by
/// opening the archive. Every other route to that number is arithmetic over a
/// planner.
pub fn root_shape(archive: &std::path::Path) -> Result<(u64, u64), String> {
    use libviprs::pyramid_reader::PmTilesPyramidReader;

    let reader = PmTilesPyramidReader::try_open(archive)
        .map_err(|e| format!("the archive does not open for reading: {e}"))?;
    let root = reader.reader().root_entries();
    let leaves = root.iter().filter(|entry| entry.is_leaf()).count();
    Ok((root.len() as u64, leaves as u64))
}

/// Which regime an archive is actually in, from its root shape.
pub fn observed_regime(archive: &std::path::Path) -> Result<Regime, String> {
    let (_, leaves) = root_shape(archive)?;
    Ok(if leaves == 0 {
        Regime::Root
    } else {
        Regime::Leaves
    })
}

/// Every source a sweep may walk, publishable or not.
pub const SOURCES: [Source; 4] = [
    Source::Gradient,
    Source::Noise,
    Source::Flat,
    Source::PeriodicGradient,
];

/// The cell a sweep measures through its whole length, as its own noise floor.
///
/// `None` on `ci`, which proves the harness runs and is never published, so it
/// has no noise floor to publish. The dispersion across its placements is the
/// only in-run figure a host with no calibrated baseline has, and it is a floor
/// rather than a calibration.
pub fn replicate_cell(profile: Profile) -> Option<Cell> {
    match profile {
        Profile::Ci => None,
        Profile::Full | Profile::Xl => profile.cells().first().copied(),
    }
}

/// The scenarios a profile walks, by name, in registry order.
///
/// `ci` walks the cheap end and says so here rather than skipping quietly at
/// run time. What it leaves out is the thread ladder, which is four scenarios
/// over two backends and measures contention that one cell on a shared runner
/// cannot see anyway, and `replicate`, which needs a schedule `ci` does not
/// have. Everything else runs, because the point of `ci` is that the harness
/// produces every row shape the full profile does.
pub fn scenario_names_for(profile: Profile) -> Vec<String> {
    let all = [
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
    all.iter()
        .filter(|name| match profile {
            Profile::Ci => !name.starts_with("read_concurrent@"),
            Profile::Full | Profile::Xl => true,
        })
        .map(|name| name.to_string())
        .collect()
}
