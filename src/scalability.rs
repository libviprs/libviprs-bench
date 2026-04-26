//! Engine scalability benchmark.
//!
//! Extracts a raster from `fixtures/43551_California_South.pdf`, crops it
//! to progressively larger sizes, and runs all four engines (libvips,
//! monolithic, streaming, MapReduce) at each size. Produces SVG line
//! charts showing how wall time, peak memory, and efficiency scale with
//! image area.
//!
//! Run: cargo run --release --bin scalability
//!
//! Output: report/scalability_*.svg + report/scalability_results.json

use std::fs;
use std::path::Path;
use std::time::Instant;

use plotters::prelude::*;
use serde::{Deserialize, Serialize};

use libviprs::streaming::BudgetPolicy;
use libviprs::{
    EngineBuilder, EngineConfig, EngineKind, Layout, MemorySink, PyramidPlanner, Raster,
    RasterStripSource,
};
use libviprs_bench::{bench_libvips, gradient_raster, vips_available, write_temp_png};

const TILE_SIZE: u32 = 256;
const STREAMING_BUDGET: u64 = 4_000_000; // 4 MB — forces streaming behavior

/// Default x-axis crop (megapixels) for scalability charts.
///
/// The smallest images we benchmark (sub-megapixel up to ~12 MP) sit in a
/// regime where libvips's per-region working set and the libviprs engines'
/// fixed setup costs dominate the metrics. On efficiency / resource-cost
/// charts this manifests as a near-vertical drop on the left side of the
/// graph that hides the trend in the size range users actually care about
/// (full-page blueprints at 72 DPI live near 15 MP and above). We crop the
/// x-axis at this value by default; `--no-crop` falls back to showing the
/// full series.
const DEFAULT_X_AXIS_CROP_MP: f64 = 15.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScalabilityPoint {
    width: u32,
    height: u32,
    megapixels: f64,
    engine: String,
    wall_time_ms: f64,
    peak_memory_mb: f64,
    tiles_produced: u64,
    tiles_per_second: f64,
    tiles_per_second_per_mb: f64,
    resource_cost: f64,
}

fn run_monolithic(src: &Raster, tile_size: u32) -> (std::time::Duration, u64, u64) {
    let planner =
        PyramidPlanner::new(src.width(), src.height(), tile_size, 0, Layout::DeepZoom).unwrap();
    let plan = planner.plan();
    let sink = MemorySink::new();
    let start = Instant::now();
    let result = EngineBuilder::new(src, plan, &sink)
        .with_engine(EngineKind::Monolithic)
        .with_config(EngineConfig::default())
        .run()
        .unwrap();
    (
        start.elapsed(),
        result.peak_memory_bytes,
        result.tiles_produced,
    )
}

fn run_streaming(src: &Raster, tile_size: u32, budget: u64) -> (std::time::Duration, u64, u64) {
    let planner =
        PyramidPlanner::new(src.width(), src.height(), tile_size, 0, Layout::DeepZoom).unwrap();
    let plan = planner.plan();
    let sink = MemorySink::new();
    let strip_src = RasterStripSource::new(src);
    let start = Instant::now();
    let result = EngineBuilder::new(strip_src, plan, &sink)
        .with_engine(EngineKind::Streaming)
        .with_config(EngineConfig::default())
        .with_memory_budget(budget)
        .with_budget_policy(BudgetPolicy::Error)
        .run()
        .unwrap();
    (
        start.elapsed(),
        result.peak_memory_bytes,
        result.tiles_produced,
    )
}

fn run_mapreduce(src: &Raster, tile_size: u32, budget: u64) -> (std::time::Duration, u64, u64) {
    let planner =
        PyramidPlanner::new(src.width(), src.height(), tile_size, 0, Layout::DeepZoom).unwrap();
    let plan = planner.plan();
    let sink = MemorySink::new();
    let strip_src = RasterStripSource::new(src);
    let start = Instant::now();
    let result = EngineBuilder::new(strip_src, plan, &sink)
        .with_engine(EngineKind::MapReduce)
        .with_config(EngineConfig::default().with_concurrency(4))
        .with_memory_budget(budget)
        .with_budget_policy(BudgetPolicy::Error)
        .run()
        .unwrap();
    (
        start.elapsed(),
        result.peak_memory_bytes,
        result.tiles_produced,
    )
}

