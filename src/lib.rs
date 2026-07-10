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

/// The single tile codec used on **both** sides of the cross-engine
/// comparison. The libviprs engines encode their tiles in this format via
/// [`FsSink`], and libvips `dzsave` is invoked with the matching `--suffix`,
/// so the codec is never a hidden variable between the two engines (issue
/// #153). Keep [`BENCH_TILE_FORMAT`] and [`BENCH_TILE_SUFFIX`] in lockstep.
pub const BENCH_TILE_FORMAT: TileFormat = TileFormat::Png;
/// dzsave `--suffix` (and file extension) matching [`BENCH_TILE_FORMAT`].
pub const BENCH_TILE_SUFFIX: &str = ".png";

/// A snapshot of benchmark results for a specific libviprs version.
///
/// Stored in `report/benchmark_history.json` so that performance can be
/// tracked across releases. Each run appends one entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkSnapshot {
    /// libviprs version (from Cargo.toml).
    pub version: String,
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
    /// Wall-clock time for pyramid generation.
    pub wall_time: Duration,
    /// Peak engine-tracked working set in bytes (raster buffers the engine
    /// accounts for during the run). This is a per-run figure, reset for each
    /// engine, and is available only for the libviprs engines. libvips does
    /// not expose an equivalent internal counter, so it is reported as `0`
    /// there — the two figures are kept in **separate** columns rather than
    /// being compared against each other (issue #153).
    pub tracked_memory_bytes: u64,
    /// Peak resident set size (RSS) in bytes — the OS-level high-water mark.
    /// This is measured the same way for every engine (via `getrusage` for
    /// in-process engines, `/usr/bin/time` for the libvips CLI child), so it
    /// is the one memory basis that is directly comparable across libviprs
    /// and libvips. See [`RunMetrics::peak_rss_mb`] for the shared-process
    /// caveat.
    pub peak_rss_bytes: u64,
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
    let _ = std::fs::remove_dir_all(&out_dir);

    RunMetrics {
        label: label.to_string(),
        width: src.width(),
        height: src.height(),
        engine: "monolithic".to_string(),
        wall_time,
        tracked_memory_bytes: result.peak_memory_bytes,
        peak_rss_bytes,
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
        wall_time,
        tracked_memory_bytes: result.peak_memory_bytes,
        peak_rss_bytes,
        tiles_produced: result.tiles_produced,
        levels_processed: result.levels_processed,
        tiles_skipped: result.tiles_skipped,
        strips,
        batches: 0,
        inflight_strips: 0,
        concurrency,
        memory_budget_bytes: memory_budget_bytes,
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
        wall_time,
        tracked_memory_bytes: result.peak_memory_bytes,
        peak_rss_bytes,
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

/// Write a Raster to a temporary PNG file for libvips benchmarking.
///
/// Returns the path to the temp file. The caller is responsible for cleanup.
pub fn write_temp_png(src: &Raster) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("libviprs-bench");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("bench_{}x{}.png", src.width(), src.height()));

    let file = std::fs::File::create(&path).unwrap();
    let w = std::io::BufWriter::new(file);
    let encoder = image::codecs::png::PngEncoder::new(w);
    image::ImageEncoder::write_image(
        encoder,
        src.data(),
        src.width(),
        src.height(),
        image::ColorType::Rgb8.into(),
    )
    .unwrap();

    path
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

    // Count output tiles
    let tiles_dir = out_dir.join("pyramid_files");
    let tiles_produced = if tiles_dir.exists() {
        walkdir(&tiles_dir)
    } else {
        0
    };

    // Count levels (subdirectories)
    let levels_processed = if tiles_dir.exists() {
        std::fs::read_dir(&tiles_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .count() as u32
            })
            .unwrap_or(0)
    } else {
        0
    };

    // Cleanup
    let _ = std::fs::remove_dir_all(&out_dir);

    Some(RunMetrics {
        label: label.to_string(),
        width,
        height,
        engine: "libvips".to_string(),
        wall_time,
        // libvips exposes no engine-internal working-set counter, so the
        // tracked column is left at 0 and only the RSS column is populated.
        tracked_memory_bytes: 0,
        peak_rss_bytes: peak_memory_bytes,
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

    // Count tiles
    let tiles_dir = out_dir.join("pyramid_files");
    let tiles_produced = if tiles_dir.exists() {
        walkdir(&tiles_dir)
    } else {
        0
    };

    let levels_processed = if tiles_dir.exists() {
        std::fs::read_dir(&tiles_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .count() as u32
            })
            .unwrap_or(0)
    } else {
        0
    };

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
        wall_time,
        tracked_memory_bytes: 0,
        peak_rss_bytes,
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

/// Get current process peak RSS in bytes.
fn get_peak_rss() -> u64 {
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

/// Recursively count files in a directory.
fn walkdir(dir: &std::path::Path) -> u64 {
    let mut count = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                count += walkdir(&path);
            } else {
                count += 1;
            }
        }
    }
    count
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

            // libvips: prefer in-process FFI when available, fall back to CLI
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

