//! The PVec row of the storage family (libviprs/libviprs#1331).
//!
//! One scan is generated into four things, each in a process of its own:
//!
//! | row | what writes it |
//! |---|---|
//! | `pmtiles` | libviprs's PMTiles sink: the previous release's sink, and what a caller used before PVec |
//! | `pvec` | libviprs's PVec sink (`sink_pvec::PvecSink`), then `finish_to` |
//! | `gdal-cog` | `gdal_translate -of COG` with overviews, the C reference that offers one tiled file with overviews |
//! | `libvips-zip` | `vips dzsave --container zip`, PNG tiles, the second reference |
//!
//! Both of ours get the same raster, the same plan (Google layout, 512 px tiles) and the same
//! engine and budget, and write PNG tiles; `ours_write_the_same_png_tiles...` holds the PVec
//! tiles byte for byte to the PMTiles ones, so neither has a codec advantage. GDAL's COG driver
//! has no PNG codec, so that row writes DEFLATE strips with a horizontal predictor, and the
//! document says so in the row's source label: it is a reference for wall time and memory, and
//! its bytes are not comparable with a PNG per tile.
//!
//! # What the document says about each metric
//!
//! * `wall`: milliseconds. For ours, from the sink opening to the artifact published, with the
//!   source raster built before the clock. For GDAL and libvips, spawn to reap of the tool,
//!   which also reads the scan from a PGM file that was written untimed.
//! * `throughput`: grid tiles per second, `cell.tiles() / wall`, the same numerator for every row
//!   (PMTiles stores the Google padding too and the tools count their own tiles differently).
//! * `peak_rss`: the process's peak resident set. `VmHWM` read by the child for ours, `ru_maxrss`
//!   from the reaping `wait4` for the tools. Ours holds the source raster, which is 248 MB at
//!   A0, and the tools stream it from the file, so the figures describe what each process costs,
//!   not the same working set.
//! * `heap_peak` (ours only): live heap above what the source raster holds, from the counting
//!   allocator.
//! * `output_bytes`: the artifact's size.
//!
//! # Attestation
//!
//! Every repetition is checked from the artifact it left, never from what the child reported.
//! PMTiles and PVec are read back through their own readers: the tile count must equal what the
//! plan holds, and every tile sampled must be a 512 px PNG. GDAL's file is read by `gdalinfo`
//! (the size must be the scan's and the overview count the pyramid's), and the zip is listed
//! (the PNG entries must equal the pyramid's tile count). That is structural for the tools; it is
//! not a pixel comparison, and the document does not claim one.
//!
//! # What was not run
//!
//! The arm64 run. `notRun` in the document's provenance says so with the reason, so a reader of
//! the archive does not mistake a missing arch for a passing one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use libviprs::pmtiles::Reader;
use libviprs::raster_pvec::pyramid::{RasterPlacement, RasterPyramidOptions, prepare_raster};
use libviprs::sink::TileFormat;
use libviprs::sink_pmtiles::PmTilesSink;
use libviprs::sink_pvec::PvecSink;
use libviprs::{EngineBuilder, EngineKind, Layout, PixelFormat, PyramidPlanner, Raster};
use pvec_facade::unstable::RasterId;
use pvec_facade::{Bounds2, Curve, EntityId, Manifest, PvecReader, PvecWriter};
use pvec_geometry::{Point2, Polyline};

use super::cells::{Profile, SEED};
use super::document::{
    CellLabels, CellReport, Document, DocumentCell, FAMILY, InvariantBlock, MachineLoad,
    Measurement, MeasurementSpec, RUNNER, Reps,
};
use super::heap;
use super::scenarios::{Direction, Isolation, MetricSpec, Outcome, Unit};
use super::stats;
use super::writer_sweep::{
    ChildReply, RepOut, env_f64, spawn_measured, tail, vm_hwm_bytes, wait_for_quiet, worse, worst_load,
};
use super::{Scratch, artefact_digest, now_iso, occupancy};

/// The file this sweep writes, inside the storage family's report directory.
pub const DOCUMENT_NAME: &str = "pvec-results.json";

/// The hidden subcommand a parent uses to re-invoke itself as one repetition.
pub const SINGLE_FLAG: &str = "--pvec-single";

/// The one scenario this sweep publishes.
pub const SCENARIO: &str = "generate_scan";

/// The tile edge of every row.
pub const TILE_EDGE: u32 = 512;

/// The streaming budget both of ours are given.
pub const BUDGET_BYTES: u64 = 512 * 1024 * 1024;

/// Where the PGM the tools read is written, as the file name inside the repetition's directory.
const INPUT_NAME: &str = "scan.pgm";

/// How many tiles a read-back samples.
const ATTEST_SAMPLE: usize = 64;