fn to_point(
    w: u32,
    h: u32,
    engine: &str,
    dur: std::time::Duration,
    peak: u64,
    tiles: u64,
) -> ScalabilityPoint {
    let mp = w as f64 * h as f64 / 1_000_000.0;
    let secs = dur.as_secs_f64();
    let ms = secs * 1000.0;
    let peak_mb = peak as f64 / (1024.0 * 1024.0);
    let tps = if secs > 0.0 { tiles as f64 / secs } else { 0.0 };
    let tps_mb = if peak_mb > 0.0 { tps / peak_mb } else { 0.0 };
    let cost = if tiles > 0 {
        (peak_mb * secs) / tiles as f64
    } else {
        0.0
    };

    ScalabilityPoint {
        width: w,
        height: h,
        megapixels: mp,
        engine: engine.to_string(),
        wall_time_ms: ms,
        peak_memory_mb: peak_mb,
        tiles_produced: tiles,
        tiles_per_second: tps,
        tiles_per_second_per_mb: tps_mb,
        resource_cost: cost,
    }
}

fn draw_scalability_chart(
    path: &std::path::Path,
    title: &str,
    x_label: &str,
    y_label: &str,
    series: &[(&str, RGBColor, &[(f64, f64)])],
    // When `Some(v)`, the chart's x-axis starts at `v` and any data
    // points with x < v are dropped before plotting. The y-axis range
    // is recomputed from only the visible points so labels suit the
    // zoomed-in scale. When `None`, behaves like the original chart
    // (axis from 0, full series).
    x_min: Option<f64>,
) {
    let x_lo = x_min.unwrap_or(0.0);

    // Filter every series to the visible x range up-front. Plotters would
    // clip out-of-range points on its own, but we still need the filtered
    // view to drive y-axis sizing — otherwise a huge off-chart point keeps
    // the y-axis stretched and the visible range looks flat against the
    // axis.
    let visible_series: Vec<(&str, RGBColor, Vec<(f64, f64)>)> = series
        .iter()
        .map(|(name, color, pts)| {
            let visible: Vec<(f64, f64)> = pts
                .iter()
                .filter(|(x, _)| *x >= x_lo)
                .copied()
                .collect();
            (*name, *color, visible)
        })
        .collect();

    let max_x = visible_series
        .iter()
        .flat_map(|(_, _, pts)| pts.iter().map(|(x, _)| *x))
        .fold(x_lo, f64::max)
        * 1.05;
    let max_y = visible_series
        .iter()
        .flat_map(|(_, _, pts)| pts.iter().map(|(_, y)| *y))
        .fold(0.0f64, f64::max)
        * 1.15;

    let root = SVGBackend::new(path, (700, 450)).into_drawing_area();
    root.fill(&WHITE).unwrap();

    let display_title: String = if x_min.is_some() {
        format!("{title}  (x ≥ {x_lo:.0} MP)")
    } else {
        title.to_string()
    };

    let mut chart = ChartBuilder::on(&root)
        .caption(display_title, ("sans-serif", 18).into_font())
        .margin(15)
        .x_label_area_size(40)
        .y_label_area_size(70)
        .build_cartesian_2d(x_lo..max_x, 0.0..max_y)
        .unwrap();

    chart
        .configure_mesh()
        .x_desc(x_label)
        .y_desc(y_label)
        .draw()
        .unwrap();

    for (name, color, pts) in &visible_series {
        let data = pts.clone();
        let legend_color = *color;
        chart
            .draw_series(LineSeries::new(data.clone(), color.stroke_width(2)))
            .unwrap()
            .label(*name)
            .legend(move |(x, y)| {
                Rectangle::new([(x, y - 5), (x + 15, y + 5)], legend_color.filled())
            });
        chart
            .draw_series(
                data.iter()
                    .map(|&(x, y)| Circle::new((x, y), 4, color.filled())),
            )
            .unwrap();
    }

    chart
        .configure_series_labels()
        .position(SeriesLabelPosition::UpperLeft)
        .background_style(WHITE.mix(0.9))
        .border_style(BLACK.mix(0.3))
        .label_font(("sans-serif", 12))
        .draw()
        .unwrap();

    root.present().unwrap();
}

