//! The `storage` family: PMTiles against a directory tree, with repetitions.
//!
//! # What this replaces, and why
//!
//! The PMTiles harness in the engine repository measures one process per cell,
//! once. A cell carries a single value copied into a field called `median`, no
//! warm-up, no dispersion, and no way to tell a real move from the noise
//! floor. Both of its sweep profiles walk `2048x2048@256`, so the committed
//! exports contain a free replicate pair: the same cell, twice, on an idle
//! host, running identical code. Across that pair p99 moves 74.5%, wall 10.7%
//! and p50 7.1%. Nothing in that suite can see it.
//!
//! So every cell here carries `samples[]` and every summary statistic is
//! computed from them, a pass scenario throws a warm-up away and says so, and
//! a sample set too small for a 99th percentile publishes a maximum under the
//! name `max`.
//!
//! # The isolation, and the bug it closes
//!
//! `libviprs-bench` already owns child-per-cell isolation with `wait4`
//! `ru_maxrss`, and this family runs on it. One child per
//! `(backend, cell, scenario)`, and for the fresh-process scenarios one child
//! per repetition on top of that.
//!
//! That is not tidiness. In the harness this replaces, every read scenario for
//! a cell ran in one process against one reader: `read_cold` opened readers,
//! `read_warm` warmed one, `read_sequential` walked it, and then `read_random`
//! walked the reader `read_sequential` had just warmed and published the
//! result as a random read. On a leaf-bearing archive that is the difference
//! between a lookup that fetches a directory and a lookup that does not.
//! Nothing here hands a scenario a reader: a scenario gets a
//! [`scenarios::ReaderFactory`] and asks it for one that has never served a
//! lookup.
//!
//! # What an archived run has to prove about itself
//!
//! A number is only worth keeping if the document carrying it can say what
//! produced it. [`integrity`] canonicalises a document and digests it four ways,
//! [`attest`] decides whether a cell's declared regime is the one the archive is
//! actually in, and [`archive`] refuses a run it cannot vouch for rather than
//! averaging it in. The invariants below are what those digests are taken over.
//!
//! # This family does not need libvips
//!
//! Nothing in it touches the comparison, so it builds and runs without the
//! `libvips` feature, which is what lets it live in the cheap check job and in
//! a Docker stage that skips the source build entirely.

/// What refuses a run rather than averaging it in, and what an archived run is
/// filed under (issue #66).
pub mod archive;
/// Whether the thing a cell says it measured is the thing that ran, observed
/// from the archive rather than read off the cell's own label (issue #66).
pub mod attest;
pub mod cells;
pub mod document;
/// Live heap bytes, counted by a global allocator a binary opts into
/// (libviprs#1136).
pub mod heap;
/// Canonical JSON and the four digests a sealed document carries (issue #66).
pub mod integrity;
/// The two declared cost models (issue #67). Not measurements, and never
/// charted on an axis carrying one.
pub mod model;
pub mod scenarios;
pub mod stats;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

// One SHA-256 for the crate. This module and `crate::sha256` each grew a thin
// `sha2` wrapper in the same week; they are the same hash and now the same code,
// and `sha256.rs` is the one that keeps the known-answer vectors.
use crate::sha256::Sha256Stream;

use libviprs::checksum::{ChecksumAlgo, hash_tile};
use libviprs::planner::{PyramidPlan, TileCoord};
use libviprs::pyramid_reader::{DirectoryPyramidReader, PmTilesPyramidReader, PyramidReader};
use libviprs::sink::TileFormat;
use libviprs::sink_pmtiles::PmTilesSink;
use libviprs::{EngineBuilder, FsSink, PixelFormat, Raster};

use cells::{Backend, Cell, Profile, Regime, SEED, Source};
use document::{CellLabels, CellReport, Document, DocumentCell, InvariantBlock, MachineLoad};
use scenarios::{
    Coordinates, Invariants, Isolation, MetricSpec, Outcome, ReaderFactory, RepFacts, Scenario,
    ScenarioContext, ScenarioRun, Skip, TileReader, Unit,
};

/// The argv subcommand a parent uses to re-invoke itself as one scenario.
pub const SINGLE_FLAG: &str = "--storage-single";

/// The file a sweep writes, inside the family's own report directory.
pub const DOCUMENT_NAME: &str = "storage-results.json";

/// Where a sweep's document lands under a report root.
///
/// Derived from [`Family::report_dir`](crate::family::Family::report_dir) and
/// not spelled out, so the storage family cannot drift out of the
/// `report/<family>/` layout every other family follows. Two families writing
/// into one directory can overwrite each other's charts and append to each
/// other's history, and the JS renderer takes a `--report-dir` and nothing
/// else, so a document outside a family directory is one no chart will draw.
pub fn default_output_path(report_root: &Path) -> PathBuf {
    crate::family::Family::Storage
        .report_dir(report_root)
        .join(DOCUMENT_NAME)
}

/// Every scenario a sweep can walk, in order.
///
/// The reference set (`generate`, `read_plan_order`, `read_tileid_order`,
/// `read_random`) plus K1.4's: `open`, `first_lookup`, `decode_root`, the
/// `read_concurrent@T` ladder and `requests`.
///
/// Which of them a given sweep walks is [`Profile::scenario_names`], not this
/// list. `ci` leaves out the thread ladder and says so there rather than
/// skipping quietly at run time.
///
/// `tests/storage_registry.rs` asserts this against the family's declared list
/// and against a real `ci` document, because the failure this had for one whole
/// wave was that seven scenarios existed, were tested, were merged, and were
/// never in here.
pub fn registry() -> Vec<Box<dyn Scenario>> {
    let mut out = scenarios::reference::all();
    out.extend(scenarios::all());
    out
}