/// Environment variables naming the external tools.
pub const GDAL_ENV: &str = "BENCH_GDAL_TRANSLATE";
/// See [`GDAL_ENV`].
pub const VIPS_ENV: &str = "BENCH_VIPS";

/// A row of the sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contender {
    /// libviprs's PMTiles sink.
    Pmtiles,
    /// libviprs's PVec sink.
    Pvec,
    /// GDAL's COG driver.
    GdalCog,
    /// libvips `dzsave` into a zip.
    VipsZip,
}

impl Contender {
    /// All four, in the order a sweep rotates them.
    pub const ALL: [Contender; 4] = [
        Contender::Pmtiles,
        Contender::Pvec,
        Contender::GdalCog,
        Contender::VipsZip,
    ];

    /// The backend label the document carries.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Contender::Pmtiles => "pmtiles",
            Contender::Pvec => "pvec",
            Contender::GdalCog => "gdal-cog",
            Contender::VipsZip => "libvips-zip",
        }
    }

    /// The contender a label names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Contender> {
        Contender::ALL.into_iter().find(|c| c.label() == s)
    }

    fn source_label(self) -> &'static str {
        match self {
            Contender::GdalCog => "scan-gray8 (deflate strips, predictor 2, not PNG)",
            _ => "scan-gray8 (png tiles)",
        }
    }
}

/// The file a contender leaves in a repetition's directory.
#[must_use]
pub fn artifact_name(c: Contender) -> &'static str {
    match c {
        Contender::Pmtiles => "pyramid.pmtiles",
        Contender::Pvec => "pyramid.pvec",
        Contender::GdalCog => "pyramid.tif",
        Contender::VipsZip => "pyramid.zip",
    }
}

/// The scan a row generates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanCell {
    /// Pixels across.
    pub width: u32,
    /// Pixels down.
    pub height: u32,
}

impl ScanCell {
    /// A 400 dpi A0 sheet: 18,720 by 13,240, 1,308 tiles at 512 px.
    #[must_use]
    pub fn a0() -> ScanCell {
        ScanCell {
            width: 18_720,
            height: 13_240,
        }
    }

    /// Small enough for a pull request's run, never published.
    #[must_use]
    pub fn ci() -> ScanCell {
        ScanCell {
            width: 2_048,
            height: 1_448,
        }
    }

    /// The name the document knows the cell by.
    #[must_use]
    pub fn spec(&self) -> String {
        format!("scan@{}x{}", self.width, self.height)
    }

    /// The cell a spec names.
    #[must_use]
    pub fn parse(s: &str) -> Option<ScanCell> {
        let (w, h) = s.strip_prefix("scan@")?.split_once('x')?;
        Some(ScanCell {
            width: w.parse().ok()?,
            height: h.parse().ok()?,
        })
    }

    fn plan(&self) -> libviprs::PyramidPlan {
        PyramidPlanner::new(self.width, self.height, TILE_EDGE, 0, Layout::Google)
            .expect("a scan plans")
            .plan()
    }

    /// Tiles in the pyramid's own grids: `cols * rows` over every level, with none of the
    /// Google layout's padding.
    #[must_use]
    pub fn tiles(&self) -> u64 {
        self.plan()
            .levels
            .iter()
            .map(|l| u64::from(l.width.div_ceil(TILE_EDGE)) * u64::from(l.height.div_ceil(TILE_EDGE)))
            .sum()
    }

    /// Tiles in the pyramid libvips cuts. `dzsave --depth onetile` keeps halving until the longer
    /// side is at most half the tile size (256 here), where ours stops at the first level of one
    /// tile (a longer side of 512 or less), so libvips ends on one more single-tile level than
    /// ours whenever the last level of ours is wider than 256. Observed with libvips 8.16 on the
    /// ci cell (18 against 17) and the A0 cell (1,309 against 1,308), and held by the attestation.
    fn vips_tiles(&self) -> u64 {
        let (mut w, mut h) = (self.width, self.height);
        let mut total = u64::from(w.div_ceil(TILE_EDGE)) * u64::from(h.div_ceil(TILE_EDGE));
        while w.max(h) > TILE_EDGE / 2 {
            w = w.div_ceil(2);
            h = h.div_ceil(2);
            total += u64::from(w.div_ceil(TILE_EDGE)) * u64::from(h.div_ceil(TILE_EDGE));
        }
        total
    }

    /// Levels in the pyramid, down to one tile.
    fn levels(&self) -> usize {
        self.plan().levels.len()
    }
}

