//! The PMTiles writer sweep (libviprs-bench #132).
//!
//! The epic that made the PMTiles writer bounded (libviprs#1135) named two
//! benchmarks as a deliverable and closed without either: the regression against
//! the previous revision, and the comparison against go-pmtiles. This module is
//! both, and it is built to be read the way the rest of the storage family is,
//! which is as a document the aggregator archives, not as a table in a PR.
//!
//! # What is measured
//!
//! A *variant* is a point in the writer's configuration space: a layout
//! (`tile-id` or `arrival`), whether the engine is asked for ordered emission,
//! and a dedupe budget. The matrix is two layouts, with and without ordered
//! emission, at three budgets, which is the issue's proposal exactly. Every
//! variant is measured two ways, because the two answer different questions:
//!
//! * **Engine-driven** (`write_ingest`, `write_finalize`, `write_output`): the
//!   real engine builds the real pyramid into a [`PmTilesSink`] configured as
//!   the variant says, with the finish held back so ingest and finalize time
//!   separately, exactly as [`super::scenarios::write_split`] splits them. Peak
//!   live heap is published for both halves. This is the number a caller
//!   running the CLI gets, and it is dominated on a real pyramid by the source
//!   raster and the engine's own buffers, which is the first reason there is a
//!   second measurement.
//! * **Writer-driven** (`build_from_tiles`): the same encoded tiles, read from a
//!   tile set on disk and handed to [`Writer`] directly. No engine, no encode,
//!   no resampling, so the heap peak is the writer's and the wall time is the
//!   writer's, and it is the only form go-pmtiles can be put beside, because
//!   go-pmtiles does not encode a pyramid either: it converts a tile set that
//!   already exists. Ordered variants feed the tiles in tile id order and the
//!   others in a seeded shuffle, which is what an engine that delivers tiles as
//!   its workers finish looks like to a writer.
//!
//! The cells are the storage family's own three corpus cells, so the writer is
//! compared on bytes the storage sweep already publishes, plus two synthetic
//! cells of one distinct payload per tile. A real pyramid has a few thousand
//! payloads at most, which is far too few for a table that grows with them to
//! show; the synthetic cells are where the bounded-memory claim is actually
//! tested, and they are labelled synthetic in the cell name and the source so
//! nobody reads them as imagery.
//!
//! # Why the baseline is a revision and not a release
//!
//! The epic's checklist says "the previous libviprs release". No release carries
//! the writer as it was before the epic: v0.4.0 has no PMTiles at all and
//! v0.5.0 (2026-09-24) already has every Phase 1 and Phase 2 change. The honest
//! baseline is the last revision before the bounded writer landed, which is a
//! pinned commit, and a document measured there carries that commit in its
//! provenance like any other. This module compiles against it: the three knobs
//! the epic added are behind `cfg(core_writer_knobs)`, which `build.rs` sets
//! only when the measured core has them, and a revision without them has one
//! variant, `tile-id@unbounded`, because its payload table is.
//!
//! # go-pmtiles
//!
//! `pmtiles convert` reads an MBTiles file, so the tile set is written to one
//! (`tools/mbtiles-from-tiles.py`, untimed) and go-pmtiles is timed converting
//! it. Both contenders are timed inside the process that does the work (the
//! writer from opening to publishing, go-pmtiles from spawn to reap), so neither
//! carries its own start-up, and both report a peak resident set that has no
//! floor from whoever started them. Ours is `VmHWM` read by the child. go-pmtiles
//! is not ours to instrument, so its figure is the kernel's `ru_maxrss` from the
//! reaping `wait4`, taken by a small fresh helper process rather than by the
//! sweep: that figure includes the parent's resident set at the fork, and the
//! sweep, which has just built tile sets, is not a small parent. What is and is
//! not the same work is spelled out in `archive/storage/README.md`; the short
//! version is that go-pmtiles also reads SQLite and the writer here reads loose
//! files.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use libviprs::pmtiles::writer::content_hash;
use libviprs::pmtiles::{Reader, TileType, Writer, WriterOptions, tileid_to_zxy, zxy_to_tileid};
use libviprs::sink::TileFormat;
use libviprs::sink_pmtiles::PmTilesSink;
use serde::{Deserialize, Serialize};

use super::cells::{self, Backend, Cell, Profile, SEED, Source};
use super::document::{
    CellLabels, CellReport, Document, DocumentCell, FAMILY, InvariantBlock, MachineLoad,
    Measurement, MeasurementSpec, RUNNER, Reps,
};
use super::heap;
use super::scenarios::{Direction, Isolation, MetricSpec, Outcome, Unit};
use super::scenarios::write_split;
use super::stats;
use super::{Scratch, artefact_digest, now_iso, occupancy, raster, write_pyramid};

/// The file a writer sweep writes, inside the storage family's report directory.
pub const DOCUMENT_NAME: &str = "writer-results.json";

/// The backend label go-pmtiles rows carry.
pub const GO_PMTILES: &str = "go-pmtiles";

/// The hidden subcommand a parent uses to re-invoke itself as one repetition.
pub const SINGLE_FLAG: &str = "--writer-single";

/// The scenarios a writer sweep publishes, in sweep order.
pub const SCENARIOS: &[&str] = &[
    "write_ingest",
    "write_finalize",
    "write_output",
    "build_from_tiles",
];

/// The dedupe budgets the matrix walks, in bytes.
///
/// 256 KiB and 1 MiB are under what the 21851-tile cells need to remember every
/// payload (about 1.4 MiB at the writer's 65 bytes a payload), so they evict
/// there and the window is doing something. 8 MiB is the core's default and
/// holds all of them, which makes it the control: a window that is never full
/// should cost what a table does.
pub const BUDGETS_BYTES: [usize; 3] = [256 * 1024, 1024 * 1024, 8 * 1024 * 1024];

/// Where the mbtiles builder lives, relative to the crate root.
pub const MBTILES_SCRIPT: &str = "tools/mbtiles-from-tiles.py";

/// The environment variable that names the go-pmtiles binary.
pub const GO_ENV: &str = "BENCH_GO_PMTILES";

/// The contention (one-minute load per core) a repetition waits to fall under
/// before it starts. Half a core's worth of load per core by default.
pub const QUIET_BELOW_ENV: &str = "WRITER_QUIET_BELOW";

/// The most a repetition waits for the machine to go quiet, in seconds.
pub const QUIET_WAIT_ENV: &str = "WRITER_QUIET_WAIT_S";

/// The share of the last ten seconds some task spent stalled on I/O, above which
/// a repetition waits (`/proc/pressure/io`, `some avg10`). CPU load does not see
/// a neighbour hammering the disk, and the archive's finalize is an `fsync`.
pub const QUIET_IO_BELOW_ENV: &str = "WRITER_QUIET_IO_BELOW";

const DEFAULT_QUIET_IO_BELOW: f64 = 20.0;
const DEFAULT_QUIET_BELOW: f64 = 0.5;
const DEFAULT_QUIET_WAIT_S: u64 = 300;

// ---------------------------------------------------------------------------
// Variants
// ---------------------------------------------------------------------------

