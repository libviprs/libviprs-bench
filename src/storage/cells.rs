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
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Gradient => "gradient",
            Source::Noise => "noise",
            Source::Flat => "flat",
        }
    }

    pub fn parse(s: &str) -> Option<Source> {
        match s {
            "gradient" => Some(Source::Gradient),
            "noise" => Some(Source::Noise),
            "flat" => Some(Source::Flat),
            _ => None,
        }
    }

    /// Whether a row measured on this source may be published.
    pub fn publishable(self) -> bool {
        !matches!(self, Source::Flat)
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