/// Load benchmark history from disk, or return an empty vec.
pub fn load_history(path: &std::path::Path) -> Vec<BenchmarkSnapshot> {
    match std::fs::read_to_string(path) {
        Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Append a snapshot to the history file.
pub fn save_history(path: &std::path::Path, history: &[BenchmarkSnapshot]) {
    let json = serde_json::to_string_pretty(history).unwrap();
    std::fs::write(path, json).unwrap();
}

/// Create a `BenchmarkSnapshot` from current run metrics.
pub fn create_snapshot(
    runs: Vec<RunMetrics>,
    tile_size: u32,
    memory_budget_bytes: u64,
) -> BenchmarkSnapshot {
    BenchmarkSnapshot {
        version: env!("CARGO_PKG_VERSION").to_string(),
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        tile_size,
        memory_budget_bytes,
        runs,
    }
}

// ---------------------------------------------------------------------------
// SVG chart generation via plotters
// ---------------------------------------------------------------------------

use plotters::prelude::*;

/// Color palette for the four engines.
const COLOR_VIPS: RGBColor = RGBColor(156, 39, 176); // purple — libvips
const COLOR_MONO: RGBColor = RGBColor(66, 133, 244); // blue   — monolithic
const COLOR_STREAM: RGBColor = RGBColor(52, 168, 83); // green  — streaming
const COLOR_MR: RGBColor = RGBColor(234, 67, 53); // red    — mapreduce

/// Grouped bar chart data.
struct ChartGroup {
    label: String,
    values: Vec<(f64, &'static str, RGBColor)>,
}

/// Extract chart groups from results. Groups by (width, height, concurrency).
/// Each group contains one bar per engine found.
fn extract_groups(results: &[RunMetrics], metric: fn(&RunMetrics) -> f64) -> Vec<ChartGroup> {
    // Group results by config key
    let mut map: std::collections::BTreeMap<String, Vec<&RunMetrics>> =
        std::collections::BTreeMap::new();
    for r in results {
        let key = format!("{}x{}_c{}", r.width, r.height, r.concurrency);
        map.entry(key).or_default().push(r);
    }

    map.into_iter()
        .map(|(_, runs)| {
            let first = runs[0];
            let label = format!("{}x{}\nc{}", first.width, first.height, first.concurrency);
            let values: Vec<(f64, &'static str, RGBColor)> = runs
                .iter()
                .filter_map(|r| {
                    let (name, color) = match r.engine.as_str() {
                        "libvips" => ("libvips", COLOR_VIPS),
                        "monolithic" => ("Monolithic", COLOR_MONO),
                        "streaming" => ("Streaming", COLOR_STREAM),
                        "mapreduce" => ("MapReduce", COLOR_MR),
                        _ => return None,
                    };
                    Some((metric(r), name, color))
                })
                .collect();
            ChartGroup { label, values }
        })
        .collect()
}

fn draw_grouped_bar_chart(
    path: &std::path::Path,
    title: &str,
    y_label: &str,
    groups: &[ChartGroup],
) {
    let max_val = groups
        .iter()
        .flat_map(|g| g.values.iter().map(|(v, _, _)| *v))
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
        for (j, (val, _name, color)) in group.values.iter().enumerate() {
            let bx = x + gap + j as f64 * (bar_w + gap);
            chart
                .draw_series(std::iter::once(Rectangle::new(
                    [(bx, 0.0), (bx + bar_w, *val)],
                    color.filled(),
                )))
                .unwrap();

            // Value label above bar
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
                    (bx + bar_w / 2.0, val + max_val * 0.01),
                    ("sans-serif", 9).into_font().color(&BLACK),
                )))
                .unwrap();
        }
    }

    // Collect unique legend entries (preserving order)
    let mut seen = std::collections::HashSet::new();
    let legend_entries: Vec<(&str, RGBColor)> = groups
        .iter()
        .flat_map(|g| g.values.iter().map(|(_, name, color)| (*name, *color)))
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
    // Wall time chart
    let groups = extract_groups(results, |r| r.wall_time_ms());
    draw_grouped_bar_chart(
        &report_dir.join("chart_wall_time.svg"),
        "Wall Time (lower is better)",
        "Time (ms)",
        &groups,
    );

    // Peak RSS chart — the cross-engine-comparable memory basis.
    let groups = extract_groups(results, |r| r.peak_rss_mb());
    draw_grouped_bar_chart(
        &report_dir.join("chart_peak_memory.svg"),
        "Peak RSS (lower is better)",
        "Peak RSS (MB)",
        &groups,
    );

    // Engine-tracked working set — a libviprs-only, per-run figure kept in a
    // separate chart so it is never confused with the RSS basis above.
    let groups = extract_groups(results, |r| r.tracked_memory_mb());
    draw_grouped_bar_chart(
        &report_dir.join("chart_tracked_memory.svg"),
        "Engine-Tracked Working Set (libviprs engines; lower is better)",
        "Tracked (MB)",
        &groups,
    );

    // Raw throughput chart
    let groups = extract_groups(results, |r| r.tiles_per_second());
    draw_grouped_bar_chart(
        &report_dir.join("chart_throughput.svg"),
        "Raw Throughput (higher is better)",
        "Tiles/s",
        &groups,
    );

    // Memory-normalised throughput: tiles/s per MB
    let groups = extract_groups(results, |r| r.tiles_per_second_per_mb());
    draw_grouped_bar_chart(
        &report_dir.join("chart_efficiency.svg"),
        "Memory Efficiency — Tiles/s per RSS-MB (higher is better)",
        "Tiles/s/RSS-MB",
        &groups,
    );

    // Resource cost: MB-seconds per tile (lower is better)
    let groups = extract_groups(results, |r| r.resource_cost_per_tile());
    draw_grouped_bar_chart(
        &report_dir.join("chart_resource_cost.svg"),
        "Resource Cost — RSS-MB\u{00b7}s per Tile (lower is better)",
        "RSS-MB\u{00b7}s / tile",
        &groups,
    );
}