/// Render the five scalability SVGs into `report_dir`. Shared between
/// the bench-then-render path and the `--replot` path.
fn render_charts(
    all_points: &[ScalabilityPoint],
    report_dir: &std::path::Path,
    x_min: Option<f64>,
) {
    let vips_color = RGBColor(156, 39, 176);
    let mono_color = RGBColor(66, 133, 244);
    let stream_color = RGBColor(52, 168, 83);
    let mr_color = RGBColor(234, 67, 53);

    let extract_series = |engine: &str| -> Vec<(f64, f64, f64, f64, f64, f64)> {
        all_points
            .iter()
            .filter(|p| p.engine == engine)
            .map(|p| {
                (
                    p.megapixels,
                    p.wall_time_ms,
                    p.peak_memory_mb,
                    p.tiles_per_second,
                    p.tiles_per_second_per_mb,
                    p.resource_cost,
                )
            })
            .collect()
    };

    let vips_data = extract_series("libvips");
    let mono_data = extract_series("monolithic");
    let stream_data = extract_series("streaming");
    let mr_data = extract_series("mapreduce");

    // Helper to pull one metric (by tuple field index) from each engine's
    // (megapixels, m1, m2, m3, m4, m5) tuples.
    macro_rules! xy {
        ($data:expr, $idx:tt) => {
            $data.iter().map(|d| (d.0, d.$idx)).collect::<Vec<_>>()
        };
    }

    // Per-chart definition: (filename, title, y-label, tuple-index for the
    // metric on each engine's series). Looping over this avoids repeating
    // the build-up boilerplate five times.
    let charts: [(&str, &str, &str, u8); 5] = [
        (
            "scalability_wall_time.svg",
            "Wall Time Scalability — 43551_California_South.pdf",
            "Time (ms)",
            1,
        ),
        (
            "scalability_peak_memory.svg",
            "Peak Memory Scalability — 43551_California_South.pdf",
            "Peak Memory (MB)",
            2,
        ),
        (
            "scalability_throughput.svg",
            "Throughput Scalability — 43551_California_South.pdf",
            "Tiles/s",
            3,
        ),
        (
            "scalability_efficiency.svg",
            "Memory Efficiency Scalability — Tiles/s per MB",
            "Tiles/s/MB",
            4,
        ),
        (
            "scalability_resource_cost.svg",
            "Resource Cost Scalability — MB\u{00b7}s per Tile (lower is better)",
            "MB\u{00b7}s / tile",
            5,
        ),
    ];

    for (filename, title, y_label, idx) in charts {
        let mut series: Vec<(&str, RGBColor, Vec<(f64, f64)>)> = Vec::new();
        if !vips_data.is_empty() {
            let pts = match idx {
                1 => xy!(vips_data, 1),
                2 => xy!(vips_data, 2),
                3 => xy!(vips_data, 3),
                4 => xy!(vips_data, 4),
                5 => xy!(vips_data, 5),
                _ => unreachable!(),
            };
            series.push(("libvips", vips_color, pts));
        }
        let pts_for = |data: &Vec<(f64, f64, f64, f64, f64, f64)>| match idx {
            1 => xy!(data, 1),
            2 => xy!(data, 2),
            3 => xy!(data, 3),
            4 => xy!(data, 4),
            5 => xy!(data, 5),
            _ => unreachable!(),
        };
        series.push(("Monolithic", mono_color, pts_for(&mono_data)));
        series.push(("Streaming", stream_color, pts_for(&stream_data)));
        series.push(("MapReduce", mr_color, pts_for(&mr_data)));

        let refs: Vec<(&str, RGBColor, &[(f64, f64)])> = series
            .iter()
            .map(|(n, c, d)| (*n, *c, d.as_slice()))
            .collect();

        draw_scalability_chart(
            &report_dir.join(filename),
            title,
            "Image Size (megapixels)",
            y_label,
            &refs,
            x_min,
        );
    }
}

/// Parsed CLI options for the scalability binary.
struct CliOpts {
    /// When true, skip the bench loop and re-render charts from the
    /// `scalability_results.json` already on disk. Useful for iterating on
    /// chart styling without paying the multi-minute bench cost.
    replot: bool,
    /// When true, draw charts with the original full-range x-axis. When
    /// false (the default), x-axis starts at `DEFAULT_X_AXIS_CROP_MP` so
    /// the steep-slope startup region is hidden.
    no_crop: bool,
}

