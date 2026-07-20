//! Shared utilities for libviprs benchmarks.
//!
//! Provides test raster generation, metric collection, and reporting
//! infrastructure used by both criterion benchmarks and standalone
//! profiling binaries.

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use libviprs::streaming::BudgetPolicy;
use libviprs::{
    CollectingObserver, EngineBuilder, EngineConfig, EngineEvent, EngineKind, FsSink, Layout,
    PixelFormat, PyramidPlan, PyramidPlanner, Raster, RasterStripSource, TileFormat,
};
use serde::{Deserialize, Serialize};

pub mod harness;
pub mod provenance;
pub mod version_id;
pub mod version_matrix;

/// Current on-disk schema version for [`BenchmarkSnapshot`] /
/// [`RunMetrics`]. Bump whenever a field is renamed or its meaning
/// changes so [`load_history`] can migrate older files forward. History
/// written before this field existed deserializes as `0` (via
/// `#[serde(default)]`) and is normalized on load.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

/// The single tile codec used on **both** sides of the cross-engine
/// comparison. The libviprs engines encode their tiles in this format via
/// [`FsSink`], and libvips `dzsave` is invoked with the matching `--suffix`,
/// so the codec is never a hidden variable between the two engines (issue
/// #153). Keep [`BENCH_TILE_FORMAT`] and [`BENCH_TILE_SUFFIX`] in lockstep.
pub const BENCH_TILE_FORMAT: TileFormat = TileFormat::Png;
/// dzsave `--suffix` (and file extension) matching [`BENCH_TILE_FORMAT`].
pub const BENCH_TILE_SUFFIX: &str = ".png";

/// The canonical measurement suite, defined once and shared by every axis so
/// "the identical suite" is a compile-time fact rather than hand-copied
/// literals. The everyday `report` binary and the version-matrix runner's
/// [`version_matrix::MatrixConfig::default`] both consume these, mirroring how
/// they already share [`harness::DEFAULT_ITERS`] / [`harness::DEFAULT_WARMUP`]
/// (issue #19). Changing any of them re-defines the suite for both axes at once.
pub const DEFAULT_SIZES: &[(u32, u32)] = &[(512, 512), (1024, 1024), (2048, 2048), (4096, 4096)];
/// Concurrency levels swept per size (`0` = serial).
pub const DEFAULT_CONCURRENCY: &[usize] = &[0, 4];
/// Tile edge in pixels for the measured pyramids.
pub const BENCH_TILE_SIZE: u32 = 256;
/// Streaming engine memory budget, in bytes (1 MB).
pub const BENCH_STREAMING_BUDGET: u64 = 1_000_000;

/// A snapshot of benchmark results for a specific libviprs version.
///
/// Stored in `report/benchmark_history.json` so that performance can be
/// tracked across releases. Each run appends one entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkSnapshot {
    /// On-disk schema version. `0` for files written before this field
    /// existed; [`load_history`] migrates those to [`CURRENT_SCHEMA_VERSION`]
    /// in memory (normalizing legacy run labels and mapping the old
    /// `peak_memory_bytes` field onto `tracked_memory_bytes`).
    #[serde(default)]
    pub schema_version: u32,
    /// Environment fingerprint under which this snapshot was measured:
    /// libvips version, host CPU/OS/arch, container flag, rustc, and the
    /// bench build profile. Deltas across snapshots with *different*
    /// fingerprints are not apples-to-apples; `cross_version` flags them.
    /// Defaults to an "unknown" fingerprint for pre-provenance history.
    #[serde(default)]
    pub provenance: provenance::Provenance,
    /// Version of the *measured* `libviprs` core crate, captured at
    /// build time from `../libviprs/Cargo.toml`. This is the engine the
    /// numbers describe, not this harness's own `CARGO_PKG_VERSION`
    /// (the two drift, e.g. bench 0.3.0 while core is 0.3.1).
    pub version: String,
    /// Short git SHA of the measured core crate's checkout, or
    /// `"unknown"` when git could not resolve it at build time. Defaults
    /// to empty for history files written before this field existed.
    #[serde(default)]
    pub git_sha: String,
    /// ISO 8601 timestamp of the run.
    pub timestamp: String,
    /// Tile size used.
    pub tile_size: u32,
    /// Memory budget used for streaming/mapreduce engines.
    pub memory_budget_bytes: u64,
    /// Individual run metrics.
    pub runs: Vec<RunMetrics>,
}

/// Metrics collected from a single benchmark run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMetrics {
    /// Human-readable label for this run.
    pub label: String,
    /// Image dimensions.
    pub width: u32,
    pub height: u32,
    /// Engine used.
    pub engine: String,
    /// How the libvips number was obtained: `"ffi"` (in-process
    /// `vips_dzsave`) or `"cli"` (`vips dzsave` child). Empty for the
    /// libviprs engines (always in-process). Recorded so a libvips row's
    /// measurement path is never a hidden variable. Defaults to empty for
    /// pre-provenance history.
    #[serde(default)]
    pub measurement_path: String,
    /// Wall-clock time for pyramid generation. When [`RunMetrics::stats`]
    /// is present this is the *median* iteration's wall time.
    pub wall_time: Duration,
    /// Peak engine-tracked working set in bytes (raster buffers the engine
    /// accounts for during the run). This is a per-run figure, reset for each
    /// engine, and is available only for the libviprs engines. libvips does
    /// not expose an equivalent internal counter, so it is reported as `0`
    /// there — the two figures are kept in **separate** columns rather than
    /// being compared against each other (issue #153).
    ///
    /// The `peak_memory_bytes` alias lets history files written under the
    /// pre-#153 schema (which used that name for the same quantity)
    /// deserialize unchanged.
    #[serde(alias = "peak_memory_bytes")]
    pub tracked_memory_bytes: u64,
    /// Peak resident set size (RSS) in bytes — the OS-level high-water mark.
    ///
    /// Measured on a single, per-run basis for **every** engine: the suite
    /// runs each (engine, size, concurrency) cell in its own child process
    /// and reads that child's `ru_maxrss` via `wait4` /
    /// `getrusage(RUSAGE_CHILDREN)` in the parent (see [`harness`]). Because
    /// the watermark is scoped to a fresh process per cell, it is a true
    /// per-run peak rather than the monotonic process-wide high-water mark
    /// that the old in-process path reported. `0` means unknown (older
    /// history predating this field).
    #[serde(default)]
    pub peak_rss_bytes: u64,
    /// Multi-iteration statistics for this cell (median/min/IQR/CI over
    /// at least 7 timed iterations after a discarded warm-up). `None` for a
    /// single-shot measurement (e.g. a child `--single` cell, or legacy
    /// history predating the statistics rework).
    #[serde(default)]
    pub stats: Option<RunStats>,
    /// PNG tiles produced per pyramid level, ordered by level directory
    /// name. Drives the cross-engine output-equivalence gate (equal level
    /// count + per-level grid). Empty for legacy history.
    #[serde(default)]
    pub per_level_tiles: Vec<u64>,
    /// Minimum PSNR (dB) of **this engine's own** mid-pyramid tiles against the
    /// libvips `dzsave` reference, from the pixel-level output-equivalence
    /// spot-check ([`harness::spot_check_tile_psnr`]). Each libviprs engine
    /// builds its *own* candidate pyramid at *this row's concurrency* and is
    /// scored against the shared per-size reference — the figure is never
    /// borrowed from another engine or configuration.
    ///
    /// The pixel-level companion to [`RunMetrics::per_level_tiles`]: the grid
    /// check proves the tiles have the right *geometry*, this proves they carry
    /// the right *pixels*, so a fast-but-visually-wrong engine is flagged
    /// (loud stderr WARNING + a `[FAIL]` line in the report) rather than
    /// passing on tile count alone. Advisory, not fatal — a sub-threshold score
    /// does not abort the run.
    ///
    /// Only the *minimum* PSNR is persisted here; the mean PSNR, min SSIM and
    /// compared-tile count from the richer [`harness::TileFidelityCheck`] are
    /// logged to stderr but intentionally not serialized. `None` when libvips
    /// was unavailable, for the libvips row itself, when the pyramid had no
    /// comparable multi-tile mid level (a tiny image), and for legacy history
    /// predating this field.
    #[serde(default)]
    pub equivalence_psnr_db: Option<f64>,
    /// Total tiles produced.
    pub tiles_produced: u64,
    /// Levels processed.
    pub levels_processed: u32,
    /// Tiles skipped (blank).
    pub tiles_skipped: u64,
    /// Number of strips (streaming/mapreduce only, 0 for monolithic).
    pub strips: u32,
    /// Number of batches (mapreduce only, 0 for others).
    pub batches: u32,
    /// In-flight strips per batch (mapreduce only, 0 for others).
    pub inflight_strips: u32,
    /// Concurrency level used.
    pub concurrency: usize,
    /// Memory budget in bytes (streaming/mapreduce only, 0 for monolithic).
    pub memory_budget_bytes: u64,
}

/// Multi-iteration statistics for one benchmark cell.
///
/// Populated by [`harness::measure_cell`], which runs each cell in a fresh
/// child process >= 7 times (after >= 1 discarded warm-up), interleaving
/// engine order within a size. Both wall time and per-run child RSS are
/// summarized so charts can draw error bars and `cross_version` can gate
/// regression calls on confidence-interval overlap rather than raw
/// single-sample deltas.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunStats {
    /// Number of timed iterations (excludes the discarded warm-up).
    pub n: u32,
    /// Wall time, milliseconds.
    pub wall_ms_median: f64,
    pub wall_ms_min: f64,
    /// Interquartile range (p75 − p25) of wall time, ms.
    pub wall_ms_iqr: f64,
    /// Half-width of the 95% CI of the mean wall time, ms (`1.96·σ/√n`).
    pub wall_ms_ci95: f64,
    /// Peak child RSS, MB.
    pub rss_mb_median: f64,
    pub rss_mb_min: f64,
    pub rss_mb_iqr: f64,
    pub rss_mb_ci95: f64,
}