/// How the writer lays the data region out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    /// Tile id order, reordered at finalize. The core's default.
    TileId,
    /// Appended in arrival order, with no reordering pass.
    Arrival,
}

/// How much memory the writer may spend remembering which payloads it has seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Budget {
    /// The revision has no budget: its payload table grows with the number of
    /// distinct payloads. Only a core without the writer knobs has this.
    Unbounded,
    /// A fixed window of this many bytes.
    Bytes(usize),
}

/// One point in the writer's configuration space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Variant {
    pub layout: LayoutKind,
    /// Whether the engine is asked for its tiles in tile id order (and, for the
    /// writer-driven scenario, whether they are fed in that order).
    pub ordered: bool,
    pub budget: Budget,
}

impl Variant {
    /// `<layout>[+ordered]@<budget>`, which is the backend label in the document.
    pub fn label(&self) -> String {
        let layout = match self.layout {
            LayoutKind::TileId => "tile-id",
            LayoutKind::Arrival => "arrival",
        };
        let ordered = if self.ordered { "+ordered" } else { "" };
        let budget = match self.budget {
            Budget::Unbounded => "unbounded".to_string(),
            Budget::Bytes(n) if n % (1024 * 1024) == 0 => format!("{}MiB", n / (1024 * 1024)),
            Budget::Bytes(n) if n % 1024 == 0 => format!("{}KiB", n / 1024),
            Budget::Bytes(n) => format!("{n}B"),
        };
        format!("{layout}{ordered}@{budget}")
    }

    /// The variant a label names.
    pub fn parse(label: &str) -> Option<Variant> {
        let (mode, budget) = label.split_once('@')?;
        let (layout, ordered) = match mode {
            "tile-id" => (LayoutKind::TileId, false),
            "tile-id+ordered" => (LayoutKind::TileId, true),
            "arrival" => (LayoutKind::Arrival, false),
            "arrival+ordered" => (LayoutKind::Arrival, true),
            _ => return None,
        };
        let budget = if budget == "unbounded" {
            Budget::Unbounded
        } else if let Some(n) = budget.strip_suffix("MiB") {
            Budget::Bytes(n.parse::<usize>().ok()?.checked_mul(1024 * 1024)?)
        } else if let Some(n) = budget.strip_suffix("KiB") {
            Budget::Bytes(n.parse::<usize>().ok()?.checked_mul(1024)?)
        } else {
            Budget::Bytes(budget.strip_suffix('B')?.parse().ok()?)
        };
        Some(Variant {
            layout,
            ordered,
            budget,
        })
    }

    /// The writer options this variant stands for.
    ///
    /// The tile type is PNG, which is what every real cell's tiles are. The
    /// layout and the budget are only set on a core that has them, and a
    /// variant that asks for a budget the core cannot express is the core's
    /// default, which is why [`matrix`] never builds one.
    ///
    /// One consequence worth knowing when reading the document: the
    /// engine-driven arrival run without ordered emission lays tiles down in
    /// whatever order the worker threads finished, so its bytes are not
    /// reproducible between repetitions and the row's `artefactDigest` is
    /// `null` there, because [`agreed`] only publishes what every repetition
    /// agreed on.
    pub fn options(&self) -> WriterOptions {
        let options = WriterOptions::default().with_tile_type(TileType::Png);
        #[cfg(core_writer_knobs)]
        let options = {
            use libviprs::pmtiles::Layout;
            let options = match self.budget {
                Budget::Bytes(n) => options.with_dedupe_memory_bytes(n),
                Budget::Unbounded => options,
            };
            options.with_layout(match self.layout {
                LayoutKind::TileId => Layout::TileId,
                LayoutKind::Arrival => Layout::Arrival,
            })
        };
        options
    }
}

/// Every variant the measured core can express.
///
/// Twelve on a core with the bounded writer, one on a core without it.
pub fn matrix() -> Vec<Variant> {
    #[cfg(core_writer_knobs)]
    {
        let mut out = Vec::new();
        for (layout, ordered) in [
            (LayoutKind::TileId, false),
            (LayoutKind::Arrival, false),
            (LayoutKind::TileId, true),
            (LayoutKind::Arrival, true),
        ] {
            for bytes in BUDGETS_BYTES {
                out.push(Variant {
                    layout,
                    ordered,
                    budget: Budget::Bytes(bytes),
                });
            }
        }
        out
    }
    #[cfg(not(core_writer_knobs))]
    {
        vec![Variant {
            layout: LayoutKind::TileId,
            ordered: false,
            budget: Budget::Unbounded,
        }]
    }
}

// ---------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------

/// One cell of the writer sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepCell {
    /// A pyramid the storage family already publishes.
    Real(Cell),
    /// One distinct payload at every address of one zoom level: `4^z` tiles.
    Synthetic(u8),
}

/// How long a synthetic payload is. Padded, so every one is the same length and
/// no two are the same bytes.
pub const SYNTHETIC_LEN: usize = 48;

/// The payload a synthetic tile carries.
///
/// A pure function of the address, and the same function in
/// `tools/mbtiles-from-tiles.py`, so go-pmtiles is handed the same bytes. It is
/// not a PNG and does not pretend to be: neither converter decodes a tile.
pub fn synthetic_payload(z: u8, x: u32, y: u32, into: &mut Vec<u8>) {
    into.clear();
    into.extend_from_slice(format!("viprs-synth z={z} x={x} y={y}").as_bytes());
    into.resize(SYNTHETIC_LEN, b'.');
}

impl SweepCell {
    /// The name the document knows the cell by.
    pub fn spec(&self) -> String {
        match self {
            SweepCell::Real(cell) => cell.spec(),
            SweepCell::Synthetic(z) => format!("synthetic@z{z}"),
        }
    }

    pub fn parse(spec: &str) -> Option<SweepCell> {
        if let Some(z) = spec.strip_prefix("synthetic@z") {
            return Some(SweepCell::Synthetic(z.parse().ok().filter(|z| *z <= 12)?));
        }
        Cell::parse(spec).map(SweepCell::Real)
    }

    /// The number of tiles, which is the cell's `scale` in the document.
    pub fn tiles(&self) -> u64 {
        match self {
            SweepCell::Real(cell) => cell.planned_tiles().unwrap_or(0) as u64,
            SweepCell::Synthetic(z) => 1u64 << (2 * u32::from(*z)),
        }
    }

    fn source_label(&self) -> String {
        match self {
            SweepCell::Real(cell) => cell.source.as_str().to_string(),
            SweepCell::Synthetic(_) => "synthetic-distinct".to_string(),
        }
    }

    fn is_real(&self) -> bool {
        matches!(self, SweepCell::Real(_))
    }
}

/// The cells a profile walks.
pub fn cells_for(profile: Profile) -> Vec<SweepCell> {
    match profile {
        Profile::Ci => vec![
            SweepCell::Real(cells::smoke_cell(Source::Gradient)),
            SweepCell::Synthetic(8),
        ],
        Profile::Full | Profile::Xl => vec![
            SweepCell::Real(cells::mid_cell(Source::Gradient)),
            SweepCell::Real(cells::leaf_cell(Source::Gradient)),
            SweepCell::Real(cells::leaf_cell(Source::Noise)),
            SweepCell::Synthetic(9),
            SweepCell::Synthetic(10),
        ],
    }
}