/// Look one up by the name it publishes.
pub fn scenario_named(name: &str) -> Option<Box<dyn Scenario>> {
    registry().into_iter().find(|s| s.name() == name)
}

// ---------------------------------------------------------------------------
// Rasters
// ---------------------------------------------------------------------------

/// The raster behind a cell.
///
/// `Gradient` is the deterministic one the old harness used, kept byte for
/// byte so a cell measured here and a cell measured there are the same work.
/// `Noise` is seeded and incompressible, which is the archive's worst copy
/// case and the tree's largest files. `Flat` is a dedupe guard and never
/// produces a published row.
pub fn raster(source: Source, width: u32, height: u32) -> Raster {
    let mut data = vec![0u8; width as usize * height as usize * 3];
    match source {
        Source::Gradient => {
            for y in 0..height {
                for x in 0..width {
                    let off = (y as usize * width as usize + x as usize) * 3;
                    data[off] = (x % 251) as u8;
                    data[off + 1] = (y % 241) as u8;
                    data[off + 2] = ((x * 7 + y * 13) % 239) as u8;
                }
            }
        }
        Source::Noise => {
            let mut rng = stats::Splitmix::new(SEED);
            let mut chunk = rng.next_u64().to_le_bytes();
            let mut taken = 0usize;
            for byte in data.iter_mut() {
                if taken == chunk.len() {
                    chunk = rng.next_u64().to_le_bytes();
                    taken = 0;
                }
                *byte = chunk[taken];
                taken += 1;
            }
        }
        Source::PeriodicGradient => {
            // The same ramp with its moduli rounded to 256, so it repeats every
            // 256 pixels on both axes. It exists as the positive control for
            // `cells::collapses_at`: a guard that refuses a source whose period
            // divides the tile size has to be shown refusing one, and the
            // gradient above never will.
            //
            // It is not invented for the test. `libviprs_bench::gradient_raster`
            // in `src/lib.rs` is `(x % 256, y % 256, (x * 7 + y * 13) % 256)`
            // and has exactly this shape, which makes this source a measurement
            // of what that costs as well as a control.
            for y in 0..height {
                for x in 0..width {
                    let off = (y as usize * width as usize + x as usize) * 3;
                    data[off] = (x % 256) as u8;
                    data[off + 1] = (y % 256) as u8;
                    data[off + 2] = ((x * 7 + y * 13) % 256) as u8;
                }
            }
        }
        Source::Flat => {}
    }
    Raster::new(width, height, PixelFormat::Rgb8, data).expect("a bench raster is well formed")
}

// ---------------------------------------------------------------------------
// Scratch space
// ---------------------------------------------------------------------------

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The directory under `$TMPDIR` every storage scenario's scratch sits in.
///
/// Public and named for the same reason [`crate::engine_sink_root`] is: the
/// document has to record which filesystem the pyramids were written to, and a
/// provenance block that probed some other directory describes a run nobody
/// made. Until this existed there was no such directory to probe. [`Scratch`]
/// based straight at `$TMPDIR` and the provenance block probed
/// `$TMPDIR/libviprs-storage-provenance`, a name nothing in this crate ever
/// creates, so `statfs` answered ENOENT and the document recorded
/// `fsType: "unknown"` for a mount the `engines` family named `ext4` two
/// minutes later.
pub fn storage_scratch_root() -> PathBuf {
    std::env::temp_dir().join("libviprs-storage")
}

/// A directory this process made and this process removes.
///
/// Every name is unique within the process and is never reused, so two
/// repetitions that report the same scratch path did not regenerate anything,
/// whatever their timings say.
#[derive(Debug)]
pub struct Scratch {
    path: PathBuf,
}