impl RunStats {
    /// Summarize paired (wall_ms, rss_mb) samples. `samples` must be
    /// non-empty.
    pub fn from_samples(samples: &[(f64, f64)]) -> RunStats {
        let mut wall: Vec<f64> = samples.iter().map(|s| s.0).collect();
        let mut rss: Vec<f64> = samples.iter().map(|s| s.1).collect();
        wall.sort_by(|a, b| a.partial_cmp(b).unwrap());
        rss.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let (wm, wmin, wiqr, wci) = Self::summ(&wall);
        let (rm, rmin, riqr, rci) = Self::summ(&rss);
        RunStats {
            n: samples.len() as u32,
            wall_ms_median: wm,
            wall_ms_min: wmin,
            wall_ms_iqr: wiqr,
            wall_ms_ci95: wci,
            rss_mb_median: rm,
            rss_mb_min: rmin,
            rss_mb_iqr: riqr,
            rss_mb_ci95: rci,
        }
    }

    /// Returns (median, min, iqr, ci95_halfwidth) for a *sorted* slice.
    fn summ(sorted: &[f64]) -> (f64, f64, f64, f64) {
        let n = sorted.len();
        if n == 0 {
            return (0.0, 0.0, 0.0, 0.0);
        }
        let pct = |p: f64| -> f64 {
            if n == 1 {
                return sorted[0];
            }
            let rank = p * (n - 1) as f64;
            let lo = rank.floor() as usize;
            let hi = rank.ceil() as usize;
            let frac = rank - lo as f64;
            sorted[lo] * (1.0 - frac) + sorted[hi] * frac
        };
        let median = pct(0.5);
        let min = sorted[0];
        let iqr = pct(0.75) - pct(0.25);
        let mean = sorted.iter().sum::<f64>() / n as f64;
        let var = if n > 1 {
            sorted.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1) as f64
        } else {
            0.0
        };
        let ci95 = 1.96 * var.sqrt() / (n as f64).sqrt();
        (median, min, iqr, ci95)
    }
}

impl RunMetrics {
    /// Peak engine-tracked working set in MB (libviprs engines; `0` for
    /// libvips). A per-run, engine-internal figure — not comparable against
    /// [`RunMetrics::peak_rss_mb`], which is why the two are surfaced as
    /// separate, labelled columns.
    pub fn tracked_memory_mb(&self) -> f64 {
        self.tracked_memory_bytes as f64 / (1024.0 * 1024.0)
    }

    /// Peak process/child RSS in MB. This is the memory basis that is
    /// directly comparable across engines and drives the cross-engine
    /// efficiency and resource-cost metrics below.
    ///
    /// Caveat: for the in-process engines several runs share a single
    /// process and `ru_maxrss` is a monotonic high-water mark, so an
    /// in-process RSS figure reflects the largest allocation seen in the
    /// process up to that point rather than a freshly-reset per-run peak.
    /// The libvips CLI path spawns a child per run and therefore reports a
    /// strict per-run peak; isolate a single libviprs engine per process for
    /// the same guarantee.
    pub fn peak_rss_mb(&self) -> f64 {
        self.peak_rss_bytes as f64 / (1024.0 * 1024.0)
    }

    pub fn wall_time_ms(&self) -> f64 {
        self.wall_time.as_secs_f64() * 1000.0
    }

    pub fn tiles_per_second(&self) -> f64 {
        if self.wall_time.as_secs_f64() > 0.0 {
            self.tiles_produced as f64 / self.wall_time.as_secs_f64()
        } else {
            0.0
        }
    }

    /// Memory-normalised throughput on the common RSS basis: tiles per second
    /// per MB of peak RSS.
    ///
    /// Uses peak RSS — not the engine-tracked working set — so the figure
    /// means the same thing for libviprs and libvips. Comparing the libviprs
    /// engine-tracked bytes against the libvips process RSS is what made the
    /// old "Nx more memory-efficient" headline apples-to-oranges (issue
    /// #153); both sides now share the RSS basis. Returns `0` when RSS is
    /// unavailable.
    pub fn tiles_per_second_per_mb(&self) -> f64 {
        let mb = self.peak_rss_mb();
        if mb > 0.0 {
            self.tiles_per_second() / mb
        } else {
            0.0
        }
    }

    /// Resource cost: RSS-MB-seconds per tile.
    ///
    /// Lower is better. Measures the total resource-time consumed per tile on
    /// the common RSS basis, penalising both high memory and long wall time.
    /// Useful for comparing engines in environments where memory and CPU time
    /// are both billed (containers, serverless).
    pub fn resource_cost_per_tile(&self) -> f64 {
        let mb = self.peak_rss_mb();
        let secs = self.wall_time.as_secs_f64();
        if self.tiles_produced > 0 {
            (mb * secs) / self.tiles_produced as f64
        } else {
            0.0
        }
    }
}

/// Generate a synthetic gradient raster for benchmarking.
///
/// Uses a prime-weighted RGB pattern to avoid compression-friendly
/// uniformity while remaining deterministic.
pub fn gradient_raster(w: u32, h: u32) -> Raster {
    let bpp = PixelFormat::Rgb8.bytes_per_pixel();
    let mut data = vec![0u8; w as usize * h as usize * bpp];
    for y in 0..h {
        for x in 0..w {
            let off = (y as usize * w as usize + x as usize) * bpp;
            data[off] = (x % 256) as u8;
            data[off + 1] = (y % 256) as u8;
            data[off + 2] = ((x * 7 + y * 13) % 256) as u8;
        }
    }
    Raster::new(w, h, PixelFormat::Rgb8, data).unwrap()
}