/// The cells a profile walks, as the specs the document names them by.
pub fn sweep_cells(profile: Profile) -> Vec<String> {
    cells_for(profile).iter().map(SweepCell::spec).collect()
}

/// Repetitions per `(contender, cell)`. The storage family's own count, so a
/// writer row has the same footing as a `generate` row.
pub fn write_reps(profile: Profile) -> u32 {
    super::document::generate_reps(profile)
}

// ---------------------------------------------------------------------------
// One repetition, in a child
// ---------------------------------------------------------------------------

/// What one repetition left behind, as the child reports it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepOut {
    /// Engine-driven: milliseconds to the last `add_tile`.
    pub ingest_ms: Option<f64>,
    /// Engine-driven: milliseconds for `finish`.
    pub finalize_ms: Option<f64>,
    /// Engine-driven: peak live heap to the end of ingestion.
    pub ingest_heap: Option<u64>,
    /// Engine-driven: peak live heap over the whole pass.
    pub finalize_heap: Option<u64>,
    /// Writer-driven (and go-pmtiles): milliseconds from the writer opening to
    /// the archive being published. Measured inside the process that did the
    /// work, so it excludes starting that process, for ours and for go-pmtiles
    /// alike.
    pub build_ms: Option<f64>,
    /// Writer-driven: peak live heap over the build.
    pub heap_peak: Option<u64>,
    /// The process's own peak resident set, in bytes.
    ///
    /// Read by the child from `VmHWM` in `/proc/self/status`, never from the
    /// parent's `wait4`. On Linux a forked child that execs reports, through
    /// `wait4`, the larger of its own peak and the parent's resident set at the
    /// moment of the fork, because the old address space's high-water mark is
    /// folded in at exec. A parent that had just built a 200 MB raster would put
    /// a 200 MB floor under every child, and the numbers would be about the
    /// parent. `VmHWM` belongs to the new address space and has no floor.
    pub peak_rss_bytes: Option<u64>,
    pub output_bytes: Option<u64>,
    pub tiles: Option<u64>,
    pub digest: Option<String>,
    pub root_entries: Option<u64>,
    pub leaves: Option<u64>,
}

/// What a child prints.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChildReply {
    Ok(RepOut),
    Err(String),
}

/// How a child is asked to measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The engine builds a pyramid into a sink configured as the variant says.
    Engine,
    /// The writer is handed an existing tile set.
    Build,
    /// Write the tile set the other modes are built from. A child, so the sweep
    /// process never holds a raster and never has a large resident set to hand
    /// down.
    Tileset,
    /// Run go-pmtiles and report what it cost. A child for the same reason:
    /// `wait4` on a process spawned from here reports a floor of this process's
    /// own resident set, so it is spawned from the smallest process available.
    Go,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Engine => "engine",
            Mode::Build => "build",
            Mode::Tileset => "tileset",
            Mode::Go => "go",
        }
    }

    fn parse(s: &str) -> Option<Mode> {
        match s {
            "engine" => Some(Mode::Engine),
            "build" => Some(Mode::Build),
            "tileset" => Some(Mode::Tileset),
            "go" => Some(Mode::Go),
            _ => None,
        }
    }
}

/// The child body: `<variant|-> <cell> <mode> <out-dir> [extra...]`.
///
/// `engine` takes nothing more, `build` takes the tile set's root, `tileset`
/// takes nothing more, and `go` takes the go-pmtiles binary and its MBTiles
/// input. The variant is `-` for the two modes that have none.
pub fn run_child(args: &[String]) -> i32 {
    let [variant, cell, mode, out, rest @ ..] = args else {
        eprintln!("{SINGLE_FLAG} <variant|-> <cell> <engine|build|tileset|go> <out-dir> [extra...]");
        return 2;
    };
    let (Some(cell), Some(mode)) = (SweepCell::parse(cell), Mode::parse(mode)) else {
        eprintln!("unparseable arguments: {args:?}");
        return 2;
    };
    let variant = Variant::parse(variant);
    if variant.is_none() && matches!(mode, Mode::Engine | Mode::Build) {
        eprintln!("{mode:?} needs a variant: {args:?}");
        return 2;
    }
    let outcome = one_rep(variant, cell, mode, Path::new(out), rest);
    let reply = match outcome {
        Ok(mut out) => {
            // Last thing, so it covers the whole repetition and nothing after.
            if out.peak_rss_bytes.is_none() {
                out.peak_rss_bytes = vm_hwm_bytes();
            }
            ChildReply::Ok(out)
        }
        Err(e) => ChildReply::Err(e),
    };
    println!(
        "{}",
        serde_json::to_string(&reply).expect("a reply serialises")
    );
    0
}

/// Dispatch [`SINGLE_FLAG`] out of `argv`.
pub fn maybe_run_single_subcommand() -> Option<i32> {
    let argv: Vec<String> = std::env::args().collect();
    let at = argv.iter().position(|a| a == SINGLE_FLAG)?;
    Some(run_child(&argv[at + 1..]))
}

/// The peak resident set of this process, from `VmHWM`. `None` off Linux, where
/// there is no figure with the same meaning and a number without it would be a
/// different measurement under the same name.
pub fn vm_hwm_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find_map(|l| l.strip_prefix("VmHWM:"))?;
    let kib: u64 = line.trim().trim_end_matches("kB").trim().parse().ok()?;
    Some(kib * 1024)
}

/// One repetition, in this process.
pub fn one_rep(
    variant: Option<Variant>,
    cell: SweepCell,
    mode: Mode,
    out_dir: &Path,
    extra: &[String],
) -> Result<RepOut, String> {
    std::fs::create_dir_all(out_dir).map_err(|e| format!("no output directory: {e}"))?;
    match (mode, cell, variant) {
        (Mode::Engine, SweepCell::Real(cell), Some(variant)) => engine_rep(variant, cell, out_dir),
        (Mode::Engine, SweepCell::Synthetic(_), _) => {
            Err("a synthetic cell has no engine to drive".to_string())
        }
        (Mode::Build, cell, Some(variant)) => {
            build_rep(variant, cell, out_dir, extra.first().map(Path::new))
        }
        (Mode::Tileset, SweepCell::Real(cell), _) => tileset_rep(cell, out_dir),
        (Mode::Tileset, SweepCell::Synthetic(_), _) => {
            Err("a synthetic cell has no tile set to write".to_string())
        }
        (Mode::Go, _, _) => match extra {
            [binary, input] => go_child(Path::new(binary), Path::new(input), out_dir),
            _ => Err("go wants the binary and the mbtiles input".to_string()),
        },
        _ => Err("a mode was given no variant".to_string()),
    }
}

/// Write the tile set the writer-driven scenario and go-pmtiles are built from,
/// untimed, through the directory backend. Its XYZ layout, `<z>/<x>/<y>.png`, is
/// the neutral one both readers want.
fn tileset_rep(cell: Cell, out_dir: &Path) -> Result<RepOut, String> {
    let plan = cell.plan().ok_or("the cell does not plan")?;
    let written = write_pyramid(Backend::Directory, cell, &plan, out_dir)?;
    Ok(RepOut {
        tiles: Some(written.tiles_produced),
        ..RepOut::default()
    })
}