impl Scratch {
    pub fn new(root: Option<&Path>) -> std::io::Result<Scratch> {
        // `storage_scratch_root()` and not `$TMPDIR` directly, so the
        // directory the provenance block probes is an ancestor of every
        // directory the sweep actually writes into rather than a sibling of
        // them in name only.
        let base = match root {
            Some(root) => root.to_path_buf(),
            None => storage_scratch_root(),
        };
        let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = base.join(format!("libviprs-storage-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path)?;
        Ok(Scratch { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Only ever a directory this process created, under the scratch root,
        // carrying this process's own id in its name.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------------------
// Writing a pyramid
// ---------------------------------------------------------------------------

/// What one generation produced.
pub struct Written {
    /// The archive file, or the root of the tree.
    pub output: PathBuf,
    pub wall_ms: f64,
    pub tiles_produced: u64,
    pub tracked_memory_bytes: Option<u64>,
}

/// Write one pyramid into one backend and time it.
pub fn write_pyramid(
    backend: Backend,
    cell: Cell,
    plan: &PyramidPlan,
    into: &Path,
) -> Result<Written, String> {
    let source = raster(cell.source, cell.width, cell.height);
    let started = std::time::Instant::now();
    let (result, output) = match backend {
        Backend::PmTiles => {
            let archive = into.join("pyramid.pmtiles");
            let sink = PmTilesSink::builder(&archive)
                .plan(plan.clone())
                .tile_format(TileFormat::Png)
                .build()
                .map_err(|e| format!("the archive sink does not build: {e}"))?;
            let result = EngineBuilder::new(&source, plan.clone(), sink)
                .run()
                .map_err(|e| format!("the archive run failed: {e}"))?;
            (result, archive)
        }
        Backend::Directory => {
            let root = into.join("tree");
            let sink = FsSink::new(&root, plan.clone()).with_format(TileFormat::Png);
            let result = EngineBuilder::new(&source, plan.clone(), sink)
                .run()
                .map_err(|e| format!("the directory run failed: {e}"))?;
            (result, root)
        }
    };
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(Written {
        output,
        wall_ms,
        tiles_produced: result.tiles_produced,
        tracked_memory_bytes: Some(result.peak_memory_bytes),
    })
}

// ---------------------------------------------------------------------------
// What a pyramid costs, exactly
// ---------------------------------------------------------------------------

/// Bytes, entries, directories and allocated blocks a pyramid occupies.
///
/// A file counts one entry and so does a directory, because the cost this
/// column exists to show is namespace pressure. `None` when the path cannot be
/// walked at all, and never `(0, 0)`: zero entries is a *better* number than
/// the `1` a real archive costs, so a broken measurement used to publish a win
/// on the one column the comparison is about.
pub fn occupancy(path: &Path) -> Option<(u64, u64, u64, u64)> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_dir() {
        return Some((meta.len(), 1, 0, allocated_bytes(&meta)));
    }
    let mut bytes = 0u64;
    let mut entries = 1u64;
    let mut directories = 1u64;
    let mut allocated = allocated_bytes(&meta);
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(listing) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in listing.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            entries += 1;
            allocated += allocated_bytes(&meta);
            if meta.is_dir() {
                directories += 1;
                stack.push(entry.path());
            } else {
                bytes += meta.len();
            }
        }
    }
    Some((bytes, entries, directories, allocated))
}

#[cfg(unix)]
fn allocated_bytes(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.blocks() * 512
}

#[cfg(not(unix))]
fn allocated_bytes(_meta: &std::fs::Metadata) -> u64 {
    0
}

/// A digest of what a backend wrote.
///
/// One sha256 for an archive. For a tree, a sha256 over the sorted
/// `(relative path, sha256)` list, so the digest depends on the tile bytes and
/// on where they landed and on nothing else: not on readdir order, not on
/// mtimes, not on the scratch directory's name.
pub fn artefact_digest(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_dir() {
        return hash_file(path);
    }
    let mut lines: Vec<String> = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let listing = std::fs::read_dir(&dir).ok()?;
        for entry in listing {
            let entry = entry.ok()?;
            let meta = entry.metadata().ok()?;
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                let rel = entry.path().strip_prefix(path).ok()?.to_path_buf();
                lines.push(format!("{}  {}", rel.display(), hash_file(&entry.path())?));
            }
        }
    }
    lines.sort();
    Some(hash_tile(lines.join("\n").as_bytes(), ChecksumAlgo::Sha256))
}

fn hash_file(path: &Path) -> Option<String> {
    // Streamed in chunks rather than slurped: the 16384 canvas writes a 520 MB
    // archive and a benchmark that needed half a gigabyte of resident memory
    // to check an invariant would be measuring itself.
    use std::io::Read as _;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256Stream::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(hasher.finish())
}

/// Attest one cell's artefacts: was the archive each backend names the archive
/// it was measured against, and do the two agree about what a tile contains?
///
/// This is `attest`'s caller, and the whole point of it is that nothing here
/// reads a label. The regime a cell is *supposed* to come out as is predicted
/// from the plan and the writer's own cutoff, the regime it *is* in is read back
/// out of the file, and the two are compared. The byte half opens both backends
/// and asks them for the same coordinates.
///
/// A backend with no archive is not attested, which is the honest answer: the
/// aggregator refuses an `ok` cell that was never observed exactly as it refuses
/// one that was observed and disagreed.
///
/// The directory backend has no directory structure to be in a regime about, so
/// its attestation rests on the byte half alone, and that is said out loud here
/// rather than left for a reader to infer from a `None`.
pub fn attest_artefacts(
    cell: Cell,
    plan: &PyramidPlan,
    profile: Profile,
    artefacts: &[(Backend, Scratch, PathBuf)],
) -> Vec<(Backend, attest::Attestation)> {
    // Predicted, not declared: the writer spills into leaves at
    // `ROOT_ONLY_MAX_ENTRIES` and the comparison is strict, so this is the
    // engine's own rule applied to this cell's plan.
    let entries = cell.planned_tiles().unwrap_or(0) as u64;
    let predicted = if entries < cells::ROOT_ONLY_MAX_ENTRIES {
        Regime::Root
    } else {
        Regime::Leaves
    };
    let equivalence = byte_equivalence(plan, profile, artefacts);

    artefacts
        .iter()
        .map(|(backend, _, path)| {
            let observed = match backend {
                Backend::PmTiles => match archive_shape(*backend, path) {
                    Some((root_entries, leaves)) => attest::ObservedArchive {
                        backend: backend.as_str().to_string(),
                        // The root is reconstructed as the entry kinds `attest`
                        // classifies, from the two counts the reader gives.
                        root_entries: (0..root_entries)
                            .map(|i| {
                                if i < leaves {
                                    attest::RootEntry::LeafPointer
                                } else {
                                    attest::RootEntry::Tile
                                }
                            })
                            .collect(),
                        leaf_directories: leaves,
                        tiles: entries,
                    },
                    None => attest::ObservedArchive {
                        backend: backend.as_str().to_string(),
                        root_entries: Vec::new(),
                        leaf_directories: 0,
                        tiles: 0,
                    },
                },
                // A directory tree has no root directory in the PMTiles sense.
                // Handing `attest` an empty root would read as "nothing was
                // observed", so the shape half is satisfied by construction and
                // the byte half is what decides.
                Backend::Directory => attest::ObservedArchive {
                    backend: backend.as_str().to_string(),
                    root_entries: vec![match predicted {
                        Regime::Root => attest::RootEntry::Tile,
                        Regime::Leaves => attest::RootEntry::LeafPointer,
                    }],
                    leaf_directories: match predicted {
                        Regime::Root => 0,
                        Regime::Leaves => 1,
                    },
                    tiles: entries,
                },
            };
            (*backend, attest::attest(predicted, &observed, &equivalence))
        })
        .collect()
}

/// Read the same coordinates from both backends and compare the bytes.
///
/// Two backends that disagree about what a tile contains are not two
/// measurements of one workload, however clean the timings look. The sample is
/// seeded so it is the same coordinates every run, and the count is whatever the
/// profile reads, floored at `attest`'s own minimum so a thin profile cannot
/// quietly weaken the claim.
fn byte_equivalence(
    plan: &PyramidPlan,
    profile: Profile,
    artefacts: &[(Backend, Scratch, PathBuf)],
) -> attest::EquivalenceSample {
    let coords = coordinate_sets(plan, profile, cells::SEED);
    let wanted = attest::MIN_EQUIVALENCE_SAMPLE as usize;
    let sampled: Vec<_> = coords.random.iter().copied().take(wanted).collect();

    let readers: Vec<_> = artefacts
        .iter()
        .filter_map(|(backend, _, path)| FileReaderFactory::new(*backend, path, plan).fresh().ok())
        .collect();

    // One backend cannot disagree with itself, and claiming agreement from a
    // single reader would be the label-shaped answer this whole module exists to
    // avoid.
    if readers.len() < 2 {
        return attest::EquivalenceSample {
            seed: cells::SEED,
            sampled: 0,
            matched: 0,
        };
    }

    let mut matched = 0u32;
    for coord in &sampled {
        let mut bytes: Vec<Option<Vec<u8>>> = Vec::new();
        for reader in &readers {
            match reader.tile(*coord) {
                Ok(found) => bytes.push(found),
                Err(_) => {
                    bytes.clear();
                    break;
                }
            }
        }
        if bytes.len() == readers.len() && bytes.windows(2).all(|w| w[0] == w[1]) {
            matched += 1;
        }
    }

    attest::EquivalenceSample {
        seed: cells::SEED,
        sampled: sampled.len() as u32,
        matched,
    }
}

/// The archive's directory shape: how many entries the root holds and how many
/// of them point at a leaf.
///
/// `None` for the directory backend, which has no directory to ask.
pub fn archive_shape(backend: Backend, output: &Path) -> Option<(u64, u64)> {
    if backend != Backend::PmTiles {
        return None;
    }
    let reader = PmTilesPyramidReader::try_open(output).ok()?;
    let root = reader.reader().root_entries();
    let leaves = root.iter().filter(|e| e.is_leaf()).count();
    Some((root.len() as u64, leaves as u64))
}

// ---------------------------------------------------------------------------
// Readers
// ---------------------------------------------------------------------------

struct PyramidTileReader(Box<dyn PyramidReader>);

impl TileReader for PyramidTileReader {
    fn tile(&self, coord: TileCoord) -> Result<Option<Vec<u8>>, String> {
        self.0.tile(coord).map_err(|e| e.to_string())
    }
}

/// The reader factory a real sweep uses: one that opens the artefact fresh
/// every time it is asked.
pub struct FileReaderFactory {
    backend: Backend,
    artefact: PathBuf,
    plan: PyramidPlan,
}

impl FileReaderFactory {
    pub fn new(backend: Backend, artefact: &Path, plan: &PyramidPlan) -> FileReaderFactory {
        FileReaderFactory {
            backend,
            artefact: artefact.to_path_buf(),
            plan: plan.clone(),
        }
    }
}

impl ReaderFactory for FileReaderFactory {
    fn fresh(&self) -> Result<Arc<dyn TileReader>, String> {
        let inner: Box<dyn PyramidReader> = match self.backend {
            Backend::PmTiles => Box::new(
                PmTilesPyramidReader::try_open(&self.artefact)
                    .map_err(|e| format!("the archive does not open: {e}"))?,
            ),
            Backend::Directory => Box::new(
                DirectoryPyramidReader::try_open(
                    &self.artefact,
                    self.plan.clone(),
                    TileFormat::Png,
                )
                .map_err(|e| format!("the tree does not open: {e}"))?,
            ),
        };
        Ok(Arc::new(PyramidTileReader(inner)))
    }
}

/// A factory that refuses, for scenarios that never open a reader.
pub struct NoReaders;

impl ReaderFactory for NoReaders {
    fn fresh(&self) -> Result<Arc<dyn TileReader>, String> {
        Err("this scenario has no artefact to read".to_string())
    }
}

// ---------------------------------------------------------------------------
// Coordinates
// ---------------------------------------------------------------------------

/// Build the coordinate sets one cell's read scenarios walk.
///
/// The same `N` coordinates in three orders, so a difference between the three
/// rows is about order and nothing else. `N` is the profile's cap or the plan
/// length, whichever is smaller.
pub fn coordinate_sets(plan: &PyramidPlan, profile: Profile, seed: u64) -> Coordinates {
    let n = profile.read_samples().min(cells::coordinates(plan).len());
    // Both orders come from the scenario modules that publish them, so there is
    // one sort in the family rather than one here and another there.
    let plan_order = scenarios::plan_order::coordinates(plan, n);
    let tileid_order = scenarios::tileid_order::coordinates(plan, n);

    // A seeded Fisher-Yates over the same set, so the random row addresses the
    // same tiles as the ordered rows and only the order differs.
    let mut random = plan_order.clone();
    let mut rng = stats::Splitmix::new(seed);
    for i in (1..random.len()).rev() {
        let j = rng.below(i + 1);
        random.swap(i, j);
    }

    Coordinates {
        root_addressed: plan_order.first().copied(),
        leaf_addressed: None,
        plan_order,
        tileid_order,
        random,
    }
}

// ---------------------------------------------------------------------------
// The wire between a parent and its child
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireSeries {
    pub metric: String,
    pub unit: String,
    pub direction: String,
    pub samples: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireRun {
    pub outcome: String,
    pub reason: Option<String>,
    pub series: Vec<WireSeries>,
    pub reps: Vec<RepFacts>,
    #[serde(rename = "discardedWarmup")]
    pub discarded_warmup: Vec<f64>,
    #[serde(rename = "peakRssBytes")]
    pub peak_rss_bytes: Option<u64>,
    #[serde(rename = "heapPeakBytes")]
    pub heap_peak_bytes: Option<u64>,
}

impl WireRun {
    fn from_run(run: &ScenarioRun) -> WireRun {
        WireRun {
            outcome: Outcome::Ok.as_str().to_string(),
            reason: None,
            series: run
                .series
                .iter()
                .map(|s| WireSeries {
                    metric: s.metric.name.to_string(),
                    unit: s.metric.unit.as_str().to_string(),
                    direction: s.metric.direction.as_str().to_string(),
                    samples: s.samples.clone(),
                })
                .collect(),
            reps: run.reps.clone(),
            discarded_warmup: run.discarded_warmup.clone(),
            peak_rss_bytes: run.peak_rss_bytes,
            heap_peak_bytes: run.heap_peak_bytes,
        }
    }

    fn from_skip(skip: &Skip) -> WireRun {
        WireRun {
            outcome: skip.outcome.as_str().to_string(),
            reason: Some(skip.reason.clone()),
            series: Vec::new(),
            reps: Vec::new(),
            discarded_warmup: Vec::new(),
            peak_rss_bytes: None,
            heap_peak_bytes: None,
        }
    }
}

/// The child body: run one scenario against one `(backend, cell)` and print
/// one [`WireRun`] as JSON on stdout.
///
/// `argv` after the flag is `<backend> <cell spec> <scenario> <reps>` and then
/// an optional artefact path.
pub fn run_child(args: &[String]) -> i32 {
    let [backend, spec, scenario_name, reps, rest @ ..] = args else {
        eprintln!("{SINGLE_FLAG} <backend> <cell> <scenario> <reps> [artefact]");
        return 2;
    };
    let Some(backend) = Backend::parse(backend) else {
        eprintln!("unknown backend {backend:?}");
        return 2;
    };
    let Some(cell) = Cell::parse(spec) else {
        eprintln!("unparseable cell {spec:?}");
        return 2;
    };
    let Some(scenario) = scenario_named(scenario_name) else {
        eprintln!("unknown scenario {scenario_name:?}");
        return 2;
    };
    let reps: u32 = reps.parse().unwrap_or(1);
    let artefact = rest.first().map(PathBuf::from);
    let profile = Profile::from_env();

    let plan = cell.plan().expect("a cell plans");
    let coords = coordinate_sets(&plan, profile, SEED);

    let factory: Box<dyn ReaderFactory> = match artefact.as_deref() {
        Some(path) => Box::new(FileReaderFactory::new(backend, path, &plan)),
        None => Box::new(NoReaders),
    };

    let ctx = ScenarioContext {
        backend,
        cell,
        profile,
        seed: SEED,
        scratch_root: None,
        artefact: artefact.as_deref(),
        coords: &coords,
        readers: factory.as_ref(),
    };

    let wire = match scenario.run(&ctx, reps) {
        Ok(run) => WireRun::from_run(&run),
        Err(skip) => WireRun::from_skip(&skip),
    };
    println!(
        "{}",
        serde_json::to_string(&wire).expect("a wire run serialises")
    );
    0
}

/// Dispatch [`SINGLE_FLAG`] out of `argv`, the way the engine harness
/// dispatches `--single`.
pub fn maybe_run_single_subcommand() -> Option<i32> {
    let argv: Vec<String> = std::env::args().collect();
    let at = argv.iter().position(|a| a == SINGLE_FLAG)?;
    Some(run_child(&argv[at + 1..]))
}

// ---------------------------------------------------------------------------
// The parent
// ---------------------------------------------------------------------------

/// Spawn one scenario child and read its run back, taking the child's own
/// `ru_maxrss` through `wait4` rather than trusting its self-report.
pub fn spawn_scenario(
    exe: &Path,
    backend: Backend,
    cell: Cell,
    scenario: &str,
    reps: u32,
    artefact: Option<&Path>,
) -> Result<WireRun, String> {
    let mut command = Command::new(exe);
    command
        .arg(SINGLE_FLAG)
        .arg(backend.as_str())
        .arg(cell.spec())
        .arg(scenario)
        .arg(reps.to_string());
    if let Some(path) = artefact {
        command.arg(path);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("the scenario child does not spawn: {e}"))?;
    let pid = child.id() as i32;
    let mut out = String::new();
    child
        .stdout
        .take()
        .ok_or_else(|| "the child has no stdout".to_string())?
        .read_to_string(&mut out)
        .map_err(|e| format!("the child's stdout does not read: {e}"))?;
    let rss = wait4_maxrss(pid);
    let mut wire: WireRun = serde_json::from_str(out.trim())
        .map_err(|e| format!("the child's output does not parse: {e}"))?;
    if let Some(rss) = rss
        && rss > 0
    {
        wire.peak_rss_bytes = Some(rss);
    }
    Ok(wire)
}

/// `wait4` a pid and normalise `ru_maxrss` to bytes.
fn wait4_maxrss(pid: i32) -> Option<u64> {
    use std::mem::MaybeUninit;
    let mut status: libc::c_int = 0;
    let mut ru = MaybeUninit::<libc::rusage>::uninit();
    let ret = unsafe { libc::wait4(pid, &mut status, 0, ru.as_mut_ptr()) };
    if ret <= 0 {
        return None;
    }
    let ru = unsafe { ru.assume_init() };
    let maxrss = ru.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        Some(maxrss)
    } else {
        Some(maxrss * 1024)
    }
}

/// The invariants every repetition of a scenario agreed on.
///
/// An invariant that differs between two repetitions of one commit is a
/// defect, never noise, so a disagreement is reported rather than averaged:
/// the field goes `None` and the reason says which one moved.
pub fn agreed(reps: &[RepFacts]) -> (Invariants, Vec<String>) {
    let mut out = Invariants::default();
    let mut disagreements = Vec::new();
    macro_rules! agree {
        ($field:ident) => {{
            let first = reps.first().map(|r| r.invariants.$field.clone()).flatten();
            let all_equal = reps
                .iter()
                .all(|r| r.invariants.$field.clone() == first.clone());
            if all_equal {
                out.$field = first;
            } else {
                disagreements
                    .push(concat!(stringify!($field), " differs between reps").to_string());
            }
        }};
    }
    agree!(output_bytes);
    agree!(allocated_bytes);
    agree!(filesystem_entries);
    agree!(directories);
    agree!(tiles_produced);
    agree!(artefact_digest);
    agree!(root_entries);
    agree!(leaves);
    agree!(requests);
    agree!(request_bytes);
    (out, disagreements)
}

/// Turn one child's run into the document rows it earned: one row per series.
pub fn rows_from_wire(
    backend: Backend,
    cell: Cell,
    scenario: &dyn Scenario,
    profile: Profile,
    wire: &WireRun,
    load: MachineLoad,
    timer: Option<stats::TimerProbe>,
) -> Vec<DocumentCell> {
    let (invariants, disagreements) = agreed(&wire.reps);
    let mut block = InvariantBlock::from(&invariants);
    // Published for both isolations. It used to be filtered to
    // `ProcessPerRep`, which silently dropped it from every read row, and the
    // read rows are where a leaf cache would show. It is a real measurement
    // either way: `wait4` gives the child's own `ru_maxrss` whichever isolation
    // spawned it. What it *means* differs, and that is a caption rather than a
    // reason to discard it. Under `ProcessPerRep` it is the largest single
    // repetition, taken as a max across the children. Under
    // `ProcessPerScenario` it is one child's high-water mark across its warm-up
    // and every repetition, so it is the scenario's peak rather than a
    // repetition's, which is the right number for a capacity question and the
    // wrong one for a per-repetition dispersion. It is a scalar on the cell and
    // never a series, so nothing downstream can mistake it for the latter.
    block.peak_rss_mb = wire.peak_rss_bytes.map(|b| b as f64 / (1024.0 * 1024.0));
    block.heap_peak_bytes = wire.heap_peak_bytes;

    let outcome = if wire.outcome == "ok" && disagreements.is_empty() {
        Outcome::Ok
    } else if wire.outcome == "ok" {
        Outcome::Refused
    } else {
        Outcome::Failed
    };
    let reason = match (&wire.reason, disagreements.is_empty()) {
        (Some(r), _) => Some(r.clone()),
        (None, false) => Some(disagreements.join("; ")),
        _ => None,
    };

    let mut rows = Vec::new();
    if wire.series.is_empty() {
        rows.push(DocumentCell::from_report(CellReport {
            labels: CellLabels::storage(backend, cell),
            scenario: &scenario.name(),
            metric: scenario.primary(),
            isolation: scenario.isolation(),
            oversubscribed: scenario.oversubscribed(host_ncpu()),
            warmup: scenario.warmup(),
            discarded_warmup: wire.discarded_warmup.clone(),
            reps_declared: scenario.reps(profile),
            min_reps: scenario.min_reps(profile),
            samples: Vec::new(),
            outcome,
            reason,
            invariants: block,
            machine_load: load,
            timer,
        }));
        return rows;
    }
    for series in &wire.series {
        rows.push(DocumentCell::from_report(CellReport {
            labels: CellLabels::storage(backend, cell),
            scenario: &scenario.name(),
            metric: MetricSpec {
                name: leak(&series.metric),
                unit: parse_unit(&series.unit),
                direction: parse_direction(&series.direction),
            },
            isolation: scenario.isolation(),
            oversubscribed: scenario.oversubscribed(host_ncpu()),
            warmup: scenario.warmup(),
            discarded_warmup: wire.discarded_warmup.clone(),
            reps_declared: scenario.reps(profile),
            min_reps: scenario.min_reps(profile),
            samples: series.samples.clone(),
            outcome,
            reason: reason.clone(),
            invariants: block.clone(),
            machine_load: load,
            timer,
        }));
    }
    rows
}

/// How many cores the sweep is running on.
///
/// Read in the parent, not in the child: the two are the same machine, and the
/// parent is where the row is built. `1` when the platform will not say, which
/// makes every rung above the first oversubscribed and is the honest reading of
/// "this host cannot tell me how many cores it has".
fn host_ncpu() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// The metric name a child chose, which the parent did not know statically.
fn leak(name: &str) -> &'static str {
    Box::leak(name.to_string().into_boxed_str())
}

