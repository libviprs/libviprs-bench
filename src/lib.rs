//! Shared utilities for libviprs benchmarks.
//!
//! Provides test raster generation, metric collection, and reporting
//! infrastructure used by both criterion benchmarks and standalone
//! profiling binaries.

use std::time::{Duration, Instant};

use libviprs::{
    CollectingObserver, EngineConfig, EngineEvent, Layout, MemorySink, PixelFormat,
    PyramidPlan, PyramidPlanner, Raster, RasterStripSource, StreamingConfig,
    generate_pyramid_observed, generate_pyramid_streaming,
};
use serde::Serialize;

/// Metrics collected from a single benchmark run.
#[derive(Debug, Clone, Serialize)]
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
    /// Peak tracked memory in bytes (raster buffers only).
    pub peak_memory_bytes: u64,
    /// Total tiles produced.
    pub tiles_produced: u64,
    /// Levels processed.
    pub levels_processed: u32,
    /// Tiles skipped (blank).
    pub tiles_skipped: u64,
    /// Number of strips (streaming only, 0 for monolithic).
    pub strips: u32,
    /// Concurrency level used.
    pub concurrency: usize,
}

impl RunMetrics {
    pub fn peak_memory_mb(&self) -> f64 {
        self.peak_memory_bytes as f64 / (1024.0 * 1024.0)
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

/// Run the monolithic engine and collect metrics.
pub fn bench_monolithic(
    src: &Raster,
    plan: &PyramidPlan,
    concurrency: usize,
    label: &str,
) -> RunMetrics {
    let sink = MemorySink::new();
    let observer = CollectingObserver::new();
    let config = EngineConfig::default().with_concurrency(concurrency);

    let start = Instant::now();
    let result = generate_pyramid_observed(src, plan, &sink, &config, &observer).unwrap();
    let wall_time = start.elapsed();

    RunMetrics {
        label: label.to_string(),
        width: src.width(),
        height: src.height(),
        engine: "monolithic".to_string(),
        wall_time,
        peak_memory_bytes: result.peak_memory_bytes,
        tiles_produced: result.tiles_produced,
        levels_processed: result.levels_processed,
        tiles_skipped: result.tiles_skipped,
        strips: 0,
        concurrency,
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
    let sink = MemorySink::new();
    let observer = CollectingObserver::new();
    let config = StreamingConfig {
        memory_budget_bytes,
        engine: EngineConfig::default().with_concurrency(concurrency),
    };

    let strip_src = RasterStripSource::new(src);
    let start = Instant::now();
    let result =
        generate_pyramid_streaming(&strip_src, plan, &sink, &config, &observer).unwrap();
    let wall_time = start.elapsed();

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
        peak_memory_bytes: result.peak_memory_bytes,
        tiles_produced: result.tiles_produced,
        levels_processed: result.levels_processed,
        tiles_skipped: result.tiles_skipped,
        strips,
        concurrency,
    }
}

/// Run both engines across a matrix of image sizes and concurrency levels.
pub fn comparison_suite(
    sizes: &[(u32, u32)],
    concurrency_levels: &[usize],
    tile_size: u32,
    streaming_budget_bytes: u64,
) -> Vec<RunMetrics> {
    let mut results = Vec::new();

    for &(w, h) in sizes {
        let src = gradient_raster(w, h);
        let planner = PyramidPlanner::new(w, h, tile_size, 0, Layout::DeepZoom).unwrap();
        let plan = planner.plan();

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
        }
    }

    results
}

/// Print a comparison table to stdout.
pub fn print_comparison_table(results: &[RunMetrics]) {
    println!(
        "{:<24} {:<12} {:>10} {:>12} {:>10} {:>10} {:>8}",
        "Label", "Engine", "Time (ms)", "Memory (MB)", "Tiles", "Tiles/s", "Strips"
    );
    println!("{}", "-".repeat(90));

    for r in results {
        println!(
            "{:<24} {:<12} {:>10.1} {:>12.2} {:>10} {:>10.0} {:>8}",
            r.label,
            r.engine,
            r.wall_time_ms(),
            r.peak_memory_mb(),
            r.tiles_produced,
            r.tiles_per_second(),
            r.strips,
        );
    }
}

/// Group results into pairs (monolithic, streaming) for the same config.
pub fn paired_comparisons(results: &[RunMetrics]) -> Vec<(&RunMetrics, &RunMetrics)> {
    let mut pairs = Vec::new();
    let mut i = 0;
    while i + 1 < results.len() {
        if results[i].engine == "monolithic" && results[i + 1].engine == "streaming" {
            pairs.push((&results[i], &results[i + 1]));
        }
        i += 2;
    }
    pairs
}

/// Print a summary comparing paired results.
pub fn print_savings_summary(results: &[RunMetrics]) {
    let pairs = paired_comparisons(results);

    println!();
    println!(
        "{:<20} {:>14} {:>14} {:>10} {:>14} {:>14} {:>10}",
        "Config", "Mono Mem(MB)", "Stream Mem(MB)", "Mem Saved", "Mono Time(ms)", "Stream Time(ms)", "Speedup"
    );
    println!("{}", "-".repeat(100));

    for (mono, stream) in pairs {
        let mem_ratio = 1.0 - (stream.peak_memory_mb() / mono.peak_memory_mb());
        let time_ratio = mono.wall_time_ms() / stream.wall_time_ms();
        let config = format!("{}x{} c{}", mono.width, mono.height, mono.concurrency);

        println!(
            "{:<20} {:>14.2} {:>14.2} {:>9.0}% {:>14.1} {:>14.1} {:>9.2}x",
            config,
            mono.peak_memory_mb(),
            stream.peak_memory_mb(),
            mem_ratio * 100.0,
            mono.wall_time_ms(),
            stream.wall_time_ms(),
            time_ratio,
        );
    }
}