/// Run go-pmtiles once, from this (small, fresh) process, and report it.
///
/// The `wait4` that matters here is the one on go-pmtiles itself, and its
/// `ru_maxrss` floor is this process's resident set at the fork. This process
/// has just started and holds nothing, so the floor is a few megabytes under a
/// Go runtime that is itself tens, rather than the sweep's own.
fn go_child(binary: &Path, input: &Path, out_dir: &Path) -> Result<RepOut, String> {
    let output = out_dir.join("pyramid.pmtiles");
    let mut command = Command::new(binary);
    command
        .arg("convert")
        .arg("--quiet")
        .arg(input)
        .arg(&output)
        .arg("--tmpdir")
        .arg(out_dir);
    let spawned = spawn_measured(&mut command, &out_dir.join("go-stderr.txt"))?;
    if !spawned.success {
        return Err(format!(
            "go-pmtiles convert failed: {}",
            tail(&out_dir.join("go-stderr.txt"))
        ));
    }
    let shape = archive_shape_of(&output);
    Ok(RepOut {
        build_ms: Some(spawned.wall_ms),
        peak_rss_bytes: spawned.peak_rss_bytes,
        output_bytes: occupancy(&output).map(|o| o.0),
        tiles: Reader::try_open(&output)
            .ok()
            .map(|r| r.header().addressed_tiles_count),
        digest: artefact_digest(&output),
        root_entries: shape.map(|s| s.0),
        leaves: shape.map(|s| s.1),
        ..RepOut::default()
    })
}

/// The engine builds the pyramid; ingest and finalize are timed apart.
fn engine_rep(variant: Variant, cell: Cell, out_dir: &Path) -> Result<RepOut, String> {
    let plan = cell.plan().ok_or("the cell does not plan")?;
    // Before the clock and before the counting window, exactly as the storage
    // family's own write split does it, so the two measure the same work.
    let source = raster(cell.source, cell.width, cell.height);
    let archive = out_dir.join("pyramid.pmtiles");
    let armed = heap::arm();
    let started = Instant::now();
    #[allow(unused_mut)]
    let mut builder = PmTilesSink::builder(&archive)
        .plan(plan.clone())
        .tile_format(TileFormat::Png)
        .writer_options(variant.options());
    #[cfg(core_writer_knobs)]
    {
        builder = builder.ordered_emission(variant.ordered);
    }
    let sink = builder
        .build()
        .map_err(|e| format!("the archive sink does not build: {e}"))?;
    let walked = write_split::walk(&sink, &source, &plan, &armed, started, archive.clone())?;
    let shape = archive_shape_of(&archive);
    Ok(RepOut {
        ingest_ms: Some(walked.ingest.as_secs_f64() * 1000.0),
        finalize_ms: Some(walked.finalize.as_secs_f64() * 1000.0),
        ingest_heap: walked.ingest_peak_bytes,
        finalize_heap: walked.finalize_peak_bytes,
        output_bytes: occupancy(&archive).map(|o| o.0),
        tiles: Some(walked.tiles_produced),
        digest: artefact_digest(&archive),
        root_entries: shape.map(|s| s.0),
        leaves: shape.map(|s| s.1),
        ..RepOut::default()
    })
}

/// The writer is handed an existing tile set, in the order the variant says.
fn build_rep(
    variant: Variant,
    cell: SweepCell,
    out_dir: &Path,
    tiles_root: Option<&Path>,
) -> Result<RepOut, String> {
    let archive = out_dir.join("pyramid.pmtiles");
    let armed = heap::arm();
    let started = Instant::now();
    let mut writer = Writer::create(&archive, variant.options())
        .map_err(|e| format!("the writer does not open: {e}"))?;
    match cell {
        SweepCell::Real(real) => {
            let root = tiles_root.ok_or("a real cell needs the tile set it is built from")?;
            let plan = real.plan().ok_or("the cell does not plan")?;
            let mut ids: Vec<u64> = cells::coordinates(&plan)
                .iter()
                .map(|c| {
                    zxy_to_tileid(c.level as u8, c.col, c.row)
                        .map_err(|e| format!("{c:?} is not addressable: {e}"))
                })
                .collect::<Result<_, _>>()?;
            if variant.ordered {
                ids.sort_unstable();
            } else {
                shuffle(&mut ids);
            }
            for id in ids {
                let (z, x, y) = tileid_to_zxy(id).map_err(|e| format!("{e}"))?;
                let path = root.join(format!("{z}/{x}/{y}.png"));
                let bytes = std::fs::read(&path)
                    .map_err(|e| format!("tile {} does not read: {e}", path.display()))?;
                writer
                    .add_tile(z, x, y, &bytes, content_hash(&bytes))
                    .map_err(|e| format!("add_tile {z}/{x}/{y}: {e}"))?;
            }
        }
        SweepCell::Synthetic(z) => {
            let side = 1u64 << z;
            let n = side * side;
            let base = (n - 1) / 3;
            let mut payload = Vec::with_capacity(SYNTHETIC_LEN);
            // A bijection on 0..n, because n is a power of two and the
            // multiplier is odd, so the unordered feed visits every address
            // once without holding an address list in the heap that is being
            // measured.
            let (a, b) = (0x9E37_79B1_u64 | 1, SEED & (n - 1));
            for i in 0..n {
                let (z, x, y) = if variant.ordered {
                    tileid_to_zxy(base + i).map_err(|e| format!("{e}"))?
                } else {
                    let j = i.wrapping_mul(a).wrapping_add(b) & (n - 1);
                    (z, (j & (side - 1)) as u32, (j >> z) as u32)
                };
                synthetic_payload(z, x, y, &mut payload);
                writer
                    .add_tile(z, x, y, &payload, content_hash(&payload))
                    .map_err(|e| format!("add_tile {z}/{x}/{y}: {e}"))?;
            }
        }
    }
    let finish = writer
        .finish()
        .map_err(|e| format!("the writer does not finish: {e}"))?;
    let build_ms = started.elapsed().as_secs_f64() * 1000.0;
    let heap_peak = armed.peak_bytes();
    let shape = archive_shape_of(&archive);
    Ok(RepOut {
        build_ms: Some(build_ms),
        heap_peak,
        output_bytes: occupancy(&archive).map(|o| o.0),
        tiles: Some(finish.header.addressed_tiles_count),
        digest: artefact_digest(&archive),
        root_entries: shape.map(|s| s.0),
        leaves: shape.map(|s| s.1),
        ..RepOut::default()
    })
}

/// A seeded Fisher-Yates, the same generator the storage family shuffles with.
fn shuffle(ids: &mut [u64]) {
    let mut rng = stats::Splitmix::new(SEED);
    for i in (1..ids.len()).rev() {
        let j = rng.below(i + 1);
        ids.swap(i, j);
    }
}

fn archive_shape_of(archive: &Path) -> Option<(u64, u64)> {
    let reader = Reader::try_open(archive).ok()?;
    let root = reader.root_entries();
    let leaves = root.iter().filter(|e| e.is_leaf()).count();
    Some((root.len() as u64, leaves as u64))
}