fn parse_unit(s: &str) -> Unit {
    match s {
        "ms" => Unit::Milliseconds,
        "us" => Unit::Microseconds,
        "1/s" => Unit::PerSecond,
        "MB" => Unit::Megabytes,
        "1/s/MB" => Unit::PerSecondPerMegabyte,
        "MB*s/tile" => Unit::MegabyteSecondsPerTile,
        "bytes" => Unit::Bytes,
        "ratio" => Unit::Ratio,
        _ => Unit::Count,
    }
}

fn parse_direction(s: &str) -> scenarios::Direction {
    match s {
        "higher-is-better" => scenarios::Direction::HigherIsBetter,
        _ => scenarios::Direction::LowerIsBetter,
    }
}

/// Run a whole sweep and build the document.
///
/// The backends alternate inside each cell, so slow drift over the cell's
/// wall-clock window hits both roughly equally instead of penalising whichever
/// ran last. For a scenario whose repetitions each own a process the
/// alternation is per repetition; for one that owns a single process it is per
/// scenario, which is the finest grain that isolation allows.
pub fn run_sweep(profile: Profile) -> Document {
    let exe = crate::harness::current_exe();
    // Before anything else, and it has to stay before anything else. This is
    // the only reading in the whole document that is other people's work by
    // construction: this process has measured nothing and spawned nothing yet,
    // so whatever the machine is carrying at this instant, it is not carrying
    // it for us. Move this line below the loop and it becomes a reading of our
    // own sweep, which is exactly the thing the per-cell loads already are and
    // exactly why they cannot gate a publish (#100).
    let starting_load = MachineLoad::sample();
    let started = now_iso();
    let mut doc = Document::new(profile, started);
    doc.starting_load = Some(starting_load);
    let timer = Some(stats::probe_timer());

    for cell in profile.cells() {
        let Some(plan) = cell.plan() else { continue };
        // One artefact per backend for the whole cell, generated untimed. The
        // read scenarios measure reading, not writing, and writing it once per
        // scenario would cost more than every timed pass in the sweep.
        let mut artefacts: Vec<(Backend, Scratch, PathBuf)> = Vec::new();
        for backend in Backend::ALL {
            let Ok(scratch) = Scratch::new(None) else {
                continue;
            };
            match write_pyramid(backend, cell, &plan, scratch.path()) {
                Ok(written) => artefacts.push((backend, scratch, written.output)),
                Err(e) => eprintln!("storage: {} {}: {e}", backend.as_str(), cell.spec()),
            }
        }

        // Observed once per cell, because it is a property of the artefact
        // rather than of each scenario that reads it.
        let attested: Vec<(Backend, attest::Attestation)> =
            attest_artefacts(cell, &plan, profile, &artefacts);
        for (backend, verdict) in &attested {
            for reason in verdict.reasons() {
                eprintln!(
                    "storage: {} {} is not attested: {reason}",
                    backend.as_str(),
                    cell.spec()
                );
            }
        }

        let walked: Vec<Box<dyn Scenario>> = {
            let names = profile.scenario_names();
            registry()
                .into_iter()
                .filter(|s| names.contains(&s.name()))
                .collect()
        };
        for (index, scenario) in walked.iter().enumerate() {
            let reps = scenario.reps(profile);
            // Alternate which backend leads, scenario by scenario.
            let order: Vec<Backend> = if index % 2 == 0 {
                Backend::ALL.to_vec()
            } else {
                let mut v = Backend::ALL.to_vec();
                v.reverse();
                v
            };
            let load = MachineLoad::sample();
            for backend in order {
                let artefact = artefacts
                    .iter()
                    .find(|(b, _, _)| *b == backend)
                    .map(|(_, _, path)| path.clone());
                if scenario.needs_artefact() && artefact.is_none() {
                    continue;
                }
                let wire = match scenario.isolation() {
                    Isolation::ProcessPerScenario => spawn_scenario(
                        &exe,
                        backend,
                        cell,
                        &scenario.name(),
                        reps,
                        artefact.as_deref(),
                    ),
                    Isolation::ProcessPerRep => {
                        let mut merged: Option<WireRun> = None;
                        for _ in 0..reps.max(1) {
                            match spawn_scenario(
                                &exe,
                                backend,
                                cell,
                                &scenario.name(),
                                1,
                                artefact.as_deref(),
                            ) {
                                Ok(one) => merged = Some(merge(merged, one)),
                                Err(e) => eprintln!("storage: {e}"),
                            }
                        }
                        merged.ok_or_else(|| "no repetition produced a sample".to_string())
                    }
                };
                match wire {
                    Ok(wire) => {
                        let verdict = attested
                            .iter()
                            .find(|(b, _)| *b == backend)
                            .map(|(_, v)| v.is_attested());
                        for mut row in rows_from_wire(
                            backend,
                            cell,
                            scenario.as_ref(),
                            profile,
                            &wire,
                            load,
                            timer,
                        ) {
                            // From the observation, never from the cell. A
                            // constant `true` here is the exact failure
                            // `attest` exists to prevent.
                            row.attested = verdict;
                            doc.push(row);
                        }
                    }
                    Err(e) => eprintln!("storage: {} {}: {e}", backend.as_str(), cell.spec()),
                }
            }
        }
    }

    doc.rebuild_invariant_table();
    // The two blocks a sweep has to fill itself, because nothing downstream can
    // reconstruct them: the in-run noise floor, and what a remote store would
    // charge. Both live in K1.4's modules and both are one call, so K1.3's
    // provenance and attestation work lands beside them rather than on top.
    doc.modelled = model::entries_for(&doc);
    doc.replicate = scenarios::replicate::block_for(&doc, profile);
    doc.finished_at = Some(now_iso());
    doc.provenance = Some(sweep_provenance(profile, &doc));
    // The dirt travels with every number or the run is refused for the rule
    // `--allow-dirty` exists to satisfy. Nothing filled this until #75.
    doc.stamp_dirty_from_provenance();
    // After the provenance, because the id is derived from it. The aggregator
    // derives the same id from the same fields when it files the run; the
    // document carrying it means a reader who never runs the aggregator can
    // still name the run (#75).
    doc.stamp_run_id();
    doc
}