/// Generate a version history line chart showing a metric across releases.
pub fn generate_history_chart(
    history: &[BenchmarkSnapshot],
    report_dir: &std::path::Path,
    image_size: (u32, u32),
    concurrency: usize,
) {
    if history.len() < 2 {
        return; // Need at least 2 data points for a trend
    }

    let (w, h) = image_size;
    let filter_label_prefix = format!("{w}x{h}_c{concurrency}");

    // Extract time series per engine
    struct Point {
        version: String,
        wall_time_ms: f64,
        peak_memory_mb: f64,
    }

    let mut vips_pts: Vec<Point> = Vec::new();
    let mut mono_pts: Vec<Point> = Vec::new();
    let mut stream_pts: Vec<Point> = Vec::new();
    let mut mr_pts: Vec<Point> = Vec::new();

    for snap in history {
        for run in &snap.runs {
            if !run.label.starts_with(&filter_label_prefix) {
                continue;
            }
            let pt = Point {
                version: snap.version.clone(),
                wall_time_ms: run.wall_time_ms(),
                peak_memory_mb: run.peak_rss_mb(),
            };
            match run.engine.as_str() {
                "libvips" => vips_pts.push(pt),
                "monolithic" => mono_pts.push(pt),
                "streaming" => stream_pts.push(pt),
                "mapreduce" => mr_pts.push(pt),
                _ => {}
            }
        }
    }

    if mono_pts.is_empty() {
        return;
    }

    let all_times: Vec<f64> = vips_pts
        .iter()
        .chain(mono_pts.iter())
        .chain(stream_pts.iter())
        .chain(mr_pts.iter())
        .map(|p| p.wall_time_ms)
        .collect();
    let max_time = all_times.iter().cloned().fold(0.0f64, f64::max) * 1.1;
    let n = mono_pts.len();

    let chart_w = 140 + n as u32 * 80;
    let chart_h = 380;
    let path = report_dir.join(format!("chart_history_{w}x{h}_c{concurrency}_time.svg"));

    let root = SVGBackend::new(&path, (chart_w, chart_h)).into_drawing_area();
    root.fill(&WHITE).unwrap();

    let mut chart = ChartBuilder::on(&root)
        .caption(
            format!("Wall Time History — {w}x{h} c{concurrency}"),
            ("sans-serif", 16).into_font(),
        )
        .margin(10)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(0..n.max(1), 0.0..max_time)
        .unwrap();

    chart
        .configure_mesh()
        .x_labels(n)
        .x_label_formatter(&|x| {
            mono_pts
                .get(*x)
                .map(|p| p.version.clone())
                .unwrap_or_default()
        })
        .y_desc("Time (ms)")
        .draw()
        .unwrap();

    // Draw lines for each engine
    let mut draw_line = |pts: &[Point], color: RGBColor, name: &str| {
        let series: Vec<(usize, f64)> = pts
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.wall_time_ms))
            .collect();
        if !series.is_empty() {
            chart
                .draw_series(LineSeries::new(series.clone(), color.stroke_width(2)))
                .unwrap()
                .label(name)
                .legend(move |(x, y)| {
                    Rectangle::new([(x, y - 5), (x + 15, y + 5)], color.filled())
                });
            chart
                .draw_series(
                    series
                        .iter()
                        .map(|&(x, y)| Circle::new((x, y), 3, color.filled())),
                )
                .unwrap();
        }
    };

    draw_line(&vips_pts, COLOR_VIPS, "libvips");
    draw_line(&mono_pts, COLOR_MONO, "Monolithic");
    draw_line(&stream_pts, COLOR_STREAM, "Streaming");
    draw_line(&mr_pts, COLOR_MR, "MapReduce");

    chart
        .configure_series_labels()
        .background_style(WHITE.mix(0.8))
        .border_style(BLACK.mix(0.3))
        .draw()
        .unwrap();

    root.present().unwrap();

    // Also generate memory history
    let all_mem: Vec<f64> = vips_pts
        .iter()
        .chain(mono_pts.iter())
        .chain(stream_pts.iter())
        .chain(mr_pts.iter())
        .map(|p| p.peak_memory_mb)
        .collect();
    let max_mem = all_mem.iter().cloned().fold(0.0f64, f64::max) * 1.1;

    let mem_path = report_dir.join(format!("chart_history_{w}x{h}_c{concurrency}_memory.svg"));
    let root = SVGBackend::new(&mem_path, (chart_w, chart_h)).into_drawing_area();
    root.fill(&WHITE).unwrap();

    let mut chart = ChartBuilder::on(&root)
        .caption(
            format!("Peak RSS History — {w}x{h} c{concurrency}"),
            ("sans-serif", 16).into_font(),
        )
        .margin(10)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(0..n.max(1), 0.0..max_mem)
        .unwrap();

    chart
        .configure_mesh()
        .x_labels(n)
        .x_label_formatter(&|x| {
            mono_pts
                .get(*x)
                .map(|p| p.version.clone())
                .unwrap_or_default()
        })
        .y_desc("Memory (MB)")
        .draw()
        .unwrap();

    let mut draw_mem_line = |pts: &[Point], color: RGBColor, name: &str| {
        let series: Vec<(usize, f64)> = pts
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.peak_memory_mb))
            .collect();
        if !series.is_empty() {
            chart
                .draw_series(LineSeries::new(series.clone(), color.stroke_width(2)))
                .unwrap()
                .label(name)
                .legend(move |(x, y)| {
                    Rectangle::new([(x, y - 5), (x + 15, y + 5)], color.filled())
                });
            chart
                .draw_series(
                    series
                        .iter()
                        .map(|&(x, y)| Circle::new((x, y), 3, color.filled())),
                )
                .unwrap();
        }
    };

    draw_mem_line(&vips_pts, COLOR_VIPS, "libvips");
    draw_mem_line(&mono_pts, COLOR_MONO, "Monolithic");
    draw_mem_line(&stream_pts, COLOR_STREAM, "Streaming");
    draw_mem_line(&mr_pts, COLOR_MR, "MapReduce");

    chart
        .configure_series_labels()
        .background_style(WHITE.mix(0.8))
        .border_style(BLACK.mix(0.3))
        .draw()
        .unwrap();

    root.present().unwrap();
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