// ---------------------------------------------------------------------------
// Attestation
// ---------------------------------------------------------------------------

/// How many tiles an archive is read back at to be believed.
pub const ATTEST_SAMPLE: usize = 64;

/// Read an archive back and say whether it holds the tile set it was built from.
///
/// Nothing here reads a label. The tile count the header reports is compared
/// with the cell's, and [`ATTEST_SAMPLE`] seeded coordinates are read out of the
/// archive and compared byte for byte with the source (a file in the tile set,
/// or the synthetic function). A foreign archive is read the same way, which is
/// what makes go-pmtiles' output attestable by the same rule as ours.
pub fn attest_archive(
    archive: &Path,
    cell: SweepCell,
    tiles_root: Option<&Path>,
) -> Result<(), String> {
    let reader = Reader::try_open(archive).map_err(|e| format!("the archive does not open: {e}"))?;
    let expected = cell.tiles();
    let addressed = reader.header().addressed_tiles_count;
    if addressed != expected {
        return Err(format!(
            "the archive addresses {addressed} tiles and the cell has {expected}"
        ));
    }
    let mut coords: Vec<(u8, u32, u32)> = match cell {
        SweepCell::Real(real) => {
            let plan = real.plan().ok_or("the cell does not plan")?;
            cells::coordinates(&plan)
                .iter()
                .map(|c| (c.level as u8, c.col, c.row))
                .collect()
        }
        SweepCell::Synthetic(z) => {
            let side = 1u32 << z;
            let mut rng = stats::Splitmix::new(SEED);
            (0..ATTEST_SAMPLE * 4)
                .map(|_| (z, rng.below(side as usize) as u32, rng.below(side as usize) as u32))
                .collect()
        }
    };
    let mut rng = stats::Splitmix::new(SEED);
    for i in (1..coords.len()).rev() {
        let j = rng.below(i + 1);
        coords.swap(i, j);
    }
    let mut scratch = Vec::new();
    for (z, x, y) in coords.into_iter().take(ATTEST_SAMPLE) {
        let want: Vec<u8> = match cell {
            SweepCell::Real(_) => {
                let root = tiles_root.ok_or("a real cell is attested against its tile set")?;
                std::fs::read(root.join(format!("{z}/{x}/{y}.png")))
                    .map_err(|e| format!("source tile {z}/{x}/{y}: {e}"))?
            }
            SweepCell::Synthetic(_) => {
                synthetic_payload(z, x, y, &mut scratch);
                scratch.clone()
            }
        };
        let got = reader
            .get_tile(z, x, y)
            .map_err(|e| format!("tile {z}/{x}/{y} does not read back: {e}"))?;
        if got.as_deref() != Some(want.as_slice()) {
            return Err(format!("tile {z}/{x}/{y} reads back different bytes"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The parent: measuring children
// ---------------------------------------------------------------------------

/// What a spawned process cost, as the kernel saw it.
#[derive(Debug, Clone)]
pub struct Spawned {
    pub stdout: String,
    pub wall_ms: f64,
    pub peak_rss_bytes: Option<u64>,
    pub success: bool,
}

/// Run a command to completion and report its wall time and its own peak RSS.
///
/// `wait4` rather than a self-report, for the reason the storage family uses it:
/// `ru_maxrss` from the reaping call is the kernel's figure for that process, the
/// same way for a Rust child and for go-pmtiles. stdout is drained before the
/// reap so a chatty child cannot block on a full pipe, and stderr goes to a file
/// so a failure can be quoted without a second pipe to drain.
pub fn spawn_measured(command: &mut Command, stderr_to: &Path) -> Result<Spawned, String> {
    let stderr = std::fs::File::create(stderr_to).map_err(|e| format!("no stderr file: {e}"))?;
    let started = Instant::now();
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(stderr)
        .spawn()
        .map_err(|e| format!("does not spawn: {e}"))?;
    let pid = child.id() as i32;
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .ok_or("no stdout")?
        .read_to_string(&mut stdout)
        .map_err(|e| format!("stdout does not read: {e}"))?;
    let (status, rss) = reap(pid);
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(Spawned {
        stdout,
        wall_ms,
        peak_rss_bytes: rss,
        success: status == Some(0),
    })
}

fn reap(pid: i32) -> (Option<i32>, Option<u64>) {
    use std::mem::MaybeUninit;
    let mut status: libc::c_int = 0;
    let mut usage = MaybeUninit::<libc::rusage>::uninit();
    let ret = unsafe { libc::wait4(pid, &mut status, 0, usage.as_mut_ptr()) };
    if ret <= 0 {
        return (None, None);
    }
    let usage = unsafe { usage.assume_init() };
    let maxrss = usage.ru_maxrss as u64;
    let bytes = if cfg!(target_os = "macos") {
        maxrss
    } else {
        maxrss * 1024
    };
    let code = if libc::WIFEXITED(status) {
        Some(libc::WEXITSTATUS(status))
    } else {
        None
    };
    (code, (bytes > 0).then_some(bytes))
}

/// The go-pmtiles binary, if there is one to run.
#[derive(Debug, Clone)]
pub struct GoPmtiles {
    pub binary: PathBuf,
    pub version: String,
    pub sha256: Option<String>,
}

impl GoPmtiles {
    /// Find go-pmtiles: `$BENCH_GO_PMTILES`, else `pmtiles` on `PATH`. `Err`
    /// carries the reason a row will give for being skipped.
    pub fn locate() -> Result<GoPmtiles, String> {
        let binary = std::env::var(GO_ENV)
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("pmtiles"));
        let probe = Command::new(&binary).arg("version").output().map_err(|e| {
            format!(
                "go-pmtiles is not available ({}: {e}); set {GO_ENV} or put `pmtiles` on PATH",
                binary.display()
            )
        })?;
        if !probe.status.success() {
            return Err(format!("`{} version` exited {:?}", binary.display(), probe.status));
        }
        let version = String::from_utf8_lossy(&probe.stdout)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("unknown")
            .to_string();
        let resolved = which(&binary);
        let sha256 = resolved.as_deref().and_then(artefact_digest);
        Ok(GoPmtiles {
            binary,
            version,
            sha256,
        })
    }
}

fn which(binary: &Path) -> Option<PathBuf> {
    if binary.components().count() > 1 {
        return Some(binary.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|p| p.join(binary))
            .find(|p| p.is_file())
    })
}

/// A contender in the build scenario: one of ours, or go-pmtiles.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Contender {
    Ours(Variant),
    Go,
}

impl Contender {
    fn label(&self) -> String {
        match self {
            Contender::Ours(v) => v.label(),
            Contender::Go => GO_PMTILES.to_string(),
        }
    }
}

/// One repetition's result, as the parent keeps it.
#[derive(Debug, Clone)]
struct RepResult {
    out: RepOut,
    load: MachineLoad,
}

/// Everything one `(label, family)` accumulated across a cell's repetitions.
#[derive(Debug, Default)]
struct Accum {
    reps: Vec<RepResult>,
    failures: Vec<String>,
}

/// What the quiet gate did over a sweep, for the document to say.
static GATE_WAITED_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static GATE_GAVE_UP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static GATE_REPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// `some avg10` from `/proc/pressure/io`, as a percentage. `None` where the
/// kernel does not offer pressure stall information, which is then not gated on.
fn io_pressure_some_avg10() -> Option<f64> {
    let text = std::fs::read_to_string("/proc/pressure/io").ok()?;
    let line = text.lines().find(|l| l.starts_with("some "))?;
    line.split_whitespace()
        .find_map(|field| field.strip_prefix("avg10="))
        .and_then(|v| v.parse().ok())
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Hold a repetition back until the machine is quiet, then take the reading it
/// starts under.
///
/// The host is shared, and its one-minute load swings from single digits to
/// well over a hundred as other jobs come and go. A sweep that simply started
/// every repetition would publish whatever it happened to land on, and the first
/// full capture did: three rows in four recorded a contended machine. So each
/// repetition waits, up to [`QUIET_WAIT_ENV`] seconds, for the load per core to
/// fall under [`QUIET_BELOW_ENV`], and starts anyway when the budget runs out
/// rather than stalling a sweep behind a machine that never calms. The time
/// waited is not in any sample, and the reading it started under is the one the
/// row records, so a repetition that had to give up says so in its own row.
fn wait_for_quiet() -> MachineLoad {
    let below = env_f64(QUIET_BELOW_ENV, DEFAULT_QUIET_BELOW);
    let io_below = env_f64(QUIET_IO_BELOW_ENV, DEFAULT_QUIET_IO_BELOW);
    let budget =
        std::time::Duration::from_secs(env_f64(QUIET_WAIT_ENV, DEFAULT_QUIET_WAIT_S as f64) as u64);
    let began = Instant::now();
    GATE_REPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    loop {
        let load = MachineLoad::sample();
        let calm = load.contention_per_core.is_some_and(|c| c < below)
            && io_pressure_some_avg10().is_none_or(|p| p < io_below);
        if calm || began.elapsed() >= budget {
            GATE_WAITED_MS.fetch_add(
                began.elapsed().as_millis() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
            if !calm {
                GATE_GAVE_UP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            return load;
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
}

/// The reading a repetition is recorded under: the worse of the one it started
/// under and the one it finished under, so a spike in the middle is not missed.
fn worse(a: MachineLoad, b: MachineLoad) -> MachineLoad {
    match (a.contention_per_core, b.contention_per_core) {
        (Some(x), Some(y)) if y > x => b,
        (None, Some(_)) => b,
        _ => a,
    }
}

fn worst_load(loads: impl Iterator<Item = MachineLoad>) -> MachineLoad {
    loads
        .max_by(|a, b| {
            a.contention_per_core
                .partial_cmp(&b.contention_per_core)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or_else(MachineLoad::unknown)
}

const WALL: MetricSpec = MetricSpec {
    name: "wall",
    unit: Unit::Milliseconds,
    direction: Direction::LowerIsBetter,
};
const HEAP_PEAK: MetricSpec = MetricSpec {
    name: "heap_peak",
    unit: Unit::Bytes,
    direction: Direction::LowerIsBetter,
};
const OUTPUT_BYTES: MetricSpec = MetricSpec {
    name: "output_bytes",
    unit: Unit::Bytes,
    direction: Direction::LowerIsBetter,
};
const PEAK_RSS: MetricSpec = MetricSpec {
    name: "peak_rss",
    unit: Unit::Bytes,
    direction: Direction::LowerIsBetter,
};

/// The value every repetition agreed on, or `None` when they did not.
fn agreed<T: Clone + PartialEq>(values: impl Iterator<Item = Option<T>>) -> Option<T> {
    let all: Vec<Option<T>> = values.collect();
    let first = all.first().cloned().flatten()?;
    all.iter()
        .all(|v| v.as_ref() == Some(&first))
        .then_some(first)
}

/// The rows one accumulation earns.
struct RowSpec<'a> {
    label: &'a str,
    cell: SweepCell,
    scenario: &'static str,
    reps_declared: u32,
    timer: Option<stats::TimerProbe>,
}

fn rows_for(
    spec: &RowSpec<'_>,
    acc: &Accum,
    metrics: &[(MetricSpec, Vec<f64>)],
    skipped: Option<&str>,
) -> Vec<DocumentCell> {
    // A repetition whose archive did not read back as the tile set it was built
    // from is refused, which is a different thing from a child that crashed.
    let refused = !acc.failures.is_empty() && acc.failures.iter().all(|f| f.starts_with("attest"));
    let (outcome, reason) = match (skipped, acc.failures.is_empty()) {
        (Some(why), _) => (Outcome::Skipped, Some(why.to_string())),
        (None, false) if refused => (Outcome::Refused, Some(acc.failures.join("; "))),
        (None, false) => (Outcome::Failed, Some(acc.failures.join("; "))),
        (None, true) => (Outcome::Ok, None),
    };
    let invariants = if skipped.is_some() {
        InvariantBlock::default()
    } else {
        InvariantBlock {
            output_bytes: agreed(acc.reps.iter().map(|r| r.out.output_bytes)),
            tiles_produced: agreed(acc.reps.iter().map(|r| r.out.tiles)),
            artefact_digest: agreed(acc.reps.iter().map(|r| r.out.digest.clone())),
            root_entries: agreed(acc.reps.iter().map(|r| r.out.root_entries)),
            leaves: agreed(acc.reps.iter().map(|r| r.out.leaves)),
            peak_rss_mb: acc
                .reps
                .iter()
                .filter_map(|r| r.out.peak_rss_bytes)
                .max()
                .map(|b| b as f64 / (1024.0 * 1024.0)),
            heap_peak_bytes: acc
                .reps
                .iter()
                .filter_map(|r| r.out.heap_peak.or(r.out.finalize_heap))
                .max(),
            ..InvariantBlock::default()
        }
    };
    let load = worst_load(acc.reps.iter().map(|r| r.load));
    metrics
        .iter()
        .map(|(metric, samples)| {
            let mut row = DocumentCell::from_report(CellReport {
                labels: CellLabels {
                    backend: spec.label.to_string(),
                    scale: spec.cell.tiles() as u32,
                    source: spec.cell.source_label(),
                    cell: spec.cell.spec(),
                },
                scenario: spec.scenario,
                metric: *metric,
                isolation: Isolation::ProcessPerRep,
                oversubscribed: None,
                warmup: None,
                discarded_warmup: Vec::new(),
                reps_declared: spec.reps_declared,
                min_reps: spec.reps_declared,
                samples: samples.clone(),
                outcome,
                reason: reason.clone(),
                invariants: invariants.clone(),
                machine_load: load,
                timer: spec.timer,
            });
            row.attested = match outcome {
                Outcome::Ok => Some(true),
                Outcome::Skipped => None,
                _ => Some(false),
            };
            row
        })
        .collect()
}

fn series(acc: &Accum, f: impl Fn(&RepResult) -> Option<f64>) -> Vec<f64> {
    acc.reps.iter().filter_map(f).collect()
}

/// Run a whole writer sweep and build the document.
pub fn run_sweep(profile: Profile) -> Document {
    let exe = crate::harness::current_exe();
    let starting_load = MachineLoad::sample();
    let started = now_iso();
    let reps = write_reps(profile);
    let measurement = Measurement::probed_from(MeasurementSpec {
        unit: "fresh-process-per-cell",
        isolation: "subprocess-per-cell",
        reps: Reps::of(&[("write", reps), ("build", reps)]),
        min_reps: Reps::of(&[("write", reps), ("build", reps)]),
        seed: SEED,
        warmup: None,
        page_cache: "warm-unknown",
    });
    let mut doc = Document::new_for(FAMILY, RUNNER, profile.label(), started, measurement);
    doc.starting_load = Some(starting_load);
    let timer = Some(stats::probe_timer());
    let go = GoPmtiles::locate();
    let variants = matrix();

    for cell in cells_for(profile) {
        eprintln!("pmtiles-writer: cell {}", cell.spec());
        sweep_cell(&mut doc, &exe, cell, &variants, &go, reps, timer);
    }

    doc.rebuild_invariant_table();
    doc.finished_at = Some(now_iso());
    doc.provenance = Some(sweep_provenance(profile, &doc, &variants, &go));
    doc.stamp_dirty_from_provenance();
    doc.stamp_run_id();
    doc
}

fn sweep_cell(
    doc: &mut Document,
    exe: &Path,
    cell: SweepCell,
    variants: &[Variant],
    go: &Result<GoPmtiles, String>,
    reps: u32,
    timer: Option<stats::TimerProbe>,
) {
    // The tile set the writer-driven scenario and go-pmtiles are both built
    // from, written once and untimed, by a child. The sweep process never holds
    // a raster, so it never has a large resident set to put a floor under
    // anything it spawns, and a tile set it cannot write is a cell it cannot
    // measure.
    let tiles = match cell {
        SweepCell::Real(_) => match make_tileset(exe, cell) {
            Ok(set) => Some(set),
            Err(e) => {
                eprintln!("pmtiles-writer: the tile set for {} does not build: {e}", cell.spec());
                return;
            }
        },
        SweepCell::Synthetic(_) => None,
    };
    let tiles_root: Option<PathBuf> = tiles.as_ref().map(|(_, root)| root.clone());

    // go-pmtiles' input, untimed. A failure to build it skips the go rows with
    // the reason rather than aborting the cell.
    let mbtiles_scratch = Scratch::new(None).ok();
    let go_input: Result<(GoPmtiles, PathBuf), String> = match (go, mbtiles_scratch.as_ref()) {
        (Ok(go), Some(scratch)) => {
            let out = scratch.path().join("tiles.mbtiles");
            build_mbtiles(cell, tiles_root.as_deref(), &out).map(|()| (go.clone(), out))
        }
        (Err(why), _) => Err(why.clone()),
        (_, None) => Err("no scratch directory for the mbtiles".to_string()),
    };

    let mut contenders: Vec<Contender> = variants.iter().copied().map(Contender::Ours).collect();
    contenders.push(Contender::Go);
    let mut engine: Vec<Accum> = variants.iter().map(|_| Accum::default()).collect();
    let mut build: Vec<Accum> = contenders.iter().map(|_| Accum::default()).collect();

    for rep in 0..reps as usize {
        // The starting contender rotates, so no variant is always the one that
        // runs right after a hot engine pass or right after a cold one.
        for k in 0..contenders.len() {
            let at = (k + rep) % contenders.len();
            let contender = contenders[at].clone();
            let Contender::Ours(variant) = &contender else {
                // go-pmtiles: build only.
                if let Ok((go, input)) = &go_input {
                    match go_rep(exe, go, input, cell, tiles_root.as_deref()) {
                        Ok(r) => build[at].reps.push(r),
                        Err(e) => build[at].failures.push(e),
                    }
                }
                continue;
            };
            if cell.is_real() {
                match child_rep(exe, variant, cell, Mode::Engine, tiles_root.as_deref()) {
                    Ok(r) => engine[at].reps.push(r),
                    Err(e) => engine[at].failures.push(e),
                }
            }
            match child_rep(exe, variant, cell, Mode::Build, tiles_root.as_deref()) {
                Ok(r) => build[at].reps.push(r),
                Err(e) => build[at].failures.push(e),
            }
        }
    }

    for (i, variant) in variants.iter().enumerate() {
        let label = variant.label();
        if cell.is_real() {
            let acc = &engine[i];
            let mk = |scenario| RowSpec {
                label: &label,
                cell,
                scenario,
                reps_declared: reps,
                timer,
            };
            for row in rows_for(
                &mk("write_ingest"),
                acc,
                &[
                    (WALL, series(acc, |r| r.out.ingest_ms)),
                    (HEAP_PEAK, series(acc, |r| r.out.ingest_heap.map(|b| b as f64))),
                ],
                None,
            )
            .into_iter()
            .chain(rows_for(
                &mk("write_finalize"),
                acc,
                &[
                    (WALL, series(acc, |r| r.out.finalize_ms)),
                    (HEAP_PEAK, series(acc, |r| r.out.finalize_heap.map(|b| b as f64))),
                ],
                None,
            ))
            .chain(rows_for(
                &mk("write_output"),
                acc,
                &[
                    (OUTPUT_BYTES, series(acc, |r| r.out.output_bytes.map(|b| b as f64))),
                    (PEAK_RSS, series(acc, |r| r.out.peak_rss_bytes.map(|b| b as f64))),
                ],
                None,
            )) {
                doc.push(row);
            }
        }
    }
    for (i, contender) in contenders.iter().enumerate() {
        let label = contender.label();
        let acc = &build[i];
        let skipped = match (contender, &go_input) {
            (Contender::Go, Err(why)) => Some(why.as_str()),
            _ => None,
        };
        let spec = RowSpec {
            label: &label,
            cell,
            scenario: "build_from_tiles",
            reps_declared: reps,
            timer,
        };
        let mut metrics = vec![
            (WALL, series(acc, |r| r.out.build_ms)),
            (OUTPUT_BYTES, series(acc, |r| r.out.output_bytes.map(|b| b as f64))),
            (PEAK_RSS, series(acc, |r| r.out.peak_rss_bytes.map(|b| b as f64))),
        ];
        if matches!(contender, Contender::Ours(_)) {
            metrics.insert(1, (HEAP_PEAK, series(acc, |r| r.out.heap_peak.map(|b| b as f64))));
        }
        for row in rows_for(&spec, acc, &metrics, skipped) {
            doc.push(row);
        }
    }
}

/// The tile set, as a scratch directory and the root of the tree inside it.
fn make_tileset(exe: &Path, cell: SweepCell) -> Result<(Scratch, PathBuf), String> {
    let scratch = Scratch::new(None).map_err(|e| format!("no scratch directory: {e}"))?;
    let args = vec![
        "-".to_string(),
        cell.spec(),
        Mode::Tileset.as_str().to_string(),
        scratch.path().display().to_string(),
    ];
    run_child_process(exe, &args, &scratch.path().join("stderr.txt"))?;
    let root = scratch.path().join("tree");
    Ok((scratch, root))
}

/// Spawn this binary as one child repetition and read its reply.
fn run_child_process(exe: &Path, args: &[String], stderr_to: &Path) -> Result<RepOut, String> {
    let mut command = Command::new(exe);
    command.arg(SINGLE_FLAG).args(args);
    let spawned = spawn_measured(&mut command, stderr_to)?;
    if !spawned.success {
        return Err(format!("the child failed: {}", tail(stderr_to)));
    }
    let reply: ChildReply = serde_json::from_str(spawned.stdout.trim())
        .map_err(|e| format!("the child's reply does not parse: {e}"))?;
    match reply {
        ChildReply::Ok(out) => Ok(out),
        ChildReply::Err(e) => Err(e),
    }
}

/// Spawn one child repetition and attest what it built.
fn child_rep(
    exe: &Path,
    variant: &Variant,
    cell: SweepCell,
    mode: Mode,
    tiles_root: Option<&Path>,
) -> Result<RepResult, String> {
    let scratch = Scratch::new(None).map_err(|e| format!("no scratch directory: {e}"))?;
    let started_under = wait_for_quiet();
    let out_dir = scratch.path().join("out");
    let mut args = vec![
        variant.label(),
        cell.spec(),
        mode.as_str().to_string(),
        out_dir.display().to_string(),
    ];
    // Only the writer-driven child reads the tile set. The engine-driven one
    // makes its own tiles, and the parent attests them against the same set.
    if let (Mode::Build, Some(root)) = (mode, tiles_root) {
        args.push(root.display().to_string());
    }
    let ran = run_child_process(exe, &args, &scratch.path().join("stderr.txt"));
    let load = worse(started_under, MachineLoad::sample());
    let out = ran
        .map_err(|e| format!("{} {} {}: {e}", variant.label(), cell.spec(), mode.as_str()))?;
    attest_archive(&out_dir.join("pyramid.pmtiles"), cell, tiles_root).map_err(|e| {
        format!("attest {} {} {}: {e}", variant.label(), cell.spec(), mode.as_str())
    })?;
    Ok(RepResult { out, load })
}

/// One go-pmtiles conversion, timed by a helper child and attested here.
fn go_rep(
    exe: &Path,
    go: &GoPmtiles,
    input: &Path,
    cell: SweepCell,
    tiles_root: Option<&Path>,
) -> Result<RepResult, String> {
    let scratch = Scratch::new(None).map_err(|e| format!("no scratch directory: {e}"))?;
    let started_under = wait_for_quiet();
    let out_dir = scratch.path().join("out");
    let args = vec![
        "-".to_string(),
        cell.spec(),
        Mode::Go.as_str().to_string(),
        out_dir.display().to_string(),
        go.binary.display().to_string(),
        input.display().to_string(),
    ];
    let ran = run_child_process(exe, &args, &scratch.path().join("stderr.txt"));
    let load = worse(started_under, MachineLoad::sample());
    let out = ran.map_err(|e| format!("go-pmtiles {}: {e}", cell.spec()))?;
    attest_archive(&out_dir.join("pyramid.pmtiles"), cell, tiles_root)
        .map_err(|e| format!("attest go-pmtiles {}: {e}", cell.spec()))?;
    Ok(RepResult { out, load })
}

/// The last lines of a file, for an error message.
fn tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().rev().take(6).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join(" | ")
}

/// Write go-pmtiles' input: the same tiles, as an MBTiles file.
fn build_mbtiles(cell: SweepCell, tiles_root: Option<&Path>, out: &Path) -> Result<(), String> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join(MBTILES_SCRIPT);
    let mut command = Command::new("python3");
    command.arg("-I").arg(&script);
    match (cell, tiles_root) {
        (SweepCell::Real(_), Some(root)) => command.arg("tree").arg(root),
        (SweepCell::Synthetic(z), _) => command.arg("synthetic").arg(z.to_string()),
        (SweepCell::Real(_), None) => return Err("no tile set to convert".to_string()),
    };
    command.arg(out);
    let output = command
        .output()
        .map_err(|e| format!("python3 does not run: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "the mbtiles builder failed: {}",
            String::from_utf8_lossy(&output.stderr).lines().last().unwrap_or("")
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

fn sweep_provenance(
    profile: Profile,
    doc: &Document,
    variants: &[Variant],
    go: &Result<GoPmtiles, String>,
) -> serde_json::Value {
    let scratch = crate::family::Family::Storage.scratch_root();
    let provenance = crate::provenance::Provenance::capture_for_document(&scratch);
    for warning in provenance.document_provenance_warnings() {
        eprintln!("{warning}");
    }
    let allow_dirty = std::env::var("STORAGE_ALLOW_DIRTY").is_ok();
    let scales: Vec<u64> = {
        let mut seen: Vec<u64> = cells_for(profile).iter().map(SweepCell::tiles).collect();
        seen.sort_unstable();
        seen.dedup();
        seen
    };
    let external = match go {
        Ok(go) => serde_json::json!({
            "go-pmtiles": {
                "version": go.version,
                "binary": go.binary.display().to_string(),
                "sha256": go.sha256,
            }
        }),
        Err(why) => serde_json::json!({ "go-pmtiles": { "unavailable": why } }),
    };
    provenance.to_document_block(
        &serde_json::json!({
            "argv": std::env::args().collect::<Vec<_>>(),
            "command": "pmtiles-writer",
            "cwd": std::env::current_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
            "env": {
                "STORAGE_ALLOW_DIRTY": std::env::var("STORAGE_ALLOW_DIRTY").ok(),
                "RUSTFLAGS": std::env::var("RUSTFLAGS").ok(),
                "BENCH_DAEMON_ARCH": std::env::var("BENCH_DAEMON_ARCH").ok(),
                "TMPDIR": std::env::var("TMPDIR").ok(),
                GO_ENV: std::env::var(GO_ENV).ok(),
            },
            "resolved": {
                "profile": profile.label(),
                "reps": doc.measurement.reps,
                "scenarios": SCENARIOS,
                "scales": scales,
                "cells": sweep_cells(profile),
                "variants": variants.iter().map(Variant::label).collect::<Vec<_>>(),
                "coreWriterKnobs": cfg!(core_writer_knobs),
                "quietGate": {
                    "belowContentionPerCore": env_f64(QUIET_BELOW_ENV, DEFAULT_QUIET_BELOW),
                    "belowIoPressureSomeAvg10Pct": env_f64(QUIET_IO_BELOW_ENV, DEFAULT_QUIET_IO_BELOW),
                    "ioPressureAvailable": io_pressure_some_avg10().is_some(),
                    "maxWaitPerRepetitionS": env_f64(QUIET_WAIT_ENV, DEFAULT_QUIET_WAIT_S as f64),
                    "repetitionsGated": GATE_REPS.load(std::sync::atomic::Ordering::Relaxed),
                    "repetitionsStartedBusy": GATE_GAVE_UP.load(std::sync::atomic::Ordering::Relaxed),
                    "waitedS": GATE_WAITED_MS.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1000.0,
                },
                "externalTools": external,
            },
        }),
        allow_dirty,
    )
}