/// A scanned sheet: white paper, a grid of dark lines and a few diagonals, and a left third of
/// photographic texture standing for an aerial underlay, so the pyramid has a real size.
#[must_use]
pub fn scan(w: u32, h: u32) -> Raster {
    let mut data = vec![255u8; w as usize * h as usize];
    for y in 0..h {
        let row = &mut data[y as usize * w as usize..][..w as usize];
        if y % 512 < 3 {
            row.fill(20);
        } else {
            for x in (0..w).step_by(512) {
                for dx in 0..3 {
                    if let Some(p) = row.get_mut((x + dx) as usize) {
                        *p = 20;
                    }
                }
            }
            row[(y as usize * 7 / 5) % w as usize] = 30;
        }
        for (x, p) in row.iter_mut().enumerate().take(w as usize / 3) {
            let hash = (x as u32).wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503);
            *p = ((hash >> 11) & 0xF0) as u8 | 0x08;
        }
    }
    Raster::new(w, h, PixelFormat::Gray8, data).expect("a scan is well formed")
}

// ---------------------------------------------------------------------------
// The tools
// ---------------------------------------------------------------------------

/// An external tool a row runs.
#[derive(Debug, Clone)]
pub struct Rival {
    /// The binary.
    pub binary: PathBuf,
    /// What it says its version is.
    pub version: String,
    /// SHA-256 of the binary, when it can be read.
    pub sha256: Option<String>,
}

impl Rival {
    /// Find the tool a contender runs: `over`, else its environment variable, else the tool on
    /// `PATH`. `Err` is the reason the row will give for being skipped.
    ///
    /// # Errors
    ///
    /// A contender that is not external, or a tool that does not run.
    pub fn locate_with(c: Contender, over: Option<&str>) -> Result<Rival, String> {
        let (env, default, flag) = match c {
            Contender::GdalCog => (GDAL_ENV, "gdal_translate", "--version"),
            Contender::VipsZip => (VIPS_ENV, "vips", "--version"),
            _ => return Err(format!("{} is not external", c.label())),
        };
        let binary = over
            .map(PathBuf::from)
            .or_else(|| std::env::var(env).ok().filter(|v| !v.is_empty()).map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from(default));
        let out = Command::new(&binary).arg(flag).output().map_err(|e| {
            format!(
                "{default} is not available ({}: {e}); set {env} or put `{default}` on PATH",
                binary.display()
            )
        })?;
        if !out.status.success() {
            return Err(format!("`{} {flag}` exited {:?}", binary.display(), out.status));
        }
        let version = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("unknown")
            .to_string();
        let resolved = if binary.components().count() > 1 {
            Some(binary.clone())
        } else {
            std::env::var_os("PATH").and_then(|paths| {
                std::env::split_paths(&paths)
                    .map(|p| p.join(&binary))
                    .find(|p| p.is_file())
            })
        };
        let sha256 = resolved.as_deref().and_then(artefact_digest);
        Ok(Rival {
            binary,
            version,
            sha256,
        })
    }

    /// [`Rival::locate_with`] with no override.
    ///
    /// # Errors
    ///
    /// As [`Rival::locate_with`].
    pub fn locate(c: Contender) -> Result<Rival, String> {
        Rival::locate_with(c, None)
    }
}

// ---------------------------------------------------------------------------
// One repetition, in a child
// ---------------------------------------------------------------------------

fn outline(w: u32, h: u32) -> PvecWriter {
    let mut pw = PvecWriter::new(Manifest {
        metres_per_unit: 1.0,
        bounds: Bounds2 {
            min: [0.0, 0.0],
            max: [f64::from(w), f64::from(h)],
        },
        capabilities: Vec::new(),
        producer_id: "libviprs-bench-pvec".into(),
        producer_version: "1".into(),
    })
    .expect("a manifest");
    let ring = Polyline::new(vec![
        Point2 { x: 0.0, y: 0.0 },
        Point2 { x: f64::from(w), y: 0.0 },
        Point2 { x: f64::from(w), y: f64::from(h) },
        Point2 { x: 0.0, y: 0.0 },
    ])
    .expect("an outline");
    pw.push(EntityId::new(0).expect("id 0"), &Curve::Polyline(ring))
        .expect("an outline entity");
    pw
}