/// Create a fresh, unique temp directory for a libviprs engine's on-disk tile
/// output. Both the libviprs engines and libvips `dzsave` write their tiles as
/// real files under `TMPDIR/libviprs-bench/…`, so neither side gets an in-RAM
/// sink advantage (issue #153). The directory is removed by the caller once
/// the tiles have been counted.
fn fs_sink_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir()
        .join("libviprs-bench")
        .join(format!("engine_{}_{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build the on-disk PNG sink used by every libviprs engine, rooted under a
/// fresh temp directory. Mirrors the libvips `dzsave` output: real files, same
/// [`BENCH_TILE_FORMAT`] codec, DeepZoom layout.
fn engine_fs_sink(out_dir: &std::path::Path, plan: &PyramidPlan) -> FsSink {
    FsSink::new(out_dir.join("pyramid"), plan.clone()).with_format(BENCH_TILE_FORMAT)
}

/// Run the monolithic engine and collect metrics.
pub fn bench_monolithic(
    src: &Raster,
    plan: &PyramidPlan,
    concurrency: usize,
    label: &str,
) -> RunMetrics {
    let out_dir = fs_sink_dir(label);
    let sink = engine_fs_sink(&out_dir, plan);
    let observer = Arc::new(CollectingObserver::new());
    let config = EngineConfig::default().with_concurrency(concurrency);

    let start = Instant::now();
    let result = EngineBuilder::new(src, plan.clone(), &sink)
        .with_engine(EngineKind::Monolithic)
        .with_config(config)
        .with_observer_arc(observer.clone())
        .run()
        .unwrap();
    let wall_time = start.elapsed();
    let peak_rss_bytes = get_peak_rss();
    let per_level_tiles = per_level_png_tiles(&out_dir.join("pyramid"));
    let _ = std::fs::remove_dir_all(&out_dir);

    RunMetrics {
        label: label.to_string(),
        width: src.width(),
        height: src.height(),
        engine: "monolithic".to_string(),
        measurement_path: String::new(),
        wall_time,
        tracked_memory_bytes: result.peak_memory_bytes,
        peak_rss_bytes,
        stats: None,
        per_level_tiles,
        equivalence_psnr_db: None,
        tiles_produced: result.tiles_produced,
        levels_processed: result.levels_processed,
        tiles_skipped: result.tiles_skipped,
        strips: 0,
        batches: 0,
        inflight_strips: 0,
        concurrency,
        memory_budget_bytes: 0,
    }
}

/// Run the streaming engine and collect metrics.
pub fn bench_streaming(
    src: &Raster,
    plan: &PyramidPlan,
    concurrency: usize,
    memory_budget_bytes: u64,
    label: &str,
) -> RunMetrics {
    let out_dir = fs_sink_dir(label);
    let sink = engine_fs_sink(&out_dir, plan);
    let observer = Arc::new(CollectingObserver::new());
    let engine_config = EngineConfig::default().with_concurrency(concurrency);
    let strip_src = RasterStripSource::new(src);
    let start = Instant::now();
    let result = EngineBuilder::new(strip_src, plan.clone(), &sink)
        .with_engine(EngineKind::Streaming)
        .with_config(engine_config)
        .with_memory_budget(memory_budget_bytes)
        .with_budget_policy(BudgetPolicy::Error)
        .with_observer_arc(observer.clone())
        .run()
        .unwrap();
    let wall_time = start.elapsed();
    let peak_rss_bytes = get_peak_rss();
    let per_level_tiles = per_level_png_tiles(&out_dir.join("pyramid"));
    let _ = std::fs::remove_dir_all(&out_dir);

    let strips = observer
        .events()
        .iter()
        .filter(|e| matches!(e, EngineEvent::StripRendered { .. }))
        .count() as u32;

    RunMetrics {
        label: label.to_string(),
        width: src.width(),
        height: src.height(),
        engine: "streaming".to_string(),
        measurement_path: String::new(),
        wall_time,
        tracked_memory_bytes: result.peak_memory_bytes,
        peak_rss_bytes,
        stats: None,
        per_level_tiles,
        equivalence_psnr_db: None,
        tiles_produced: result.tiles_produced,
        levels_processed: result.levels_processed,
        tiles_skipped: result.tiles_skipped,
        strips,
        batches: 0,
        inflight_strips: 0,
        concurrency,
        memory_budget_bytes,
    }
}

/// Run the MapReduce engine and collect metrics.
pub fn bench_mapreduce(
    src: &Raster,
    plan: &PyramidPlan,
    tile_concurrency: usize,
    memory_budget_bytes: u64,
    label: &str,
) -> RunMetrics {
    let out_dir = fs_sink_dir(label);
    let sink = engine_fs_sink(&out_dir, plan);
    let observer = Arc::new(CollectingObserver::new());
    let buffer_size = 64usize;
    let engine_config = EngineConfig::default()
        .with_concurrency(tile_concurrency)
        .with_buffer_size(buffer_size)
        .with_blank_tile_strategy(libviprs::BlankTileStrategy::Emit);
    let strip_src = RasterStripSource::new(src);
    let start = Instant::now();
    let result = EngineBuilder::new(strip_src, plan.clone(), &sink)
        .with_engine(EngineKind::MapReduce)
        .with_config(engine_config)
        .with_memory_budget(memory_budget_bytes)
        .with_budget_policy(BudgetPolicy::Error)
        .with_observer_arc(observer.clone())
        .run()
        .unwrap();
    let wall_time = start.elapsed();
    let peak_rss_bytes = get_peak_rss();
    let per_level_tiles = per_level_png_tiles(&out_dir.join("pyramid"));
    let _ = std::fs::remove_dir_all(&out_dir);

    let events = observer.events();
    let strips = events
        .iter()
        .filter(|e| matches!(e, EngineEvent::StripRendered { .. }))
        .count() as u32;
    let batches = events
        .iter()
        .filter(|e| matches!(e, EngineEvent::BatchStarted { .. }))
        .count() as u32;
    let strip_height = libviprs::compute_strip_height(plan, src.format(), memory_budget_bytes)
        .unwrap_or(2 * plan.tile_size);
    // Mirror the engine's own channel-backlog charge so the reported
    // in-flight strip count matches what the MapReduce engine actually
    // budgets for (libviprs `streaming_mapreduce`): the parallel emitter
    // holds up to `buffer_size + concurrency` decoded tiles in its bounded
    // channel. Zero when running single-threaded (`tile_concurrency == 0`).
    let channel_bytes = if tile_concurrency > 0 {
        let tile_bytes =
            plan.tile_size as u64 * plan.tile_size as u64 * src.format().bytes_per_pixel() as u64;
        (buffer_size as u64 + tile_concurrency as u64) * tile_bytes
    } else {
        0
    };
    let inflight = libviprs::streaming_mapreduce::compute_inflight_strips(
        plan,
        src.format(),
        strip_height,
        channel_bytes,
        memory_budget_bytes,
    );

    RunMetrics {
        label: label.to_string(),
        width: src.width(),
        height: src.height(),
        engine: "mapreduce".to_string(),
        measurement_path: String::new(),
        wall_time,
        tracked_memory_bytes: result.peak_memory_bytes,
        peak_rss_bytes,
        stats: None,
        per_level_tiles,
        equivalence_psnr_db: None,
        tiles_produced: result.tiles_produced,
        levels_processed: result.levels_processed,
        tiles_skipped: result.tiles_skipped,
        strips,
        batches,
        inflight_strips: inflight,
        concurrency: tile_concurrency,
        memory_budget_bytes,
    }
}

/// Failure modes of [`bench_streaming_pdf`], split so a benchmark driver can
/// skip the one environment-dependent case while still surfacing genuine
/// regressions (issue #22 review).
///
/// A single stringly-typed error would collapse "libpdfium isn't installed on
/// this machine" (a legitimate skip) together with "the planner or engine
/// regressed" (a real bug the benchmark exists to catch) into one opaque value,
/// so a driver that skips on *any* error would silently drop a regression from
/// the deliverable. Keeping them apart lets the driver skip only
/// [`SourceUnavailable`](Self::SourceUnavailable) and propagate the rest.
#[cfg(feature = "pdfium")]
#[derive(Debug)]
pub enum PdfBenchError {
    /// The pdfium source could not be constructed. The overwhelmingly common
    /// cause is that `libpdfium` is unavailable or ABI-incompatible on this
    /// machine — a *runtime/environment* condition, not a code regression — so
    /// a driver legitimately SKIPS the real-content cell rather than aborting
    /// the whole sweep.
    SourceUnavailable(libviprs::PdfError),
    /// Pyramid planning failed for the rendered page — a genuine regression the
    /// benchmark must surface, never skip.
    Plan(libviprs::PlannerError),
    /// The streaming engine run failed — a genuine regression the benchmark
    /// must surface, never skip.
    Engine(libviprs::EngineError),
}

#[cfg(feature = "pdfium")]
impl std::fmt::Display for PdfBenchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceUnavailable(e) => write!(f, "pdfium source unavailable (skippable): {e}"),
            Self::Plan(e) => write!(f, "pyramid planner failed: {e}"),
            Self::Engine(e) => write!(f, "streaming engine run failed: {e}"),
        }
    }
}

#[cfg(feature = "pdfium")]
impl std::error::Error for PdfBenchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SourceUnavailable(e) => Some(e),
            Self::Plan(e) => Some(e),
            Self::Engine(e) => Some(e),
        }
    }
}

/// Rasterize a PDF page straight into a DeepZoom pyramid via the streaming
/// engine — the real-content counterpart to the synthetic [`gradient_raster`]
/// streaming workload (issues #30 / #31).
///
/// Sources strips from a [`PdfiumStripSource`](libviprs::PdfiumStripSource) in
/// [`Streaming`](libviprs::PdfiumRenderMode::Streaming) mode, so the full page
/// is never materialised: the engine's *tracked* working set
/// ([`RunMetrics::tracked_memory_bytes`]) stays bounded by the strip in flight,
/// exactly the regime a rasterized full-page blueprint exercises. That bounded
/// figure is the engine's own accounting — the process
/// [`peak_rss_bytes`](RunMetrics::peak_rss_bytes) captured alongside it is a
/// monotonic, process-wide high-water mark (see [`RunMetrics::peak_rss_mb`]), so
/// in a driver that runs several engines in one process it does NOT isolate this
/// workload's strip-bounded footprint. The page is
/// rendered at `dpi`; the resulting raster dimensions (reported back in
/// [`RunMetrics::width`] / [`height`](RunMetrics::height)) grow with `dpi`,
/// which is how the scalability sweep drives one committed fixture to
/// progressively larger sizes. The run is tagged with the `"streaming-pdf"`
/// engine so it charts as a series distinct from the four gradient engines.
///
/// The DeepZoom plan is derived from the source's rendered dimensions, and the
/// budget is raised if necessary so the worst-case tile-aligned strip
/// (`width × 2·tile_size × bpp`, RGBA here) always fits under the strict
/// [`BudgetPolicy::Error`] — pdfium's 4-bpp strips are wider than the 3-bpp
/// gradient at the same width.
///
/// Returns a typed [`PdfBenchError`] (rather than panicking like the raster
/// [`bench_streaming`]) so a driver can tell the ONE legitimately-skippable
/// case — [`PdfBenchError::SourceUnavailable`], e.g. a missing or
/// ABI-mismatched `libpdfium` — apart from a genuine planner/engine regression
/// ([`PdfBenchError::Plan`] / [`PdfBenchError::Engine`]) that it must surface
/// rather than silently drop from the deliverable (issue #22 review).
#[cfg(feature = "pdfium")]
#[allow(clippy::too_many_arguments)]
pub fn bench_streaming_pdf(
    pdf_path: &std::path::Path,
    page: usize,
    dpi: u32,
    tile_size: u32,
    concurrency: usize,
    memory_budget_bytes: u64,
    label: &str,
) -> Result<RunMetrics, PdfBenchError> {
    use libviprs::{PdfiumStripSource, StripSource};

    let source = PdfiumStripSource::new_streaming(pdf_path, page, dpi)
        .map_err(PdfBenchError::SourceUnavailable)?;
    let width = source.width();
    let height = source.height();

    let planner = PyramidPlanner::new(width, height, tile_size, 0, Layout::DeepZoom)
        .map_err(PdfBenchError::Plan)?;
    let plan = planner.plan();

    // Admit the worst-case tile-aligned strip so `BudgetPolicy::Error` never
    // trips on the wide RGBA pdfium strips. Saturating arithmetic so an
    // adversarial page width or `tile_size` saturates the budget large (which
    // the engine then rejects cleanly) instead of wrapping small in release or
    // panicking in debug.
    let bpp = source.format().bytes_per_pixel() as u64;
    let min_strip_bytes = (width as u64)
        .saturating_mul(2u64.saturating_mul(tile_size as u64))
        .saturating_mul(bpp);
    let budget = memory_budget_bytes.max(min_strip_bytes.saturating_mul(2));

    let out_dir = fs_sink_dir(label);
    let sink = engine_fs_sink(&out_dir, &plan);
    let observer = Arc::new(CollectingObserver::new());
    let engine_config = EngineConfig::default().with_concurrency(concurrency);

    let start = Instant::now();
    let run_result = EngineBuilder::new(source, plan.clone(), &sink)
        .with_engine(EngineKind::Streaming)
        .with_config(engine_config)
        .with_memory_budget(budget)
        .with_budget_policy(BudgetPolicy::Error)
        .with_observer_arc(observer.clone())
        .run();
    let wall_time = start.elapsed();
    let peak_rss_bytes = get_peak_rss();
    // Reclaim the temp dir on EVERY exit — including the error path below — so a
    // post-creation engine failure never leaks a directory under $TMPDIR
    // (issue #22 review). Walking a partial/empty dir here is harmless; the
    // value is only consumed on the success path.
    let per_level_tiles = per_level_png_tiles(&out_dir.join("pyramid"));
    let _ = std::fs::remove_dir_all(&out_dir);
    let result = run_result.map_err(PdfBenchError::Engine)?;

    let strips = observer
        .events()
        .iter()
        .filter(|e| matches!(e, EngineEvent::StripRendered { .. }))
        .count() as u32;

    Ok(RunMetrics {
        label: label.to_string(),
        width,
        height,
        engine: "streaming-pdf".to_string(),
        measurement_path: String::new(),
        wall_time,
        tracked_memory_bytes: result.peak_memory_bytes,
        peak_rss_bytes,
        stats: None,
        per_level_tiles,
        tiles_produced: result.tiles_produced,
        levels_processed: result.levels_processed,
        tiles_skipped: result.tiles_skipped,
        strips,
        batches: 0,
        inflight_strips: 0,
        concurrency,
        memory_budget_bytes: budget,
        equivalence_psnr_db: None,
    })
}