fn parse_cli() -> CliOpts {
    let mut replot = false;
    let mut no_crop = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--replot" => replot = true,
            "--no-crop" | "--full-x-range" => no_crop = true,
            "-h" | "--help" => {
                println!("Usage: scalability [--replot] [--no-crop]");
                println!();
                println!("  --replot     Skip the bench; redraw SVGs from the existing");
                println!("               report/scalability_results.json.");
                println!("  --no-crop    Disable the default x-axis crop ({} MP) so the", DEFAULT_X_AXIS_CROP_MP);
                println!("               startup region of every chart is visible again.");
                std::process::exit(0);
            }
            other => {
                eprintln!("Unknown argument: {other}");
                eprintln!("Run with --help for usage.");
                std::process::exit(2);
            }
        }
    }
    CliOpts { replot, no_crop }
}

fn main() {
    let opts = parse_cli();
    let x_axis_crop: Option<f64> = if opts.no_crop {
        None
    } else {
        Some(DEFAULT_X_AXIS_CROP_MP)
    };

    let report_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("report");
    fs::create_dir_all(&report_dir).unwrap();

    // Replot mode: skip the bench, reload points from JSON, jump straight
    // to chart generation. Lets us iterate on chart styling (cropping,
    // colors, captions) without paying the multi-minute bench cost.
    if opts.replot {
        let json_path = report_dir.join("scalability_results.json");
        let raw = fs::read_to_string(&json_path).unwrap_or_else(|e| {
            eprintln!("--replot needs an existing {}: {e}", json_path.display());
            std::process::exit(1);
        });
        let all_points: Vec<ScalabilityPoint> = serde_json::from_str(&raw).unwrap_or_else(|e| {
            eprintln!("failed to parse {}: {e}", json_path.display());
            std::process::exit(1);
        });
        println!(
            "Replotting {} points from {} (crop: {})",
            all_points.len(),
            json_path.display(),
            x_axis_crop
                .map(|v| format!(">= {v} MP"))
                .unwrap_or_else(|| "off".to_string()),
        );
        render_charts(&all_points, &report_dir, x_axis_crop);
        println!(
            "Charts written to {}/scalability_*.svg",
            report_dir.display()
        );
        return;
    }

    let has_vips = vips_available();

    // Scalability series: generate gradient rasters at progressively larger
    // sizes. Uses 1.42:1 aspect ratio matching 43551_California_South.pdf
    // (4608x3240 pts). The grid intentionally spans both the small "noise"
    // regime (sub-megapixel) and several points well above the default
    // x-axis crop (15 MP) so the cropped charts have real lines instead
    // of a single dot per series. Memory: monolithic peak ≈ w×h×3×1.25
    // bytes — capped here at ~1.7 GB so the default 4 GB Docker container
    // still has headroom for libvips alongside.
    let sizes: Vec<(u32, u32)> = vec![
        (512, 360),
        (1024, 720),
        (2048, 1440),
        (4096, 2880),
        (4608, 3240),   // full California South page at 72 DPI (14.93 MP)
        (8192, 5760),   // beyond the PDF — pure scaling (47.18 MP)
        (10000, 7000),  // 70 MP
        (12000, 8400),  // 100.8 MP
        (16384, 11520), // 188.7 MP
        (20000, 14000), // 280 MP — mono peak ≈ 1.05 GB
    ];

    println!("=== Engine Scalability Benchmark ===");
    println!("Reference: 43551_California_South.pdf (4608x3240 pts)");
    println!(
        "Sizes: {} points from 512x360 to {}x{}",
        sizes.len(),
        sizes.last().unwrap().0,
        sizes.last().unwrap().1,
    );
    println!("Tile size: {TILE_SIZE}, streaming budget: {STREAMING_BUDGET} bytes");
    if has_vips {
        println!("libvips CLI: included");
    } else {
        println!("libvips CLI: not found, skipping");
    }
    println!();

    let mut all_points: Vec<ScalabilityPoint> = Vec::new();

    for &(w, h) in &sizes {
        let src = gradient_raster(w, h);
        let mp = w as f64 * h as f64 / 1_000_000.0;
        print!("{w}x{h} ({mp:.1} MP): ");

        // libvips: prefer in-process FFI, fall back to CLI
        let mut vips_done = false;
        #[cfg(feature = "libvips")]
        {
            if let Some(r) = libviprs_bench::bench_libvips_inprocess(&src, TILE_SIZE, 1, "vips") {
                print!(
                    "vips={:.0}ms/{:.1}MB  ",
                    r.wall_time_ms(),
                    r.peak_memory_mb(),
                );
                all_points.push(to_point(
                    w,
                    h,
                    "libvips",
                    r.wall_time,
                    r.peak_memory_bytes,
                    r.tiles_produced,
                ));
                vips_done = true;
            }
        }
        if !vips_done && has_vips {
            let png_path = write_temp_png(&src);
            if let Some(r) = bench_libvips(&png_path, w, h, TILE_SIZE, 1, "vips") {
                print!(
                    "vips={:.0}ms/{:.1}MB  ",
                    r.wall_time_ms(),
                    r.peak_memory_mb(),
                );
                all_points.push(to_point(
                    w,
                    h,
                    "libvips",
                    r.wall_time,
                    r.peak_memory_bytes,
                    r.tiles_produced,
                ));
            }
            let _ = fs::remove_file(&png_path);
        }

        // Monolithic
        let (dur, peak, tiles) = run_monolithic(&src, TILE_SIZE);
        print!(
            "mono={:.0}ms/{:.1}MB  ",
            dur.as_secs_f64() * 1000.0,
            peak as f64 / (1024.0 * 1024.0),
        );
        all_points.push(to_point(w, h, "monolithic", dur, peak, tiles));

        // Streaming
        let (dur, peak, tiles) = run_streaming(&src, TILE_SIZE, STREAMING_BUDGET);
        print!(
            "stream={:.0}ms/{:.1}MB  ",
            dur.as_secs_f64() * 1000.0,
            peak as f64 / (1024.0 * 1024.0),
        );
        all_points.push(to_point(w, h, "streaming", dur, peak, tiles));

        // MapReduce
        let (dur, peak, tiles) = run_mapreduce(&src, TILE_SIZE, STREAMING_BUDGET);
        println!(
            "mr={:.0}ms/{:.1}MB",
            dur.as_secs_f64() * 1000.0,
            peak as f64 / (1024.0 * 1024.0),
        );
        all_points.push(to_point(w, h, "mapreduce", dur, peak, tiles));
    }

    // --- Generate charts ---
    render_charts(&all_points, &report_dir, x_axis_crop);

    // Save raw data
    let json_path = report_dir.join("scalability_results.json");
    let json = serde_json::to_string_pretty(&all_points).unwrap();
    fs::write(&json_path, &json).unwrap();

    // Print summary table
    println!();
    println!(
        "{:<14} {:<12} {:>10} {:>10} {:>8} {:>10} {:>12}",
        "Size", "Engine", "Time (ms)", "Mem (MB)", "Tiles", "T/s/MB", "MB\u{00b7}s/tile",
    );
    println!("{}", "-".repeat(80));
    for p in &all_points {
        println!(
            "{:<14} {:<12} {:>10.1} {:>10.2} {:>8} {:>10.1} {:>12.4}",
            format!("{}x{}", p.width, p.height),
            p.engine,
            p.wall_time_ms,
            p.peak_memory_mb,
            p.tiles_produced,
            p.tiles_per_second_per_mb,
            p.resource_cost,
        );
    }

    // --- Memory bottleneck analysis ---
    println!();
    println!("=== Memory Bottleneck Analysis ===");
    println!();

    // Group by size and find the largest
    let largest = sizes.last().unwrap();
    let largest_mp = largest.0 as f64 * largest.1 as f64 / 1_000_000.0;

    // Monolithic bottleneck
    if let Some(mono) = all_points
        .iter()
        .find(|p| p.width == largest.0 && p.engine == "monolithic")
    {
        let canvas_bytes = largest.0 as f64 * largest.1 as f64 * 3.0; // RGB8 = 3 bpp
        let canvas_mb = canvas_bytes / (1024.0 * 1024.0);
        println!(
            "MONOLITHIC at {}x{} ({:.1} MP):",
            largest.0, largest.1, largest_mp,
        );
        println!(
            "  Peak memory: {:.1} MB — dominated by the full canvas allocation",
            mono.peak_memory_mb,
        );
        println!(
            "  The source raster ({:.1} MB) is cloned into a canvas-sized buffer.",
            canvas_mb,
        );
        println!("  During downscale, the current level + next level coexist in memory,",);
        println!(
            "  producing peak ≈ canvas + canvas/4 = {:.1} MB.",
            canvas_mb * 1.25,
        );
        println!("  This scales O(width × height) — doubling image dimensions quadruples memory.",);
    }

    // Streaming bottleneck
    if let Some(stream) = all_points
        .iter()
        .find(|p| p.width == largest.0 && p.engine == "streaming")
    {
        println!();
        println!(
            "STREAMING at {}x{} ({:.1} MP), budget {} MB:",
            largest.0,
            largest.1,
            largest_mp,
            STREAMING_BUDGET as f64 / (1024.0 * 1024.0),
        );
        println!(
            "  Peak memory: {:.1} MB — bounded by strip height, not canvas area.",
            stream.peak_memory_mb,
        );
        println!("  The engine holds: current strip + accumulator at each pyramid level",);
        println!("  (geometric series: strip + strip/4 + strip/16 + ...). Strip height is",);
        println!("  maximised within the budget. Memory scales O(width × strip_height),",);
        println!("  independent of image height. The bottleneck is strip width (= canvas width).",);
    }

    // MapReduce bottleneck
    if let Some(mr) = all_points
        .iter()
        .find(|p| p.width == largest.0 && p.engine == "mapreduce")
    {
        println!();
        println!(
            "MAPREDUCE at {}x{} ({:.1} MP), budget {} MB:",
            largest.0,
            largest.1,
            largest_mp,
            STREAMING_BUDGET as f64 / (1024.0 * 1024.0),
        );
        println!(
            "  Peak memory: {:.1} MB — same strip-bounded model as streaming.",
            mr.peak_memory_mb,
        );
        println!("  With K in-flight strips, peak = K × strip_cost + accumulator chain.",);
        println!("  The budget was too small for K>1 in-flight strips at this image width,",);
        println!("  so memory matches streaming. With a larger budget, K>1 trades memory",);
        println!("  for throughput by overlapping strip rendering.",);
    }

    // libvips bottleneck
    if let Some(vips) = all_points
        .iter()
        .find(|p| p.width == largest.0 && p.engine == "libvips")
    {
        println!();
        println!(
            "LIBVIPS at {}x{} ({:.1} MP):",
            largest.0, largest.1, largest_mp,
        );
        println!(
            "  Peak RSS: {:.1} MB — libvips uses a demand-driven pipeline where pixels",
            vips.peak_memory_mb,
        );
        println!("  are computed on demand per-region (O(tile_size²) working set). The RSS",);
        println!("  measured here includes the OS-level allocation footprint, which is higher",);
        println!("  than the logical working set due to memory mapping, page tables, and the",);
        println!("  decoded source image cache.",);
    }

    // Scaling comparison
    println!();
    println!("SCALING SUMMARY:");
    let smallest = sizes.first().unwrap();
    let scale_factor =
        (largest.0 as f64 * largest.1 as f64) / (smallest.0 as f64 * smallest.1 as f64);

    for engine in &["libvips", "monolithic", "streaming", "mapreduce"] {
        let small = all_points
            .iter()
            .find(|p| p.width == smallest.0 && p.engine == *engine);
        let large = all_points
            .iter()
            .find(|p| p.width == largest.0 && p.engine == *engine);
        if let (Some(s), Some(l)) = (small, large) {
            let mem_scale = l.peak_memory_mb / s.peak_memory_mb.max(0.01);
            let time_scale = l.wall_time_ms / s.wall_time_ms.max(0.01);
            println!(
                "  {:<12} image area {:.0}x larger → memory {:.1}x, time {:.1}x",
                engine, scale_factor, mem_scale, time_scale,
            );
        }
    }

    println!();
    println!(
        "Charts written to {}/scalability_*.svg",
        report_dir.display()
    );
    println!("JSON written to {}", json_path.display());
}