/// One repetition, in this process. `extra` is the tool's binary and, for GDAL and libvips, the
/// scan file.
///
/// # Errors
///
/// A message naming what failed.
pub fn one_rep(
    c: Contender,
    cell: ScanCell,
    out_dir: &Path,
    extra: &[String],
) -> Result<RepOut, String> {
    std::fs::create_dir_all(out_dir).map_err(|e| format!("no output directory: {e}"))?;
    let artifact = out_dir.join(artifact_name(c));
    match c {
        Contender::Pmtiles | Contender::Pvec => {
            let plan = cell.plan();
            // Before the clock and before the counting window, as the storage family does.
            let raster = scan(cell.width, cell.height);
            let armed = heap::arm();
            let started = Instant::now();
            if c == Contender::Pmtiles {
                let sink = PmTilesSink::builder(&artifact)
                    .plan(plan.clone())
                    .tile_format(TileFormat::Png)
                    .build()
                    .map_err(|e| format!("the archive sink does not build: {e}"))?;
                EngineBuilder::new(&raster, plan, sink)
                    .with_engine(EngineKind::Streaming)
                    .with_memory_budget(BUDGET_BYTES)
                    .run()
                    .map_err(|e| format!("the archive run failed: {e}"))?;
            } else {
                let options = RasterPyramidOptions::new(RasterPlacement::Explicit([
                    1.0,
                    0.0,
                    0.0,
                    -1.0,
                    0.0,
                    f64::from(cell.height),
                ]));
                let job = prepare_raster(&raster, &plan, &options).map_err(|e| e.to_string())?;
                let sink = PvecSink::new(outline(cell.width, cell.height), &job.pyramid, &options)
                    .map_err(|e| e.to_string())?;
                EngineBuilder::new(job.source.as_ref(), job.pyramid.plan.clone(), &sink)
                    .with_engine(EngineKind::Streaming)
                    .with_memory_budget(BUDGET_BYTES)
                    .run()
                    .map_err(|e| format!("the PVec run failed: {e}"))?;
                let staging = out_dir.join("stage");
                std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
                sink.finish_to(&artifact, &staging)
                    .map_err(|e| format!("the artifact does not publish: {e}"))?;
            }
            let build_ms = started.elapsed().as_secs_f64() * 1000.0;
            Ok(RepOut {
                build_ms: Some(build_ms),
                heap_peak: armed.peak_bytes(),
                output_bytes: occupancy(&artifact).map(|o| o.0),
                tiles: Some(cell.tiles()),
                digest: artefact_digest(&artifact),
                ..RepOut::default()
            })
        }
        Contender::GdalCog | Contender::VipsZip => {
            let [binary, input] = extra else {
                return Err("a tool row wants the binary and the scan file".to_string());
            };
            let mut command = Command::new(binary);
            if c == Contender::GdalCog {
                command
                    .args(["-q", "-of", "COG", "-co", "BLOCKSIZE=512", "-co", "COMPRESS=DEFLATE"])
                    .args(["-co", "PREDICTOR=2", "-co", "OVERVIEWS=AUTO", "-co", "NUM_THREADS=1"])
                    .arg(input)
                    .arg(&artifact);
            } else {
                command
                    .arg("dzsave")
                    .arg(input)
                    // The `.zip` in the name is what makes vips write a zip under that name; without it
                    // the file is called `pyramid` and nothing reads it as an archive.
                    .arg(&artifact)
                    .args(["--container", "zip", "--suffix", ".png", "--tile-size", "512"])
                    .args(["--overlap", "0", "--depth", "onetile", "--layout", "google"]);
            }
            let spawned = spawn_measured(&mut command, &out_dir.join("tool-stderr.txt"))?;
            if !spawned.success {
                return Err(format!(
                    "{} failed: {}",
                    c.label(),
                    tail(&out_dir.join("tool-stderr.txt"))
                ));
            }
            Ok(RepOut {
                build_ms: Some(spawned.wall_ms),
                peak_rss_bytes: spawned.peak_rss_bytes,
                output_bytes: occupancy(&artifact).map(|o| o.0),
                tiles: Some(cell.tiles()),
                digest: artefact_digest(&artifact),
                ..RepOut::default()
            })
        }
    }
}

/// Write the PGM the tools read, untimed, from a child so the sweep never holds a raster.
fn write_input(cell: ScanCell, out_dir: &Path) -> Result<RepOut, String> {
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let raster = scan(cell.width, cell.height);
    let mut pgm = format!("P5\n{} {}\n255\n", cell.width, cell.height).into_bytes();
    pgm.extend_from_slice(raster.data());
    std::fs::write(out_dir.join(INPUT_NAME), pgm).map_err(|e| e.to_string())?;
    Ok(RepOut::default())
}

/// The child body: `<contender|input> <cell> <out-dir> [extra...]`.
pub fn run_child(args: &[String]) -> i32 {
    let [who, cell, out, extra @ ..] = args else {
        eprintln!("{SINGLE_FLAG} <contender|input> <cell> <out-dir> [extra...]");
        return 2;
    };
    let Some(cell) = ScanCell::parse(cell) else {
        eprintln!("unparseable cell: {args:?}");
        return 2;
    };
    let outcome = if who == "input" {
        write_input(cell, Path::new(out))
    } else if let Some(c) = Contender::parse(who) {
        one_rep(c, cell, Path::new(out), extra)
    } else {
        eprintln!("unknown contender {who:?}");
        return 2;
    };
    let reply = match outcome {
        Ok(mut out) => {
            if out.peak_rss_bytes.is_none() {
                out.peak_rss_bytes = vm_hwm_bytes();
            }
            ChildReply::Ok(out)
        }
        Err(e) => ChildReply::Err(e),
    };
    println!("{}", serde_json::to_string(&reply).expect("a reply serialises"));
    0
}