/// Encode `src` as a lossless RGB8 PNG at `path`, creating parent directories.
///
/// The reusable core behind [`write_temp_png`]; unlike it, this reports IO and
/// encoder failures instead of panicking, so callers on a fail-soft path (the
/// output-equivalence spot-check) can degrade to "no score" rather than
/// aborting. The caller owns `path` and its cleanup.
pub fn write_png_at(src: &Raster, path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    let w = std::io::BufWriter::new(file);
    let encoder = image::codecs::png::PngEncoder::new(w);
    image::ImageEncoder::write_image(
        encoder,
        src.data(),
        src.width(),
        src.height(),
        image::ColorType::Rgb8.into(),
    )
    .map_err(std::io::Error::other)
}

/// Write a Raster to a temporary PNG file for libvips benchmarking.
///
/// Returns the path to the temp file. The caller is responsible for cleanup.
/// Panics on IO/encoder failure; use [`write_png_at`] where graceful
/// degradation is required.
pub fn write_temp_png(src: &Raster) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("libviprs-bench");
    let path = dir.join(format!("bench_{}x{}.png", src.width(), src.height()));
    write_png_at(src, &path).unwrap();
    path
}

/// Generate a libviprs pyramid from `src` into `dir` with the given `engine`
/// and `concurrency`, returning the tiles root (`dir/pyramid`, holding
/// `{level}/{col}_{row}.png`).
///
/// Parameterized over [`EngineKind`] so the output-equivalence spot-check can
/// validate **each** libviprs engine's own pixels (not just the monolithic
/// engine's) at the concurrency actually benchmarked. `budget_bytes` bounds the
/// streaming / mapreduce engines (ignored by the monolithic engine), mirroring
/// the timed `bench_*` paths so the candidate is built the same way it is
/// measured.
///
/// Unlike the `bench_*` functions this does **not** delete its output — the
/// caller owns `dir`. It exists for the pixel-level output-equivalence
/// spot-check ([`harness::spot_check_tile_psnr`]), which needs both engines'
/// tiles on disk at once. Uses the shared [`BENCH_TILE_FORMAT`] codec so the
/// tiles are byte-comparable with the libvips reference. Propagates the
/// engine's `run()` error (as an [`std::io::Error`]) rather than panicking, so
/// a spot-check failure degrades to "no score" instead of aborting the run.
pub fn write_libviprs_pyramid(
    src: &Raster,
    plan: &PyramidPlan,
    engine: EngineKind,
    concurrency: usize,
    budget_bytes: u64,
    dir: &std::path::Path,
) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let base = dir.join("pyramid");
    let sink = engine_fs_sink(dir, plan);
    let observer = Arc::new(CollectingObserver::new());
    let mut builder = EngineBuilder::new(src, plan.clone(), &sink)
        .with_engine(engine)
        .with_config(EngineConfig::default().with_concurrency(concurrency))
        .with_observer_arc(observer);
    // The streaming / mapreduce engines are budget-driven; bound them exactly
    // as their timed `bench_*` paths do. The monolithic engine ignores it.
    if matches!(engine, EngineKind::Streaming | EngineKind::MapReduce) {
        builder = builder
            .with_memory_budget(budget_bytes)
            .with_budget_policy(BudgetPolicy::Error);
    }
    builder
        .run()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(base)
}

