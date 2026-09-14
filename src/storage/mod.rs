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
//! # This family does not need libvips
//!
//! Nothing in it touches the comparison, so it builds and runs without the
//! `libvips` feature, which is what lets it live in the cheap check job and in
//! a Docker stage that skips the source build entirely.

pub mod cells;
pub mod document;
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

use libviprs::checksum::{ChecksumAlgo, hash_tile};
use libviprs::planner::{PyramidPlan, TileCoord};
use libviprs::pyramid_reader::{DirectoryPyramidReader, PmTilesPyramidReader, PyramidReader};
use libviprs::sink::TileFormat;
use libviprs::sink_pmtiles::PmTilesSink;
use libviprs::{EngineBuilder, FsSink, PixelFormat, Raster};

use cells::{Backend, Cell, Profile, SEED, Source};
use document::{CellReport, Document, DocumentCell, InvariantBlock, MachineLoad};
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

/// Every scenario a sweep walks, in order.
///
/// K1.4 extends this with `open`, `first_lookup`, `decode_root`,
/// `read_tileid_order`, the `read_concurrent@T` curve and `requests`. What is
/// here is the reference set: one generation scenario and two pass scenarios,
/// which is the least that proves the skeleton measures anything.
pub fn registry() -> Vec<Box<dyn Scenario>> {
    scenarios::reference::all()
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
        let base = match root {
            Some(root) => root.to_path_buf(),
            None => std::env::temp_dir(),
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

/// A streaming sha256. `libviprs::checksum::hash_file` is `pub(crate)`, so the
/// streaming form is here rather than borrowed.
struct Sha256Stream(sha2::Sha256);

impl Sha256Stream {
    fn new() -> Sha256Stream {
        use sha2::Digest as _;
        Sha256Stream(sha2::Sha256::new())
    }

    fn update(&mut self, bytes: &[u8]) {
        use sha2::Digest as _;
        self.0.update(bytes);
    }

    fn finish(self) -> String {
        use sha2::Digest as _;
        let out = self.0.finalize();
        out.iter().map(|b| format!("{b:02x}")).collect()
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
                DirectoryPyramidReader::try_open(&self.artefact, self.plan.clone(), TileFormat::Png)
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
    if let Some(rss) = rss {
        if rss > 0 {
            wire.peak_rss_bytes = Some(rss);
        }
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
                disagreements.push(concat!(stringify!($field), " differs between reps").to_string());
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
    block.peak_rss_mb = wire
        .peak_rss_bytes
        .map(|b| b as f64 / (1024.0 * 1024.0))
        .filter(|_| matches!(scenario.isolation(), Isolation::ProcessPerRep));
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
            backend,
            cell,
            scenario: &scenario.name(),
            metric: scenario.primary(),
            isolation: scenario.isolation(),
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
            backend,
            cell,
            scenario: &scenario.name(),
            metric: MetricSpec {
                name: leak(&series.metric),
                unit: parse_unit(&series.unit),
                direction: parse_direction(&series.direction),
            },
            isolation: scenario.isolation(),
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

/// The metric name a child chose, which the parent did not know statically.
fn leak(name: &str) -> &'static str {
    Box::leak(name.to_string().into_boxed_str())
}

fn parse_unit(s: &str) -> Unit {
    match s {
        "ms" => Unit::Milliseconds,
        "us" => Unit::Microseconds,
        "1/s" => Unit::PerSecond,
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
    let started = now_iso();
    let mut doc = Document::new(profile, started);
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

        for (index, scenario) in registry().iter().enumerate() {
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
                        for row in rows_from_wire(
                            backend,
                            cell,
                            scenario.as_ref(),
                            profile,
                            &wire,
                            load,
                            timer,
                        ) {
                            doc.push(row);
                        }
                    }
                    Err(e) => eprintln!("storage: {} {}: {e}", backend.as_str(), cell.spec()),
                }
            }
        }
    }

    doc.rebuild_invariant_table();
    doc.finished_at = Some(now_iso());
    doc
}

/// Fold one more per-repetition child into the run so far.
fn merge(into: Option<WireRun>, one: WireRun) -> WireRun {
    let Some(mut acc) = into else { return one };
    for (i, series) in one.series.into_iter().enumerate() {
        match acc.series.get_mut(i) {
            Some(existing) => existing.samples.extend(series.samples),
            None => acc.series.push(series),
        }
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