/// Dispatch [`SINGLE_FLAG`] out of `argv`.
pub fn maybe_run_single_subcommand() -> Option<i32> {
    let argv: Vec<String> = std::env::args().collect();
    let at = argv.iter().position(|a| a == SINGLE_FLAG)?;
    Some(run_child(&argv[at + 1..]))
}

// ---------------------------------------------------------------------------
// Attestation
// ---------------------------------------------------------------------------

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// A PNG's width and height from its header, or `None` when it is not one.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    (bytes.len() > 24 && bytes[..8] == PNG_MAGIC).then(|| {
        (
            u32::from_be_bytes(bytes[16..20].try_into().expect("4 bytes")),
            u32::from_be_bytes(bytes[20..24].try_into().expect("4 bytes")),
        )
    })
}

/// Every in-grid `(level, col, row)` of the cell.
fn grid_coords(cell: ScanCell) -> Vec<(u32, u32, u32)> {
    let mut out = Vec::new();
    for l in &cell.plan().levels {
        for row in 0..l.height.div_ceil(TILE_EDGE) {
            for col in 0..l.width.div_ceil(TILE_EDGE) {
                out.push((l.level, col, row));
            }
        }
    }
    out
}

/// A seeded sample of the grid.
fn sample(mut coords: Vec<(u32, u32, u32)>) -> Vec<(u32, u32, u32)> {
    let mut rng = stats::Splitmix::new(SEED);
    for i in (1..coords.len()).rev() {
        let j = rng.below(i + 1);
        coords.swap(i, j);
    }
    coords.truncate(ATTEST_SAMPLE);
    coords
}

/// The PNG tiles of the PMTiles archive and the PVec artifact in two directories, keyed by
/// `(level, col, row)`, over the in-grid tiles the PVec artifact holds.
///
/// # Errors
///
/// A message when either artifact does not read.
#[allow(clippy::type_complexity)]
pub fn png_tiles_of(
    pmtiles_dir: &Path,
    pvec_dir: &Path,
    cell: ScanCell,
) -> Result<(BTreeMap<(u32, u32, u32), Vec<u8>>, BTreeMap<(u32, u32, u32), Vec<u8>>), String> {
    let archive = Reader::try_open(&pmtiles_dir.join(artifact_name(Contender::Pmtiles)))
        .map_err(|e| format!("the archive does not open: {e}"))?;
    let bytes = std::fs::read(pvec_dir.join(artifact_name(Contender::Pvec))).map_err(|e| e.to_string())?;
    let reader = PvecReader::open(bytes).map_err(|e| format!("the artifact does not open: {e}"))?;
    let id = RasterId::new(0).map_err(|e| e.to_string())?;
    let (mut pm, mut pv) = (BTreeMap::new(), BTreeMap::new());
    for (l, c, r) in grid_coords(cell) {
        if let Some(t) = reader
            .raster_tile(id, l, c, r)
            .map_err(|e| format!("tile {l}/{c}/{r}: {e}"))?
        {
            pv.insert((l, c, r), t);
        }
        if let Some(t) = archive
            .get_tile(l as u8, c, r)
            .map_err(|e| format!("pmtiles tile {l}/{c}/{r}: {e}"))?
        {
            pm.insert((l, c, r), t);
        }
    }
    Ok((pm, pv))
}