/// Generate a libvips `dzsave` pyramid from a pre-written PNG (`png_path`) into
/// `dir` and return the DeepZoom tiles root (`dir/pyramid_files`). `None` if
/// `vips` fails or is absent.
///
/// Companion to [`write_libviprs_pyramid`] for the output-equivalence
/// spot-check: same tile size, `overlap 0`, and [`BENCH_TILE_SUFFIX`] codec as
/// the timed libvips CLI path, so the two pyramids are directly comparable.
pub fn write_libvips_pyramid(
    png_path: &std::path::Path,
    dir: &std::path::Path,
    tile_size: u32,
) -> Option<std::path::PathBuf> {
    let _ = std::fs::create_dir_all(dir);
    let dz_path = dir.join("pyramid");
    let output = Command::new("vips")
        .arg("dzsave")
        .arg(png_path)
        .arg(&dz_path)
        .args([
            "--tile-size",
            &tile_size.to_string(),
            "--overlap",
            "0",
            "--suffix",
            BENCH_TILE_SUFFIX,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        eprintln!(
            "vips dzsave failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return None;
    }
    let files = dir.join("pyramid_files");
    files.is_dir().then_some(files)
}

/// Check whether the `vips` CLI is available on the system.
pub fn vips_available() -> bool {
    Command::new("vips")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run libvips `dzsave` via the CLI and collect metrics.
///
/// Shells out to `vips dzsave` with the same tile parameters, measures
/// wall time and peak RSS (via `/usr/bin/time` on macOS, or `time -v` on
/// Linux). Counts output tiles by listing the output directory.
///
/// The `png_path` must point to a pre-written PNG file (use [`write_temp_png`]).
/// The concurrency parameter maps to `VIPS_CONCURRENCY`.
pub fn bench_libvips(
    png_path: &std::path::Path,
    width: u32,
    height: u32,
    tile_size: u32,
    concurrency: usize,
    label: &str,
) -> Option<RunMetrics> {
    let out_dir = std::env::temp_dir()
        .join("libviprs-bench")
        .join(format!("vips_{}_{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).unwrap();

    let dz_path = out_dir.join("pyramid");

    // Run vips dzsave with /usr/bin/time to capture peak RSS
    let conc_str = concurrency.max(1).to_string();
    let ts_str = tile_size.to_string();

    let start = Instant::now();

    // Try GNU time first (Linux), fall back to BSD time (macOS)
    let output = if cfg!(target_os = "macos") {
        Command::new("/usr/bin/time")
            .args(["-l", "vips", "dzsave"])
            .arg(png_path)
            .arg(&dz_path)
            .args([
                "--tile-size",
                &ts_str,
                "--overlap",
                "0",
                "--suffix",
                BENCH_TILE_SUFFIX,
            ])
            .env("VIPS_CONCURRENCY", &conc_str)
            .output()
    } else {
        Command::new("/usr/bin/time")
            .args(["-v", "vips", "dzsave"])
            .arg(png_path)
            .arg(&dz_path)
            .args([
                "--tile-size",
                &ts_str,
                "--overlap",
                "0",
                "--suffix",
                BENCH_TILE_SUFFIX,
            ])
            .env("VIPS_CONCURRENCY", &conc_str)
            .output()
    };

    let wall_time = start.elapsed();

    let output = match output {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            eprintln!("vips dzsave failed: {}", String::from_utf8_lossy(&o.stderr));
            let _ = std::fs::remove_dir_all(&out_dir);
            return None;
        }
        Err(e) => {
            eprintln!("failed to run vips: {e}");
            let _ = std::fs::remove_dir_all(&out_dir);
            return None;
        }
    };

    // Parse peak RSS from /usr/bin/time stderr output
    let stderr = String::from_utf8_lossy(&output.stderr);
    let peak_memory_bytes = if cfg!(target_os = "macos") {
        // macOS: "  NNN  peak memory footprint" (bytes)
        // or "  NNN  maximum resident set size" (bytes)
        stderr
            .lines()
            .find_map(|line| {
                let line = line.trim();
                if line.contains("peak memory footprint")
                    || line.contains("maximum resident set size")
                {
                    line.split_whitespace().next()?.parse::<u64>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0)
    } else {
        // Linux: "Maximum resident set size (kbytes): NNN"
        stderr
            .lines()
            .find_map(|line| {
                if line.contains("Maximum resident set size") {
                    line.split(':')
                        .nth(1)?
                        .trim()
                        .parse::<u64>()
                        .ok()
                        .map(|kb| kb * 1024)
                } else {
                    None
                }
            })
            .unwrap_or(0)
    };

    // Count output tiles — PNG only, so the libvips `vips-properties.xml`
    // sidecar does not inflate the count (issue #158).
    let tiles_dir = out_dir.join("pyramid_files");
    let per_level_tiles = if tiles_dir.exists() {
        per_level_png_tiles(&tiles_dir)
    } else {
        Vec::new()
    };
    let tiles_produced: u64 = per_level_tiles.iter().sum();
    let levels_processed = per_level_tiles.len() as u32;

    // Cleanup
    let _ = std::fs::remove_dir_all(&out_dir);

    Some(RunMetrics {
        label: label.to_string(),
        width,
        height,
        engine: "libvips".to_string(),
        measurement_path: "cli".to_string(),
        wall_time,
        // libvips exposes no engine-internal working-set counter, so the
        // tracked column is left at 0 and only the RSS column is populated.
        tracked_memory_bytes: 0,
        peak_rss_bytes: peak_memory_bytes,
        stats: None,
        per_level_tiles,
        equivalence_psnr_db: None,
        tiles_produced,
        levels_processed,
        tiles_skipped: 0,
        strips: 0,
        batches: 0,
        inflight_strips: 0,
        concurrency,
        memory_budget_bytes: 0,
    })
}

/// RAII wrapper around a no-copy `VipsImage` created from a [`Raster`]'s
/// pixel buffer.
///
/// `vips_image_new_from_memory` does **not** copy: the returned image
/// aliases the raster's bytes. This guard borrows the raster for `'a`, so
/// the image handle cannot outlive the buffer it points into, and it
/// unrefs the image on drop. The borrow is enforced at compile time:
///
/// ```compile_fail
/// use libviprs_bench::{gradient_raster, VipsImageGuard};
/// let guard;
/// {
///     let raster = gradient_raster(8, 8);
///     guard = VipsImageGuard::from_raster(&raster).unwrap();
/// } // `raster` dropped here, but `guard` still borrows it
/// let _ = guard.as_ptr();
/// ```
#[cfg(feature = "libvips")]
pub struct VipsImageGuard<'a> {
    img: *mut libvips_rs::bindings::VipsImage,
    _borrow: std::marker::PhantomData<&'a [u8]>,
}

#[cfg(feature = "libvips")]
impl<'a> VipsImageGuard<'a> {
    /// Wrap `src` as a no-copy `VipsImage`. Returns `None` if libvips
    /// rejects the buffer. The returned guard borrows `src` for `'a`.
    pub fn from_raster(src: &'a Raster) -> Option<Self> {
        let w = src.width() as i32;
        let h = src.height() as i32;
        // RGB8 = 3 bands, RGBA8 = 4 bands, Gray8 = 1 band
        let bands = src.format().bytes_per_pixel() as i32;
        let data = src.data();
        // SAFETY: `data` is borrowed for `'a` and outlives the returned
        // guard, so the aliased buffer stays valid for the image's life;
        // we pass its true byte length and vips reads only within it.
        let img = unsafe {
            libvips_rs::bindings::vips_image_new_from_memory(
                data.as_ptr() as *const std::ffi::c_void,
                data.len() as u64,
                w,
                h,
                bands,
                libvips_rs::bindings::VipsBandFormat_VIPS_FORMAT_UCHAR,
            )
        };
        if img.is_null() {
            eprintln!("vips_image_new_from_memory failed");
            return None;
        }
        Some(Self {
            img,
            _borrow: std::marker::PhantomData,
        })
    }

    /// Raw `VipsImage` pointer, valid for the lifetime of this guard.
    pub fn as_ptr(&self) -> *mut libvips_rs::bindings::VipsImage {
        self.img
    }
}

#[cfg(feature = "libvips")]
impl Drop for VipsImageGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: `img` is a non-null `VipsImage` for which this guard
        // holds exactly one reference, released here.
        unsafe {
            libvips_rs::bindings::g_object_unref(self.img as *mut std::ffi::c_void);
        }
    }
}

/// Run libvips dzsave in-process via FFI bindings (requires `libvips` feature).
///
/// Creates a `VipsImage` from the raw pixel buffer, runs `dzsave` to a temp
/// directory, and measures wall time + counts output tiles. This avoids the
/// process-spawn and PNG decode overhead of the CLI path, giving a fair
/// comparison of the tiling pipelines.
///
/// Falls back to the CLI path when the `libvips` feature is not enabled.
#[cfg(feature = "libvips")]
pub fn bench_libvips_inprocess(
    src: &Raster,
    tile_size: u32,
    concurrency: usize,
    label: &str,
) -> Option<RunMetrics> {
    use libvips_rs::VipsApp;
    use std::ffi::CString;

    // Initialize libvips once. A `OnceLock` gives a sound, process-wide
    // handle without an aliased `static mut` global (see #152).
    static APP: std::sync::OnceLock<VipsApp> = std::sync::OnceLock::new();
    let app = APP.get_or_init(|| VipsApp::new("bench", false).unwrap());
    app.concurrency_set(concurrency.max(1) as i32);

    // Wrap the raster as a no-copy `VipsImage`. The guard borrows `src`,
    // so the image handle cannot outlive the buffer it aliases; it unrefs
    // the image on drop.
    let img_guard = VipsImageGuard::from_raster(src)?;
    let img = img_guard.as_ptr();

    // dzsave to temp directory
    let out_dir = std::env::temp_dir()
        .join("libviprs-bench")
        .join(format!("vips_inproc_{}_{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).unwrap();

    let dz_path = out_dir.join("pyramid");
    let dz_path_c = CString::new(dz_path.to_str().unwrap()).unwrap();
    // Same tile codec as the libviprs `FsSink` and the libvips CLI path — the
    // in-process path historically wrote `.raw`, which meant "libvips" rows
    // secretly skipped PNG tile encoding that both other paths paid (issue
    // #153). Encode PNG here too so the codec is identical everywhere.
    let suffix_c = CString::new(BENCH_TILE_SUFFIX).unwrap();
    let tile_size_c = CString::new("tile-size").unwrap();
    let overlap_c = CString::new("overlap").unwrap();
    let suffix_opt_c = CString::new("suffix").unwrap();

    let start = Instant::now();
    let ret = unsafe {
        libvips_rs::bindings::vips_dzsave(
            img,
            dz_path_c.as_ptr(),
            tile_size_c.as_ptr(),
            tile_size as i32,
            overlap_c.as_ptr(),
            0i32,
            suffix_opt_c.as_ptr(),
            suffix_c.as_ptr(),
            std::ptr::null::<std::ffi::c_void>(),
        )
    };
    let wall_time = start.elapsed();

    // `img_guard` unrefs the VipsImage when it drops at the end of scope.

    if ret != 0 {
        eprintln!("vips_dzsave failed (return code {ret})");
        let _ = std::fs::remove_dir_all(&out_dir);
        return None;
    }

    // Count tiles — PNG only (issue #158).
    let tiles_dir = out_dir.join("pyramid_files");
    let per_level_tiles = if tiles_dir.exists() {
        per_level_png_tiles(&tiles_dir)
    } else {
        Vec::new()
    };
    let tiles_produced: u64 = per_level_tiles.iter().sum();
    let levels_processed = per_level_tiles.len() as u32;

    // Measure peak RSS of the current process. This is a process-wide
    // high-water mark (see `RunMetrics::peak_rss_mb`), and libvips exposes no
    // internal working-set counter, so the tracked column stays 0.
    let peak_rss_bytes = get_peak_rss();

    let _ = std::fs::remove_dir_all(&out_dir);

    Some(RunMetrics {
        label: label.to_string(),
        width: src.width(),
        height: src.height(),
        engine: "libvips".to_string(),
        measurement_path: "ffi".to_string(),
        wall_time,
        tracked_memory_bytes: 0,
        peak_rss_bytes,
        stats: None,
        per_level_tiles,
        equivalence_psnr_db: None,
        tiles_produced,
        levels_processed,
        tiles_skipped: 0,
        strips: 0,
        batches: 0,
        inflight_strips: 0,
        concurrency,
        memory_budget_bytes: 0,
    })
}

/// Get current process peak RSS in bytes (`getrusage(RUSAGE_SELF)`).
pub fn get_peak_rss() -> u64 {
    #[cfg(target_os = "macos")]
    {
        use std::mem::MaybeUninit;
        let mut rusage = MaybeUninit::<libc::rusage>::uninit();
        let ret = unsafe { libc::getrusage(libc::RUSAGE_SELF, rusage.as_mut_ptr()) };
        if ret == 0 {
            let rusage = unsafe { rusage.assume_init() };
            // macOS reports ru_maxrss in bytes
            rusage.ru_maxrss as u64
        } else {
            0
        }
    }
    #[cfg(target_os = "linux")]
    {
        use std::mem::MaybeUninit;
        let mut rusage = MaybeUninit::<libc::rusage>::uninit();
        let ret = unsafe { libc::getrusage(libc::RUSAGE_SELF, rusage.as_mut_ptr()) };
        if ret == 0 {
            let rusage = unsafe { rusage.assume_init() };
            // Linux reports ru_maxrss in kilobytes
            rusage.ru_maxrss as u64 * 1024
        } else {
            0
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}

/// Recursively count only `*.png` tiles under `dir`.
///
/// The DeepZoom `_files` tree that both libvips `dzsave` and the libviprs
/// [`FsSink`] emit also contains non-tile sidecars — libvips writes a
/// `vips-properties.xml` per output. Counting every file (the old
/// behaviour) inflated the libvips tile count by those sidecars, so the
/// libvips "tiles produced" — and every throughput/efficiency figure
/// derived from it — was overstated (issue #158). Restricting the walk to
/// the actual tile codec ([`BENCH_TILE_SUFFIX`]) puts every engine on the
/// same tile basis.
pub fn count_png_tiles(dir: &std::path::Path) -> u64 {
    let mut count = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                count += count_png_tiles(&path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("png") {
                count += 1;
            }
        }
    }
    count
}

/// PNG tiles produced per pyramid level, ordered by numeric level name.
///
/// `tiles_dir` is the DeepZoom `<name>_files` directory. Each immediate
/// subdirectory is one pyramid level (`0`, `1`, `2`, …); the returned vec
/// is the `*.png` tile count of each, sorted by level index. This is the
/// per-level grid the cross-engine output-equivalence gate compares.
pub fn per_level_png_tiles(tiles_dir: &std::path::Path) -> Vec<u64> {
    let mut levels: Vec<(u32, u64)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(tiles_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let idx = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.parse::<u32>().ok())
                    .unwrap_or(u32::MAX);
                levels.push((idx, count_png_tiles(&path)));
            }
        }
    }
    levels.sort_by_key(|(idx, _)| *idx);
    levels.into_iter().map(|(_, c)| c).collect()
}

/// Run all four engines across a matrix of image sizes and concurrency levels.
pub fn comparison_suite(
    sizes: &[(u32, u32)],
    concurrency_levels: &[usize],
    tile_size: u32,
    streaming_budget_bytes: u64,
) -> Vec<RunMetrics> {
    let mut results = Vec::new();

    let has_vips = vips_available();
    if has_vips {
        eprintln!("libvips CLI detected — including in benchmarks");
    } else {
        eprintln!("libvips CLI not found — skipping libvips benchmarks");
    }

    for &(w, h) in sizes {
        let src = gradient_raster(w, h);
        let planner = PyramidPlanner::new(w, h, tile_size, 0, Layout::DeepZoom).unwrap();
        let plan = planner.plan();

        // Write temp PNG once per image size for libvips
        let png_path = if has_vips {
            Some(write_temp_png(&src))
        } else {
            None
        };

        for &conc in concurrency_levels {
            let label = format!("{w}x{h}_c{conc}");

            let mono = bench_monolithic(&src, &plan, conc, &format!("{label}_mono"));
            results.push(mono);

            let stream = bench_streaming(
                &src,
                &plan,
                conc,
                streaming_budget_bytes,
                &format!("{label}_stream"),
            );
            results.push(stream);

            let mr = bench_mapreduce(
                &src,
                &plan,
                conc,
                streaming_budget_bytes,
                &format!("{label}_mr"),
            );
            results.push(mr);

            // libvips: prefer in-process FFI when available, fall back to CLI.
            // `vips_done` is only reassigned under the `libvips` feature.
            #[cfg_attr(not(feature = "libvips"), allow(unused_mut))]
            let mut vips_done = false;
            #[cfg(feature = "libvips")]
            {
                if let Some(vips) =
                    bench_libvips_inprocess(&src, tile_size, conc, &format!("{label}_vips"))
                {
                    results.push(vips);
                    vips_done = true;
                }
            }
            if !vips_done {
                if let Some(ref png) = png_path {
                    if let Some(vips) =
                        bench_libvips(png, w, h, tile_size, conc, &format!("{label}_vips"))
                    {
                        results.push(vips);
                    }
                }
            }
        }

        // Clean up temp PNG
        if let Some(ref png) = png_path {
            let _ = std::fs::remove_file(png);
        }
    }

    results
}

/// Print a comparison table to stdout.
pub fn print_comparison_table(results: &[RunMetrics]) {
    println!(
        "{:<24} {:<12} {:>10} {:>12} {:>10} {:>8} {:>8} {:>10} {:>12}",
        "Label",
        "Engine",
        "Time (ms)",
        "Tracked MB",
        "RSS MB",
        "Tiles",
        "T/s",
        "T/s/RSS-MB",
        "RSS-MB\u{00b7}s/tile"
    );
    println!("{}", "-".repeat(112));

    for r in results {
        println!(
            "{:<24} {:<12} {:>10.1} {:>12.2} {:>10.2} {:>8} {:>8.0} {:>10.1} {:>12.4}",
            r.label,
            r.engine,
            r.wall_time_ms(),
            r.tracked_memory_mb(),
            r.peak_rss_mb(),
            r.tiles_produced,
            r.tiles_per_second(),
            r.tiles_per_second_per_mb(),
            r.resource_cost_per_tile(),
        );
    }
}

/// Group results by config key (width × height × concurrency).
///
/// Returns groups in insertion order, each containing all engine runs
/// for that configuration. Works with 3 or 4 engines.
pub fn grouped_results(results: &[RunMetrics]) -> Vec<Vec<&RunMetrics>> {
    let mut map: std::collections::BTreeMap<String, Vec<&RunMetrics>> =
        std::collections::BTreeMap::new();
    for r in results {
        let key = format!("{}x{}_c{}", r.width, r.height, r.concurrency);
        map.entry(key).or_default().push(r);
    }
    map.into_values().collect()
}

/// Load benchmark history from disk.
///
/// A missing file is not an error: there's simply no history yet, so I
/// return an empty vec. A file that exists but fails to parse *is* an
/// error, surfaced to the caller instead of swallowed. The previous
/// `unwrap_or_default()` turned a corrupt file into an empty vec, and
/// the very next [`save_history`] then overwrote the file with only the
/// current run, silently destroying every accumulated snapshot. Callers
/// must treat `Err` as "leave the existing file untouched".
pub fn load_history(path: &std::path::Path) -> Result<Vec<BenchmarkSnapshot>, String> {
    match std::fs::read_to_string(path) {
        Ok(json) => {
            let mut history: Vec<BenchmarkSnapshot> = serde_json::from_str(&json)
                .map_err(|e| format!("couldn't parse benchmark history {}: {e}", path.display()))?;
            for snap in &mut history {
                migrate_snapshot(snap);
            }
            Ok(history)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!(
            "couldn't read benchmark history {}: {e}",
            path.display()
        )),
    }
}

/// Migrate a just-deserialized snapshot forward to
/// [`CURRENT_SCHEMA_VERSION`] in place.
///
/// Older history files (schema 0) used the field name `peak_memory_bytes`
/// for what is now `tracked_memory_bytes` (handled by the serde alias) and
/// had **no** RSS column, so `peak_rss_bytes` defaults to 0 ("unknown").
/// They also wrote run labels space-separated ("1024x1024 c0 monolithic")
/// rather than the current underscore form ("1024x1024_c0_monolithic"),
/// which broke every `starts_with("{w}x{h}_c{c}")` filter in the
/// history/cross-version pipeline. Normalize both here so a single load
/// path serves every file version.
pub fn migrate_snapshot(snap: &mut BenchmarkSnapshot) {
    for run in &mut snap.runs {
        run.label = normalize_run_label(&run.label);
    }
    if snap.schema_version < CURRENT_SCHEMA_VERSION {
        snap.schema_version = CURRENT_SCHEMA_VERSION;
    }
}

/// Normalize a legacy space-separated run label to the current
/// underscore form. `"1024x1024 c0 monolithic"` → `"1024x1024_c0_monolithic"`.
/// Labels already in the current form (no spaces) pass through unchanged.
pub fn normalize_run_label(label: &str) -> String {
    if !label.contains(' ') {
        return label.to_string();
    }
    label.split_whitespace().collect::<Vec<_>>().join("_")
}

/// Version of the *measured* `libviprs` core crate, captured at build
/// time from `../libviprs/Cargo.toml` (see `build.rs`). Falls back to
/// `"unknown"` if the build script could not stamp it.
pub fn core_version() -> &'static str {
    option_env!("LIBVIPRS_CORE_VERSION").unwrap_or("unknown")
}

/// Short git SHA of the measured core crate's checkout, captured at
/// build time. `"unknown"` when git could not resolve it.
pub fn core_git_sha() -> &'static str {
    option_env!("LIBVIPRS_CORE_GIT_SHA").unwrap_or("unknown")
}

/// Persist the full history to `path` atomically.
///
/// The history file is the canonical accumulation of every recorded run, so a
/// partial write must never be able to truncate it: a crash or `ENOSPC` between
/// `open`-for-truncate and the final `write` (what the previous
/// `fs::write(path, …)` did) would leave a mangled file and lose the lot. This
/// serializes to a sibling temp file in the *same directory* and `rename`s it
/// into place — `rename` is atomic on POSIX, so a reader (and a crash) sees
/// either the complete old file or the complete new one, never a torn one.
///
/// # Errors
/// Returns the rendered IO/serialization error if the temp file cannot be
/// written, flushed, or renamed. The destination is left untouched on error, so
/// callers can surface the failure and keep the prior history intact. The
/// version-matrix runner writes this file once per version, so hardening the
/// primitive here protects the tool that exercises it the hardest (issue #19).
pub fn save_history(path: &std::path::Path, history: &[BenchmarkSnapshot]) -> Result<(), String> {
    use std::io::Write as _;

    let json = serde_json::to_string_pretty(history)
        .map_err(|e| format!("couldn't serialize benchmark history: {e}"))?;

    // Temp sibling in the same directory so the final `rename` is a same-filesystem
    // (hence atomic) move rather than a cross-device copy.
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp.{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("benchmark_history.json"),
        std::process::id()
    ));

    let mut f = std::fs::File::create(&tmp)
        .map_err(|e| format!("couldn't create temp history {}: {e}", tmp.display()))?;
    f.write_all(json.as_bytes())
        .and_then(|()| f.flush())
        .map_err(|e| format!("couldn't write temp history {}: {e}", tmp.display()))?;
    drop(f);

    std::fs::rename(&tmp, path).map_err(|e| {
        // Best effort: don't leave the temp file lying around on a failed rename.
        let _ = std::fs::remove_file(&tmp);
        format!("couldn't move history into place {}: {e}", path.display())
    })
}