/// The `provenance` block, filled from the environment the sweep actually ran
/// in.
///
/// `Document::new` leaves this `None` and the aggregator refuses a document
/// without it, which is the correct refusal and was, until this existed, one the
/// producer earned on every run. Everything in the block is observed:
/// `capture_for_document` probes emulation, the scratch filesystem, the cgroup
/// ceilings and the resolved dependency graph, and `build.rs` stamped the two
/// trees' commits at compile time.
///
/// `resolved` is the half only the driver knows: what the profile's defaults
/// expanded to. The aggregator refuses a document whose `resolved.scenarios`
/// still says `"all"`, so the names are written out.
fn sweep_provenance(profile: Profile, doc: &Document) -> serde_json::Value {
    // `Family::scratch_root` rather than a path spelled out here: it is the one
    // place that maps a family to the directory that family writes into, and it
    // makes the directory before anything asks what it is on. Both halves are
    // load-bearing, and the second one is what was missing.
    let scratch = crate::family::Family::Storage.scratch_root();
    let provenance = crate::provenance::Provenance::capture_for_document(&scratch);
    for warning in provenance.document_provenance_warnings() {
        eprintln!("{warning}");
    }
    let allow_dirty = std::env::var("STORAGE_ALLOW_DIRTY").is_ok();
    let scenarios: Vec<String> = profile
        .scenario_names()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let scales: Vec<u64> = {
        let mut seen: Vec<u64> = profile
            .cells()
            .iter()
            .filter_map(|c| c.planned_tiles())
            .map(|t| t as u64)
            .collect();
        seen.sort_unstable();
        seen.dedup();
        seen
    };
    provenance.to_document_block(
        &serde_json::json!({
            "argv": std::env::args().collect::<Vec<_>>(),
            "command": "storage",
            "cwd": std::env::current_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
            "env": {
                "STORAGE_ALLOW_DIRTY": std::env::var("STORAGE_ALLOW_DIRTY").ok(),
                "RUSTFLAGS": std::env::var("RUSTFLAGS").ok(),
                "BENCH_DAEMON_ARCH": std::env::var("BENCH_DAEMON_ARCH").ok(),
                "TMPDIR": std::env::var("TMPDIR").ok(),
            },
            "resolved": {
                "profile": profile.label(),
                "reps": doc.measurement.reps,
                "scenarios": scenarios,
                "scales": scales,
            },
        }),
        allow_dirty,
    )
}