/// Read the artifact back and say whether it holds the pyramid it was built from.
///
/// Nothing here reads what the child reported about its tiles. See the module docs for what each
/// contender is held to.
///
/// # Errors
///
/// A message naming what the artifact did not hold.
pub fn attest(c: Contender, cell: ScanCell, out_dir: &Path, _out: &RepOut) -> Result<(), String> {
    let artifact = out_dir.join(artifact_name(c));
    let expected = cell.tiles();
    let tile_ok = |bytes: &[u8], what: &str| match png_dimensions(bytes) {
        Some((w, h)) if w == TILE_EDGE && h == TILE_EDGE => Ok(()),
        other => Err(format!("{what} is not a {TILE_EDGE} px PNG ({other:?})")),
    };
    match c {
        Contender::Pmtiles => {
            let reader = Reader::try_open(&artifact).map_err(|e| format!("the archive does not open: {e}"))?;
            let grid = grid_coords(cell);
            let held = grid
                .iter()
                .filter(|(l, c, r)| reader.get_tile(*l as u8, *c, *r).ok().flatten().is_some())
                .count() as u64;
            if held != expected {
                return Err(format!("the archive holds {held} of the {expected} grid tiles"));
            }
            for (l, col, row) in sample(grid) {
                let t = reader
                    .get_tile(l as u8, col, row)
                    .map_err(|e| format!("tile {l}/{col}/{row}: {e}"))?
                    .ok_or_else(|| format!("tile {l}/{col}/{row} is missing"))?;
                tile_ok(&t, &format!("tile {l}/{col}/{row}"))?;
            }
        }
        Contender::Pvec => {
            let bytes = std::fs::read(&artifact).map_err(|e| e.to_string())?;
            let reader = PvecReader::open(bytes).map_err(|e| format!("the artifact does not open: {e}"))?;
            let placements = reader.raster_placements().map_err(|e| e.to_string())?;
            let [p] = placements.as_slice() else {
                return Err(format!("the artifact places {} rasters", placements.len()));
            };
            if (p.width, p.height) != (cell.width, cell.height) {
                return Err(format!("the placement is {}x{}", p.width, p.height));
            }
            let id = RasterId::new(0).map_err(|e| e.to_string())?;
            let grid = grid_coords(cell);
            let mut held = 0u64;
            for &(l, col, row) in &grid {
                if reader
                    .raster_tile(id, l, col, row)
                    .map_err(|e| format!("tile {l}/{col}/{row}: {e}"))?
                    .is_some()
                {
                    held += 1;
                }
            }
            if held != expected {
                return Err(format!("the artifact holds {held} of the {expected} grid tiles"));
            }
            for (l, col, row) in sample(grid) {
                let t = reader
                    .raster_tile(id, l, col, row)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("tile {l}/{col}/{row} is missing"))?;
                tile_ok(&t, &format!("tile {l}/{col}/{row}"))?;
            }
        }
        Contender::GdalCog => {
            let gdal = Rival::locate(Contender::GdalCog)?;
            let info = gdal.binary.with_file_name("gdalinfo");
            let out = Command::new(&info)
                .arg("-json")
                .arg(&artifact)
                .output()
                .map_err(|e| format!("gdalinfo does not run: {e}"))?;
            let v: serde_json::Value = serde_json::from_slice(&out.stdout)
                .map_err(|e| format!("gdalinfo does not answer: {e}"))?;
            let size = (v["size"][0].as_u64(), v["size"][1].as_u64());
            if size != (Some(u64::from(cell.width)), Some(u64::from(cell.height))) {
                return Err(format!("the file is {size:?}, not {}x{}", cell.width, cell.height));
            }
            let overviews = v["bands"][0]["overviews"].as_array().map_or(0, Vec::len);
            if overviews != cell.levels() - 1 {
                return Err(format!(
                    "the file has {overviews} overviews and the pyramid {}",
                    cell.levels() - 1
                ));
            }
        }
        Contender::VipsZip => {
            let script = "import sys,zipfile; n=zipfile.ZipFile(sys.argv[1]).namelist(); print(sum(1 for x in n if x.endswith('.png')))";
            let out = Command::new("python3")
                .args(["-I", "-c", script])
                .arg(&artifact)
                .output()
                .map_err(|e| format!("python3 does not run: {e}"))?;
            let pngs: u64 = String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse()
                .map_err(|_| "the zip does not list".to_string())?;
            let want = cell.vips_tiles();
            if pngs != want {
                return Err(format!("the zip holds {pngs} PNG tiles and libvips's pyramid of this scan {want}"));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The parent
// ---------------------------------------------------------------------------

struct RepResult {
    out: RepOut,
    load: MachineLoad,
}

#[derive(Default)]
struct Accum {
    reps: Vec<RepResult>,
    failures: Vec<String>,
}

const WALL: MetricSpec = MetricSpec {
    name: "wall",
    unit: Unit::Milliseconds,
    direction: Direction::LowerIsBetter,
};
const THROUGHPUT: MetricSpec = MetricSpec {
    name: "throughput",
    unit: Unit::PerSecond,
    direction: Direction::HigherIsBetter,
};
const PEAK_RSS: MetricSpec = MetricSpec {
    name: "peak_rss",
    unit: Unit::Bytes,
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

fn agreed<T: Clone + PartialEq>(values: impl Iterator<Item = Option<T>>) -> Option<T> {
    let all: Vec<Option<T>> = values.collect();
    let first = all.first().cloned().flatten()?;
    all.iter().all(|v| v.as_ref() == Some(&first)).then_some(first)
}

/// Spawn this binary as one child repetition and read its reply.
fn run_child_process(exe: &Path, args: &[String], stderr_to: &Path) -> Result<RepOut, String> {
    let mut command = Command::new(exe);
    command.arg(SINGLE_FLAG).args(args);
    let spawned = spawn_measured(&mut command, stderr_to)?;
    if !spawned.success {
        return Err(format!("the child failed: {}", tail(stderr_to)));
    }
    match serde_json::from_str(spawned.stdout.trim())
        .map_err(|e| format!("the child's reply does not parse: {e}"))?
    {
        ChildReply::Ok(out) => Ok(out),
        ChildReply::Err(e) => Err(e),
    }
}

fn child_rep(
    exe: &Path,
    c: Contender,
    cell: ScanCell,
    tool: Option<(&Rival, &Path)>,
) -> Result<RepResult, String> {
    let scratch = Scratch::new(None).map_err(|e| format!("no scratch directory: {e}"))?;
    let started_under = wait_for_quiet();
    let out_dir = scratch.path().join("out");
    let mut args = vec![c.label().to_string(), cell.spec(), out_dir.display().to_string()];
    if let Some((rival, input)) = tool {
        args.push(rival.binary.display().to_string());
        args.push(input.display().to_string());
    }
    let ran = run_child_process(exe, &args, &scratch.path().join("stderr.txt"));
    let load = worse(started_under, MachineLoad::sample());
    let out = ran.map_err(|e| format!("{} {}: {e}", c.label(), cell.spec()))?;
    attest(c, cell, &out_dir, &out).map_err(|e| format!("attest {} {}: {e}", c.label(), cell.spec()))?;
    Ok(RepResult { out, load })
}

fn rows(
    c: Contender,
    cell: ScanCell,
    acc: &Accum,
    reps: u32,
    timer: Option<stats::TimerProbe>,
    skipped: Option<&str>,
) -> Vec<DocumentCell> {
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
            peak_rss_mb: acc
                .reps
                .iter()
                .filter_map(|r| r.out.peak_rss_bytes)
                .max()
                .map(|b| b as f64 / (1024.0 * 1024.0)),
            heap_peak_bytes: acc.reps.iter().filter_map(|r| r.out.heap_peak).max(),
            ..InvariantBlock::default()
        }
    };
    let load = worst_load(acc.reps.iter().map(|r| r.load));
    let series = |f: &dyn Fn(&RepResult) -> Option<f64>| -> Vec<f64> { acc.reps.iter().filter_map(f).collect() };
    let mut metrics = vec![
        (WALL, series(&|r| r.out.build_ms)),
        (
            THROUGHPUT,
            series(&|r| r.out.build_ms.map(|ms| cell.tiles() as f64 / (ms / 1000.0))),
        ),
        (PEAK_RSS, series(&|r| r.out.peak_rss_bytes.map(|b| b as f64))),
        (OUTPUT_BYTES, series(&|r| r.out.output_bytes.map(|b| b as f64))),
    ];
    if matches!(c, Contender::Pmtiles | Contender::Pvec) {
        metrics.insert(3, (HEAP_PEAK, series(&|r| r.out.heap_peak.map(|b| b as f64))));
    }
    metrics
        .into_iter()
        .map(|(metric, samples)| {
            let mut row = DocumentCell::from_report(CellReport {
                labels: CellLabels {
                    backend: c.label().to_string(),
                    scale: cell.tiles() as u32,
                    source: c.source_label().to_string(),
                    cell: cell.spec(),
                },
                scenario: SCENARIO,
                metric,
                isolation: Isolation::ProcessPerRep,
                oversubscribed: None,
                warmup: None,
                discarded_warmup: Vec::new(),
                reps_declared: reps,
                min_reps: reps,
                samples,
                outcome,
                reason: reason.clone(),
                invariants: invariants.clone(),
                machine_load: load,
                timer,
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

/// The cells a profile walks.
#[must_use]
pub fn cells_for(profile: Profile) -> Vec<ScanCell> {
    match profile {
        Profile::Ci => vec![ScanCell::ci()],
        Profile::Full | Profile::Xl => vec![ScanCell::a0()],
    }
}

/// Run a whole sweep and build the document.
#[must_use]
pub fn run_sweep(profile: Profile) -> Document {
    let exe = crate::harness::current_exe();
    let starting_load = MachineLoad::sample();
    let started = now_iso();
    let reps = super::document::generate_reps(profile);
    let measurement = Measurement::probed_from(MeasurementSpec {
        unit: "fresh-process-per-cell",
        isolation: "subprocess-per-cell",
        reps: Reps::of(&[("generate", reps)]),
        min_reps: Reps::of(&[("generate", reps)]),
        seed: SEED,
        warmup: None,
        page_cache: "warm-unknown",
    });
    let mut doc = Document::new_for(FAMILY, RUNNER, profile.label(), started, measurement);
    doc.starting_load = Some(starting_load);
    let timer = Some(stats::probe_timer());
    let rivals: Vec<(Contender, Result<Rival, String>)> = [Contender::GdalCog, Contender::VipsZip]
        .into_iter()
        .map(|c| (c, Rival::locate(c)))
        .collect();

    for cell in cells_for(profile) {
        eprintln!("pvec-sink: cell {}", cell.spec());
        // The scan the tools read, written once and untimed by a child.
        let input_dir = Scratch::new(None).ok();
        let input: Result<PathBuf, String> = match (&input_dir, rivals.iter().any(|(_, r)| r.is_ok())) {
            (_, false) => Err("no external tool is available to read a scan".to_string()),
            (None, _) => Err("no scratch directory for the scan".to_string()),
            (Some(dir), true) => {
                let args = vec!["input".to_string(), cell.spec(), dir.path().display().to_string()];
                run_child_process(&exe, &args, &dir.path().join("stderr.txt")).map(|_| dir.path().join(INPUT_NAME))
            }
        };
        let mut accs: Vec<Accum> = Contender::ALL.iter().map(|_| Accum::default()).collect();
        for rep in 0..reps as usize {
            for k in 0..Contender::ALL.len() {
                // The starting contender rotates, so none is always the one after a hot or cold pass.
                let at = (k + rep) % Contender::ALL.len();
                let c = Contender::ALL[at];
                let result = match c {
                    Contender::Pmtiles | Contender::Pvec => child_rep(&exe, c, cell, None),
                    _ => {
                        let rival = rivals.iter().find(|(r, _)| *r == c).map(|(_, r)| r);
                        match (rival, &input) {
                            (Some(Ok(rival)), Ok(input)) => child_rep(&exe, c, cell, Some((rival, input))),
                            _ => continue,
                        }
                    }
                };
                match result {
                    Ok(r) => accs[at].reps.push(r),
                    Err(e) => accs[at].failures.push(e),
                }
            }
        }
        for (at, c) in Contender::ALL.iter().enumerate() {
            let skipped = rivals
                .iter()
                .find(|(r, _)| r == c)
                .and_then(|(_, r)| r.as_ref().err().cloned())
                .or_else(|| {
                    (matches!(c, Contender::GdalCog | Contender::VipsZip))
                        .then(|| input.as_ref().err().cloned())
                        .flatten()
                });
            for row in rows(*c, cell, &accs[at], reps, timer, skipped.as_deref()) {
                doc.push(row);
            }
        }
    }

    doc.rebuild_invariant_table();
    doc.finished_at = Some(now_iso());
    doc.provenance = Some(provenance(profile, &doc, &rivals));
    doc.stamp_dirty_from_provenance();
    doc.stamp_run_id();
    doc
}

fn provenance(
    profile: Profile,
    doc: &Document,
    rivals: &[(Contender, Result<Rival, String>)],
) -> serde_json::Value {
    let scratch = crate::family::Family::Storage.scratch_root();
    let provenance = crate::provenance::Provenance::capture_for_document(&scratch);
    for warning in provenance.document_provenance_warnings() {
        eprintln!("{warning}");
    }
    let external: serde_json::Map<String, serde_json::Value> = rivals
        .iter()
        .map(|(c, r)| {
            let v = match r {
                Ok(r) => serde_json::json!({
                    "version": r.version,
                    "binary": r.binary.display().to_string(),
                    "sha256": r.sha256,
                }),
                Err(why) => serde_json::json!({ "unavailable": why }),
            };
            (c.label().to_string(), v)
        })
        .collect();
    provenance.to_document_block(
        &serde_json::json!({
            "argv": std::env::args().collect::<Vec<_>>(),
            "command": "pvec-sink",
            "cwd": std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default(),
            "env": {
                "STORAGE_ALLOW_DIRTY": std::env::var("STORAGE_ALLOW_DIRTY").ok(),
                "RUSTFLAGS": std::env::var("RUSTFLAGS").ok(),
                "BENCH_DAEMON_ARCH": std::env::var("BENCH_DAEMON_ARCH").ok(),
                "TMPDIR": std::env::var("TMPDIR").ok(),
                GDAL_ENV: std::env::var(GDAL_ENV).ok(),
                VIPS_ENV: std::env::var(VIPS_ENV).ok(),
            },
            "resolved": {
                "profile": profile.label(),
                "reps": doc.measurement.reps,
                "scenarios": [SCENARIO],
                "cells": cells_for(profile).iter().map(ScanCell::spec).collect::<Vec<_>>(),
                "contenders": Contender::ALL.iter().map(|c| c.label()).collect::<Vec<_>>(),
                "tileEdge": TILE_EDGE,
                "budgetBytes": BUDGET_BYTES,
                "externalTools": external,
                "quietBelowContentionPerCore": env_f64(super::writer_sweep::QUIET_BELOW_ENV, 0.5),
                "notRun": [{
                    "arch": "arm64",
                    "reason": "no arm64 host with a usable docker was available for this run (mercury has \
                               no docker); the x86_64 numbers stand alone and are not extrapolated",
                }],
            },
        }),
        std::env::var("STORAGE_ALLOW_DIRTY").is_ok(),
    )
}