/// Create a `BenchmarkSnapshot` from current run metrics, tagged with the
/// core version/SHA this harness was built against (`build.rs` stamps).
pub fn create_snapshot(
    runs: Vec<RunMetrics>,
    tile_size: u32,
    memory_budget_bytes: u64,
) -> BenchmarkSnapshot {
    create_snapshot_for(
        core_version(),
        core_git_sha(),
        runs,
        tile_size,
        memory_budget_bytes,
    )
}

/// Create a `BenchmarkSnapshot` tagged with an *explicit* measured core
/// version + short SHA, rather than this process's compile-time stamps.
///
/// The version-matrix runner ([`version_matrix`]) needs this: it drives a
/// *separately built* harness per tag, so the version/SHA a snapshot should
/// carry is the one it resolved from that tag's worktree, not the version this
/// driver binary was itself compiled against. The environment fingerprint is
/// still captured live from the current host/toolchain.
pub fn create_snapshot_for(
    version: &str,
    git_sha: &str,
    runs: Vec<RunMetrics>,
    tile_size: u32,
    memory_budget_bytes: u64,
) -> BenchmarkSnapshot {
    BenchmarkSnapshot {
        schema_version: CURRENT_SCHEMA_VERSION,
        provenance: provenance::Provenance::capture(),
        version: version.to_string(),
        git_sha: git_sha.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        tile_size,
        memory_budget_bytes,
        runs,
    }
}