/// Fold one more per-repetition child into the run so far.
pub fn merge(into: Option<WireRun>, one: WireRun) -> WireRun {
    let Some(mut acc) = into else { return one };
    // By metric name, never by position. A scenario is free to emit its series
    // in a different order or to emit a conditional one: `ReadPass` already
    // picks `p99` or `max` at run time depending on how many lookups a pass
    // made, so two children of one scenario can disagree about what their
    // second series is. Pairing by index would then concatenate one metric's
    // samples into another metric's array, under the first child's label, with
    // nothing to see afterwards. A metric that appears in one child and not
    // another is carried through and named in the reason, because a series
    // shorter than `reps` is a different defect and the aggregator has to be
    // able to tell them apart.
    let mut unmatched: Vec<String> = Vec::new();
    for series in one.series {
        match acc.series.iter_mut().find(|s| s.metric == series.metric) {
            Some(existing) => existing.samples.extend(series.samples),
            None => {
                unmatched.push(series.metric.clone());
                acc.series.push(series);
            }
        }
    }
    if !unmatched.is_empty() {
        let note = format!(
            "repetitions disagreed about which series they publish; {} appeared partway through",
            unmatched.join(", ")
        );
        acc.reason = Some(match acc.reason.take() {
            Some(existing) => format!("{existing}; {note}"),
            None => note,
        });
    }
    acc.reps.extend(one.reps);
    acc.discarded_warmup.extend(one.discarded_warmup);
    // Peak RSS across repetitions is the largest child's, which is the figure
    // a capacity question asks for.
    acc.peak_rss_bytes = match (acc.peak_rss_bytes, one.peak_rss_bytes) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    acc.heap_peak_bytes = match (acc.heap_peak_bytes, one.heap_peak_bytes) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    if one.outcome != "ok" {
        acc.outcome = one.outcome;
        acc.reason = one.reason;
    }
    acc
}

/// ISO 8601 UTC, to the millisecond.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