// ---------------------------------------------------------------------------
// SVG chart generation via plotters
// ---------------------------------------------------------------------------
//
// Scope note (issue #20/#21 charting rework): the history-trend and
// scalability SVGs are now rendered in JS (`tools/charts/chart.mjs` +
// `render.mjs`) from the harness JSON. The six grouped-bar `chart_*.svg`
// comparison charts below (`generate_charts`) remain Rust-owned via plotters
// for now; porting them to JS and dropping the plotters dependency is tracked
// as follow-up work. Until then the engine palette here is intentionally
// MIRRORED by `COLORS` in `tools/charts/chart.mjs` (same RGB values) and the
// two must be kept in lockstep.

use plotters::prelude::*;

/// Color palette for the four engines. Mirrored — keep in lockstep — with the
/// `COLORS` map in `tools/charts/chart.mjs` (the JS history/scalability charts).
const COLOR_VIPS: RGBColor = RGBColor(156, 39, 176); // purple — libvips
const COLOR_MONO: RGBColor = RGBColor(66, 133, 244); // blue   — monolithic
const COLOR_STREAM: RGBColor = RGBColor(52, 168, 83); // green  — streaming
const COLOR_MR: RGBColor = RGBColor(234, 67, 53); // red    — mapreduce

/// One bar: value, error half-width (0 when unknown), engine name, colour.
type Bar = (f64, f64, &'static str, RGBColor);

/// Grouped bar chart data.
struct ChartGroup {
    label: String,
    values: Vec<Bar>,
}

/// Extract chart groups from results. Groups by (width, height, concurrency).
/// Each group contains one bar per engine found. `error` yields the error
/// bar half-width for each bar (e.g. CI95 of the plotted metric); return `0`
/// to suppress the whisker.
fn extract_groups(
    results: &[RunMetrics],
    metric: fn(&RunMetrics) -> f64,
    error: fn(&RunMetrics) -> f64,
) -> Vec<ChartGroup> {
    // Group results by config key
    let mut map: std::collections::BTreeMap<String, Vec<&RunMetrics>> =
        std::collections::BTreeMap::new();
    for r in results {
        let key = format!("{}x{}_c{}", r.width, r.height, r.concurrency);
        map.entry(key).or_default().push(r);
    }

    map.into_values()
        .map(|runs| {
            let first = runs[0];
            let label = format!("{}x{}\nc{}", first.width, first.height, first.concurrency);
            let values: Vec<Bar> = runs
                .iter()
                .filter_map(|r| {
                    let (name, color) = match r.engine.as_str() {
                        "libvips" => ("libvips", COLOR_VIPS),
                        "monolithic" => ("Monolithic", COLOR_MONO),
                        "streaming" => ("Streaming", COLOR_STREAM),
                        "mapreduce" => ("MapReduce", COLOR_MR),
                        _ => return None,
                    };
                    Some((metric(r), error(r), name, color))
                })
                .collect();
            ChartGroup { label, values }
        })
        .collect()
}

/// No error bars (for metrics without a meaningful CI, e.g. tile counts).
fn no_error(_: &RunMetrics) -> f64 {
    0.0
}

fn draw_grouped_bar_chart(
    path: &std::path::Path,
    title: &str,
    y_label: &str,
    groups: &[ChartGroup],
) {
    let max_val = groups
        .iter()
        .flat_map(|g| g.values.iter().map(|(v, e, _, _)| *v + *e))
        .fold(0.0f64, f64::max)
        * 1.15;

    // Maximum number of bars in any group
    let max_bars = groups.iter().map(|g| g.values.len()).max().unwrap_or(1);

    let n = groups.len();
    let chart_w = 160 + n as u32 * (max_bars as u32 * 35 + 50);
    let chart_h = 420;

    let root = SVGBackend::new(path, (chart_w, chart_h)).into_drawing_area();
    root.fill(&WHITE).unwrap();

    let mut chart = ChartBuilder::on(&root)
        .caption(title, ("sans-serif", 18).into_font())
        .margin(10)
        .x_label_area_size(55)
        .y_label_area_size(65)
        .build_cartesian_2d(0.0..(n as f64), 0.0..max_val)
        .unwrap();

    chart
        .configure_mesh()
        .disable_x_mesh()
        .y_desc(y_label)
        .x_labels(n)
        .x_label_formatter(&|x| {
            let idx = *x as usize;
            if idx < groups.len() {
                groups[idx].label.clone()
            } else {
                String::new()
            }
        })
        .y_label_formatter(&|y| {
            if *y >= 1000.0 {
                format!("{:.0}k", y / 1000.0)
            } else if *y >= 1.0 {
                format!("{:.0}", y)
            } else {
                format!("{:.2}", y)
            }
        })
        .draw()
        .unwrap();

    let bar_w = 0.85 / max_bars as f64;
    let gap = 0.02;

    // Draw bars for each group
    for (i, group) in groups.iter().enumerate() {
        let x = i as f64;
        for (j, (val, err, _name, color)) in group.values.iter().enumerate() {
            let bx = x + gap + j as f64 * (bar_w + gap);
            chart
                .draw_series(std::iter::once(Rectangle::new(
                    [(bx, 0.0), (bx + bar_w, *val)],
                    color.filled(),
                )))
                .unwrap();

            // Error whisker (± half-width, e.g. 95% CI) when available.
            if *err > 0.0 {
                let cx = bx + bar_w / 2.0;
                let cap = bar_w * 0.25;
                chart
                    .draw_series(std::iter::once(PathElement::new(
                        vec![(cx, val - err), (cx, val + err)],
                        BLACK.stroke_width(1),
                    )))
                    .unwrap();
                chart
                    .draw_series(std::iter::once(PathElement::new(
                        vec![(cx - cap, val + err), (cx + cap, val + err)],
                        BLACK.stroke_width(1),
                    )))
                    .unwrap();
                chart
                    .draw_series(std::iter::once(PathElement::new(
                        vec![(cx - cap, val - err), (cx + cap, val - err)],
                        BLACK.stroke_width(1),
                    )))
                    .unwrap();
            }

            // Value label above bar (above the whisker when present)
            let label = if *val >= 100.0 {
                format!("{:.0}", val)
            } else if *val >= 1.0 {
                format!("{:.1}", val)
            } else {
                format!("{:.2}", val)
            };
            chart
                .draw_series(std::iter::once(Text::new(
                    label,
                    (bx + bar_w / 2.0, val + err + max_val * 0.01),
                    ("sans-serif", 9).into_font().color(&BLACK),
                )))
                .unwrap();
        }
    }

    // Collect unique legend entries (preserving order)
    let mut seen = std::collections::HashSet::new();
    let legend_entries: Vec<(&str, RGBColor)> = groups
        .iter()
        .flat_map(|g| g.values.iter().map(|(_, _, name, color)| (*name, *color)))
        .filter(|(name, _)| seen.insert(*name))
        .collect();

    let legend_y = max_val * 0.97;
    let legend_x = n as f64 * 0.55;
    for (i, (name, color)) in legend_entries.iter().enumerate() {
        let y = legend_y - i as f64 * max_val * 0.05;
        chart
            .draw_series(std::iter::once(Rectangle::new(
                [(legend_x, y), (legend_x + 0.06, y + max_val * 0.025)],
                color.filled(),
            )))
            .unwrap();
        chart
            .draw_series(std::iter::once(Text::new(
                name.to_string(),
                (legend_x + 0.09, y + max_val * 0.012),
                ("sans-serif", 11).into_font().color(&BLACK),
            )))
            .unwrap();
    }

    root.present().unwrap();
}

/// Generate all benchmark charts as SVG files in the report directory.
pub fn generate_charts(results: &[RunMetrics], report_dir: &std::path::Path) {
    // Wall time chart — error bars are the 95% CI of the wall-time samples.
    let groups = extract_groups(
        results,
        |r| r.wall_time_ms(),
        |r| r.stats.as_ref().map(|s| s.wall_ms_ci95).unwrap_or(0.0),
    );
    draw_grouped_bar_chart(
        &report_dir.join("chart_wall_time.svg"),
        "Wall Time (lower is better; whiskers = 95% CI)",
        "Time (ms)",
        &groups,
    );

    // Peak RSS chart — the cross-engine-comparable memory basis.
    let groups = extract_groups(
        results,
        |r| r.peak_rss_mb(),
        |r| r.stats.as_ref().map(|s| s.rss_mb_ci95).unwrap_or(0.0),
    );
    draw_grouped_bar_chart(
        &report_dir.join("chart_peak_memory.svg"),
        "Peak RSS (lower is better; whiskers = 95% CI)",
        "Peak RSS (MB)",
        &groups,
    );

    // Engine-tracked working set — a libviprs-only, per-run figure kept in a
    // separate chart so it is never confused with the RSS basis above.
    let groups = extract_groups(results, |r| r.tracked_memory_mb(), no_error);
    draw_grouped_bar_chart(
        &report_dir.join("chart_tracked_memory.svg"),
        "Engine-Tracked Working Set (libviprs engines; lower is better)",
        "Tracked (MB)",
        &groups,
    );

    // Raw throughput chart
    let groups = extract_groups(results, |r| r.tiles_per_second(), no_error);
    draw_grouped_bar_chart(
        &report_dir.join("chart_throughput.svg"),
        "Raw Throughput (higher is better)",
        "Tiles/s",
        &groups,
    );

    // Memory-normalised throughput: tiles/s per MB
    let groups = extract_groups(results, |r| r.tiles_per_second_per_mb(), no_error);
    draw_grouped_bar_chart(
        &report_dir.join("chart_efficiency.svg"),
        "Memory Efficiency — Tiles/s per RSS-MB (higher is better)",
        "Tiles/s/RSS-MB",
        &groups,
    );

    // Resource cost: MB-seconds per tile (lower is better)
    let groups = extract_groups(results, |r| r.resource_cost_per_tile(), no_error);
    draw_grouped_bar_chart(
        &report_dir.join("chart_resource_cost.svg"),
        "Resource Cost — RSS-MB\u{00b7}s per Tile (lower is better)",
        "RSS-MB\u{00b7}s / tile",
        &groups,
    );
}

/// Build the executive verdict table.
///
/// For each `(w × h, concurrency)` configuration: which engine wins on wall
/// time, peak RSS, and memory efficiency, and every engine's ratio versus
/// libvips **in the same snapshot** (so the comparison is never against a
/// libvips number measured on a different day/machine). A ratio < 1 on wall
/// time / RSS means "faster / leaner than libvips"; > 1 on efficiency means
/// "more tiles/s/MB than libvips".
pub fn executive_verdict(results: &[RunMetrics]) -> String {
    use std::fmt::Write as _;
    let groups = grouped_results(results);
    let mut out = String::new();
    out.push_str("=== Executive verdict (per configuration) ===\n");
    out.push_str(
        "Ratios are vs libvips in the SAME snapshot. wall/RSS: <1 beats libvips; \
         eff: >1 beats libvips.\n\n",
    );
    out.push_str(&format!(
        "{:<16} {:<12} {:>12} {:>12} {:>12} {:>12} {:>12}\n",
        "Config", "Engine", "wall ms", "RSS MB", "eff", "wall/vips", "RSS/vips",
    ));
    out.push_str(&format!("{}\n", "-".repeat(92)));

    for group in &groups {
        let cfg = format!(
            "{}x{} c{}",
            group[0].width, group[0].height, group[0].concurrency
        );
        let vips = group.iter().find(|r| r.engine == "libvips");
        let vips_wall = vips.map(|v| v.wall_time_ms()).filter(|v| *v > 0.0);
        let vips_rss = vips.map(|v| v.peak_rss_mb()).filter(|v| *v > 0.0);

        // Winners on each axis.
        let win_wall = group
            .iter()
            .min_by(|a, b| a.wall_time_ms().partial_cmp(&b.wall_time_ms()).unwrap())
            .map(|r| r.engine.as_str())
            .unwrap_or("-");
        let win_rss = group
            .iter()
            .filter(|r| r.peak_rss_mb() > 0.0)
            .min_by(|a, b| a.peak_rss_mb().partial_cmp(&b.peak_rss_mb()).unwrap())
            .map(|r| r.engine.as_str())
            .unwrap_or("-");
        let win_eff = group
            .iter()
            .max_by(|a, b| {
                a.tiles_per_second_per_mb()
                    .partial_cmp(&b.tiles_per_second_per_mb())
                    .unwrap()
            })
            .map(|r| r.engine.as_str())
            .unwrap_or("-");

        for (i, r) in group.iter().enumerate() {
            let cfg_col = if i == 0 { cfg.as_str() } else { "" };
            let wall_ratio = vips_wall
                .map(|v| format!("{:.2}", r.wall_time_ms() / v))
                .unwrap_or_else(|| "-".to_string());
            let rss_ratio = match (vips_rss, r.peak_rss_mb() > 0.0) {
                (Some(v), true) => format!("{:.2}", r.peak_rss_mb() / v),
                _ => "-".to_string(),
            };
            let _ = writeln!(
                out,
                "{:<16} {:<12} {:>12.1} {:>12.2} {:>12.1} {:>12} {:>12}",
                cfg_col,
                r.engine,
                r.wall_time_ms(),
                r.peak_rss_mb(),
                r.tiles_per_second_per_mb(),
                wall_ratio,
                rss_ratio,
            );
        }
        let _ = writeln!(
            out,
            "  -> winners: wall={win_wall}  RSS={win_rss}  efficiency={win_eff}",
        );
        out.push('\n');
    }
    out
}

/// Print a summary comparing all engines.
///
/// Shows each engine's memory efficiency and resource cost side-by-side.
pub fn print_savings_summary(results: &[RunMetrics]) {
    let groups = grouped_results(results);

    println!();
    println!(
        "{:<16} {:<12} {:>10} {:>12} {:>10} {:>10} {:>10} {:>12}",
        "Config",
        "Engine",
        "Time (ms)",
        "Tracked MB",
        "RSS MB",
        "T/s",
        "T/s/RSS-MB",
        "RSS-MB\u{00b7}s/tile",
    );
    println!("{}", "-".repeat(98));

    for group in &groups {
        let config = format!(
            "{}x{} c{}",
            group[0].width, group[0].height, group[0].concurrency
        );
        for (i, r) in group.iter().enumerate() {
            let label = if i == 0 { &config } else { "" };
            println!(
                "{:<16} {:<12} {:>10.1} {:>12.2} {:>10.2} {:>10.0} {:>10.1} {:>12.4}",
                label,
                r.engine,
                r.wall_time_ms(),
                r.tracked_memory_mb(),
                r.peak_rss_mb(),
                r.tiles_per_second(),
                r.tiles_per_second_per_mb(),
                r.resource_cost_per_tile(),
            );
        }
        println!();
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;

    /// A unique scratch path under the OS temp dir, no external crate.
    fn scratch_path(tag: &str) -> std::path::PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("libviprs_bench_{tag}_{nanos}.json"))
    }

    #[test]
    fn missing_history_file_is_empty_not_error() {
        let path = scratch_path("missing");
        // Ensure it really does not exist.
        let _ = std::fs::remove_file(&path);
        let loaded = load_history(&path).expect("missing file must be Ok(empty)");
        assert!(loaded.is_empty());
    }

    #[test]
    fn valid_history_round_trips() {
        let path = scratch_path("valid");
        let history = vec![create_snapshot(Vec::new(), 256, 1_000_000)];
        save_history(&path, &history).expect("save must succeed");

        let loaded = load_history(&path).expect("valid file must parse");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].version, history[0].version);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_history_is_reported_and_left_intact() {
        let path = scratch_path("corrupt");
        // A prior run's accumulated history that happens to be corrupt.
        let corrupt = "[ { \"version\": \"0.3.1\", not-valid-json ]";
        std::fs::write(&path, corrupt).unwrap();

        // load_history must surface the error rather than silently
        // returning an empty vec (which the caller would then persist,
        // wiping the file).
        let result = load_history(&path);
        assert!(
            result.is_err(),
            "corrupt history must be an error, got {result:?}"
        );

        // And critically, load_history does not itself mutate the file:
        // the accumulated (if unreadable) history is preserved on disk
        // for repair instead of being discarded.
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, corrupt);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn label_normalization_maps_legacy_form() {
        assert_eq!(
            normalize_run_label("1024x1024 c0 monolithic"),
            "1024x1024_c0_monolithic"
        );
        // Already-current labels pass through unchanged.
        assert_eq!(normalize_run_label("512x512_c4_vips"), "512x512_c4_vips");
        // Collapses runs of whitespace.
        assert_eq!(normalize_run_label("2048x2048  c4  mr"), "2048x2048_c4_mr");
    }

    #[test]
    fn run_stats_summarize_samples() {
        // Wall samples 1..=9 ms, rss constant 10 MB.
        let samples: Vec<(f64, f64)> = (1..=9).map(|i| (i as f64, 10.0)).collect();
        let s = RunStats::from_samples(&samples);
        assert_eq!(s.n, 9);
        assert_eq!(s.wall_ms_median, 5.0);
        assert_eq!(s.wall_ms_min, 1.0);
        assert!((s.wall_ms_iqr - 4.0).abs() < 1e-9, "iqr {}", s.wall_ms_iqr);
        assert!(s.wall_ms_ci95 > 0.0);
        // Constant RSS → zero spread.
        assert_eq!(s.rss_mb_median, 10.0);
        assert_eq!(s.rss_mb_iqr, 0.0);
        assert_eq!(s.rss_mb_ci95, 0.0);
    }

    #[test]
    fn migrate_snapshot_is_idempotent_on_current_labels() {
        let mut snap = create_snapshot(Vec::new(), 256, 1_000_000);
        let before = snap.schema_version;
        migrate_snapshot(&mut snap);
        assert_eq!(snap.schema_version, before);
        assert_eq!(snap.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn snapshot_records_measured_core_version_not_the_harness() {
        // The recorded version must be the measured core crate's version
        // (from build.rs), not this bench harness's own CARGO_PKG_VERSION.
        // A regression to `env!("CARGO_PKG_VERSION")` would fail here
        // whenever core and bench versions differ (the real-world case).
        let snap = create_snapshot(Vec::new(), 256, 1_000_000);
        assert_eq!(snap.version, core_version());
        assert_eq!(snap.git_sha, core_git_sha());
        assert!(!snap.version.is_empty());
    }
}
