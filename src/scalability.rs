//! Engine scalability benchmark.
//!
//! Generates a SYNTHETIC gradient raster (see `gradient_raster`) at
//! progressively larger sizes — the actual `43551_California_South.pdf`
//! fixture is not committed, so the workload is a stand-in sized to that
//! page's 1.42:1 aspect, NOT a rasterized blueprint. Runs all four engines
//! (libvips, monolithic, streaming, MapReduce) at each size and at matched
//! thread budgets (1 and num_cpus), producing SVG line charts — one set per
//! thread budget — showing how wall time, peak RSS, and efficiency scale
//! with image area.
//!
//! Run: cargo run --release --bin scalability
//!
//! Output: report/scalability_*.svg + report/scalability_results.json

// The chart plumbing threads fixed-arity metric tuples and borrowed series
// slices through a few local closures; naming each as a `type` would add
// noise without aiding readers, so the complexity lint is allowed here.
#![allow(clippy::type_complexity)]

use std::fs;
use std::path::Path;
use std::time::Instant;

use plotters::prelude::*;
use serde::{Deserialize, Serialize};

use libviprs::streaming::BudgetPolicy;
use libviprs::{
    EngineBuilder, EngineConfig, EngineKind, FsSink, Layout, PyramidPlanner, Raster,
    RasterStripSource, TileFormat,
};
use libviprs_bench::provenance::{OracleMatch, Provenance};
use libviprs_bench::{bench_libvips, gradient_raster, vips_available, write_temp_png};

/// Peak RSS of the current process in bytes. Mirrors the RSS basis the
/// libvips paths report so the scalability charts compare like-for-like
/// memory (issue #153). `ru_maxrss` is a process-wide high-water mark; see
/// the note in `libviprs_bench::RunMetrics::peak_rss_mb`.
fn process_peak_rss() -> u64 {
    use std::mem::MaybeUninit;
    let mut rusage = MaybeUninit::<libc::rusage>::uninit();
    let ret = unsafe { libc::getrusage(libc::RUSAGE_SELF, rusage.as_mut_ptr()) };
    if ret != 0 {
        return 0;
    }
    let rusage = unsafe { rusage.assume_init() };
    if cfg!(target_os = "macos") {
        // macOS reports ru_maxrss in bytes.
        rusage.ru_maxrss as u64
    } else {
        // Linux reports ru_maxrss in kilobytes.
        rusage.ru_maxrss as u64 * 1024
    }
}

const TILE_SIZE: u32 = 256;
/// Floor on the streaming engine's memory budget. Keeps small images in
/// the "true streaming" regime; large images need more (computed below).
const STREAMING_BUDGET_FLOOR: u64 = 4_000_000; // 4 MB

/// Compute the per-image streaming budget. The floor `STREAMING_BUDGET_FLOOR`
/// keeps small images in the streaming regime; for wider canvases we need
/// at least one tile-aligned strip (`min strip = 2 × tile_size`) to fit,
/// otherwise `BudgetPolicy::Error` (intentionally strict) trips. The
/// 2× multiplier leaves headroom for the per-level accumulator chain.
fn streaming_budget_for(width: u32, tile_size: u32) -> u64 {
    let min_strip_bytes = (width as u64) * (tile_size as u64) * 2 * 3;
    (min_strip_bytes * 2).max(STREAMING_BUDGET_FLOOR)
}

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
    /// Thread budget this point was measured at (`VIPS_CONCURRENCY` for
    /// libvips; engine concurrency for the libviprs engines). Points at
    /// different thread caps are NEVER mixed on one chart line set (issue
    /// #156). Defaults to 0 for pre-#156 history that predates the field.
    #[serde(default)]
    concurrency: usize,
    wall_time_ms: f64,
    /// Engine-tracked working set (libviprs engines; 0 for libvips). Kept in a
    /// separate field from `peak_rss_mb` so the two memory bases are never
    /// conflated (issue #153). Defaults to 0 for pre-#153 history.
    #[serde(default)]
    tracked_memory_mb: f64,
    /// Process/child peak RSS — the cross-engine-comparable memory basis.
    /// The `peak_memory_mb` alias lets pre-#153 scalability history (which
    /// used that field name) deserialize unchanged.
    #[serde(alias = "peak_memory_mb")]
    peak_rss_mb: f64,
    tiles_produced: u64,
    tiles_per_second: f64,
    /// Tiles/s per RSS-MB (common basis).
    tiles_per_second_per_mb: f64,
    /// RSS-MB-seconds per tile (common basis).
    resource_cost: f64,
}

/// A libviprs engine run's measurements: wall time, engine-tracked working
/// set, process peak RSS, and tile count.
struct EngineRun {
    dur: std::time::Duration,
    tracked_bytes: u64,
    rss_bytes: u64,
    tiles: u64,
}

/// Fresh temp directory for on-disk tile output. The libviprs engines write
/// real PNG tiles here just like libvips `dzsave`, so neither side gets an
/// in-RAM sink advantage (issue #153). Removed by the caller once counted.
fn sink_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir()
        .join("libviprs-bench")
        .join(format!("scal_{}_{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_monolithic(src: &Raster, tile_size: u32, concurrency: usize) -> EngineRun {
    let planner =
        PyramidPlanner::new(src.width(), src.height(), tile_size, 0, Layout::DeepZoom).unwrap();
    let plan = planner.plan();
    let out_dir = sink_dir("mono");
    let sink = FsSink::new(out_dir.join("pyramid"), plan.clone()).with_format(TileFormat::Png);
    let start = Instant::now();
    let result = EngineBuilder::new(src, plan, &sink)
        .with_engine(EngineKind::Monolithic)
        .with_config(EngineConfig::default().with_concurrency(concurrency))
        .run()
        .unwrap();
    let dur = start.elapsed();
    let rss_bytes = process_peak_rss();
    let _ = std::fs::remove_dir_all(&out_dir);
    EngineRun {
        dur,
        tracked_bytes: result.peak_memory_bytes,
        rss_bytes,
        tiles: result.tiles_produced,
    }
}

fn run_streaming(src: &Raster, tile_size: u32, budget: u64, concurrency: usize) -> EngineRun {
    let planner =
        PyramidPlanner::new(src.width(), src.height(), tile_size, 0, Layout::DeepZoom).unwrap();
    let plan = planner.plan();
    let out_dir = sink_dir("stream");
    let sink = FsSink::new(out_dir.join("pyramid"), plan.clone()).with_format(TileFormat::Png);
    let strip_src = RasterStripSource::new(src);
    let start = Instant::now();
    let result = EngineBuilder::new(strip_src, plan, &sink)
        .with_engine(EngineKind::Streaming)
        .with_config(EngineConfig::default().with_concurrency(concurrency))
        .with_memory_budget(budget)
        .with_budget_policy(BudgetPolicy::Error)
        .run()
        .unwrap();
    let dur = start.elapsed();
    let rss_bytes = process_peak_rss();
    let _ = std::fs::remove_dir_all(&out_dir);
    EngineRun {
        dur,
        tracked_bytes: result.peak_memory_bytes,
        rss_bytes,
        tiles: result.tiles_produced,
    }
}

fn run_mapreduce(src: &Raster, tile_size: u32, budget: u64, concurrency: usize) -> EngineRun {
    let planner =
        PyramidPlanner::new(src.width(), src.height(), tile_size, 0, Layout::DeepZoom).unwrap();
    let plan = planner.plan();
    let out_dir = sink_dir("mr");
    let sink = FsSink::new(out_dir.join("pyramid"), plan.clone()).with_format(TileFormat::Png);
    let strip_src = RasterStripSource::new(src);
    let start = Instant::now();
    let result = EngineBuilder::new(strip_src, plan, &sink)
        .with_engine(EngineKind::MapReduce)
        .with_config(EngineConfig::default().with_concurrency(concurrency))
        .with_memory_budget(budget)
        .with_budget_policy(BudgetPolicy::Error)
        .run()
        .unwrap();
    let dur = start.elapsed();
    let rss_bytes = process_peak_rss();
    let _ = std::fs::remove_dir_all(&out_dir);
    EngineRun {
        dur,
        tracked_bytes: result.peak_memory_bytes,
        rss_bytes,
        tiles: result.tiles_produced,
    }
}

#[allow(clippy::too_many_arguments)]
fn to_point(
    w: u32,
    h: u32,
    engine: &str,
    concurrency: usize,
    dur: std::time::Duration,
    tracked_bytes: u64,
    rss_bytes: u64,
    tiles: u64,
) -> ScalabilityPoint {
    let mp = w as f64 * h as f64 / 1_000_000.0;
    let secs = dur.as_secs_f64();
    let ms = secs * 1000.0;
    let tracked_mb = tracked_bytes as f64 / (1024.0 * 1024.0);
    let rss_mb = rss_bytes as f64 / (1024.0 * 1024.0);
    let tps = if secs > 0.0 { tiles as f64 / secs } else { 0.0 };
    // Efficiency and resource-cost use the common RSS basis so every engine's
    // number means the same thing (issue #153).
    let tps_mb = if rss_mb > 0.0 { tps / rss_mb } else { 0.0 };
    let cost = if tiles > 0 {
        (rss_mb * secs) / tiles as f64
    } else {
        0.0
    };

    ScalabilityPoint {
        width: w,
        height: h,
        megapixels: mp,
        engine: engine.to_string(),
        concurrency,
        wall_time_ms: ms,
        tracked_memory_mb: tracked_mb,
        peak_rss_mb: rss_mb,
        tiles_produced: tiles,
        tiles_per_second: tps,
        tiles_per_second_per_mb: tps_mb,
        resource_cost: cost,
    }
}

/// Path to the committed real-content PDF fixture (issue #30), resolved against
/// the crate manifest so it works regardless of the working directory.
#[cfg(feature = "pdfium")]
const PDF_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/cc_licenses_mapping.pdf"
);

/// Render DPI that scales the committed PDF fixture to approximately
/// `target_width` pixels wide.
///
/// The fixture's page is ~1190 pt wide (A3 landscape) — i.e. ~1190 px at 72
/// DPI — so we scale linearly from that. This lands the rasterized-PDF series
/// at the same image sizes as the synthetic-gradient sweep, keeping the two
/// workloads directly comparable on the shared megapixel x-axis (issue #31).
#[cfg(feature = "pdfium")]
fn pdf_dpi_for_width(target_width: u32) -> u32 {
    const FIXTURE_WIDTH_PX_AT_72_DPI: f64 = 1190.0;
    let dpi = 72.0 * target_width as f64 / FIXTURE_WIDTH_PX_AT_72_DPI;
    (dpi.round() as u32).max(1)
}

/// Run the rasterized-PDF streaming workload for one sweep size and return its
/// scalability point (engine series `"streaming-pdf"`), or `None` if the pdfium
/// source could not be rendered (e.g. libpdfium unavailable) — the benchmark
/// then simply omits the real-content series for that point rather than
/// aborting the whole run.
///
/// The point is plotted at the PDF's *actual* rendered dimensions (from the
/// returned metrics), so its megapixel x-position is honest even when pdfium
/// rounds the page a pixel differently from the gradient target.
#[cfg(feature = "pdfium")]
fn run_pdf_streaming(
    target_width: u32,
    concurrency: usize,
    tile_size: u32,
) -> Option<ScalabilityPoint> {
    let dpi = pdf_dpi_for_width(target_width);
    let label = format!("pdf_{target_width}_c{concurrency}");
    match libviprs_bench::bench_streaming_pdf(
        std::path::Path::new(PDF_FIXTURE),
        1,
        dpi,
        tile_size,
        concurrency,
        streaming_budget_for(target_width, tile_size),
        &label,
    ) {
        Ok(m) => Some(to_point(
            m.width,
            m.height,
            "streaming-pdf",
            concurrency,
            m.wall_time,
            m.tracked_memory_bytes,
            m.peak_rss_bytes,
            m.tiles_produced,
        )),
        Err(e) => {
            eprintln!("  [pdf] skipped {target_width}px @ {dpi} dpi: {e}");
            None
        }
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
            let visible: Vec<(f64, f64)> =
                pts.iter().filter(|(x, _)| *x >= x_lo).copied().collect();
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

/// Render one full set of five scalability SVGs for every distinct thread
/// budget present in `all_points`. Points measured at different thread caps
/// are NEVER drawn on the same line set (issue #156): each concurrency level
/// gets its own `scalability_*_c{n}.svg` files with the cap in the caption.
fn render_charts(
    all_points: &[ScalabilityPoint],
    report_dir: &std::path::Path,
    x_min: Option<f64>,
) {
    let mut concs: Vec<usize> = all_points.iter().map(|p| p.concurrency).collect();
    concs.sort_unstable();
    concs.dedup();
    for conc in concs {
        let subset: Vec<ScalabilityPoint> = all_points
            .iter()
            .filter(|p| p.concurrency == conc)
            .cloned()
            .collect();
        render_charts_for_conc(&subset, report_dir, x_min, conc);
    }
}

/// Render the five scalability SVGs for a single thread budget into
/// `report_dir`, filenames suffixed `_c{concurrency}`.
fn render_charts_for_conc(
    all_points: &[ScalabilityPoint],
    report_dir: &std::path::Path,
    x_min: Option<f64>,
    concurrency: usize,
) {
    let thread_cap = if concurrency == 0 {
        "auto threads".to_string()
    } else {
        format!(
            "{concurrency} thread{}",
            if concurrency == 1 { "" } else { "s" }
        )
    };
    let vips_color = RGBColor(156, 39, 176);
    let mono_color = RGBColor(66, 133, 244);
    let stream_color = RGBColor(52, 168, 83);
    let mr_color = RGBColor(234, 67, 53);
    // Orange — the real-content rasterized-PDF series, kept visually distinct
    // from the four synthetic-gradient engine lines (issue #31).
    let pdf_color = RGBColor(255, 152, 0);

    let extract_series = |engine: &str| -> Vec<(f64, f64, f64, f64, f64, f64)> {
        all_points
            .iter()
            .filter(|p| p.engine == engine)
            .map(|p| {
                (
                    p.megapixels,
                    p.wall_time_ms,
                    p.peak_rss_mb,
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
    // Real-content series (issue #31). Empty on a default build (no
    // `streaming-pdf` points), in which case the chart is byte-for-byte what it
    // was before and the titles keep saying "synthetic gradient".
    let pdf_data = extract_series("streaming-pdf");
    let workload = if pdf_data.is_empty() {
        "synthetic gradient"
    } else {
        "gradient + rasterized PDF"
    };

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
    // NOTE: the four engine series (libvips/monolithic/streaming/mapreduce)
    // are a SYNTHETIC gradient raster (see `gradient_raster`), sized to the
    // 1.42:1 aspect of the California South page. Under the `pdfium` feature a
    // fifth `streaming-pdf` series is added: the committed real-content fixture
    // (`fixtures/cc_licenses_mapping.pdf`) rasterized via `PdfiumStripSource`
    // (issues #30/#31). Titles name whichever workloads are actually present
    // (`workload`) rather than implying a real blueprint when only the gradient
    // ran (issue #161).
    let charts: [(String, String, &str, u8); 5] = [
        (
            format!("scalability_wall_time_c{concurrency}.svg"),
            format!("Wall Time Scalability — {workload} ({thread_cap})"),
            "Time (ms)",
            1,
        ),
        (
            format!("scalability_peak_memory_c{concurrency}.svg"),
            format!("Peak RSS Scalability — {workload} ({thread_cap})"),
            "Peak RSS (MB)",
            2,
        ),
        (
            format!("scalability_throughput_c{concurrency}.svg"),
            format!("Throughput Scalability — {workload} ({thread_cap})"),
            "Tiles/s",
            3,
        ),
        (
            format!("scalability_efficiency_c{concurrency}.svg"),
            format!("Memory Efficiency — Tiles/s per RSS-MB ({thread_cap})"),
            "Tiles/s/RSS-MB",
            4,
        ),
        (
            format!("scalability_resource_cost_c{concurrency}.svg"),
            format!("Resource Cost — RSS-MB\u{00b7}s per Tile, lower is better ({thread_cap})"),
            "RSS-MB\u{00b7}s / tile",
            5,
        ),
    ];

    for (filename, title, y_label, idx) in &charts {
        let filename = filename.as_str();
        let title = title.as_str();
        let idx = *idx;
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
        // The real-content series only when it was measured (pdfium build).
        if !pdf_data.is_empty() {
            series.push(("Streaming (PDF)", pdf_color, pts_for(&pdf_data)));
        }

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
    /// When true, crop the x-axis to `DEFAULT_X_AXIS_CROP_MP` and up. The
    /// full range is now the DEFAULT (issue #161) — the crop hid the small-
    /// image regime and was only opt-in for zooming into the large-image
    /// tail.
    crop: bool,
}

fn parse_cli() -> CliOpts {
    let mut replot = false;
    let mut crop = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--replot" => replot = true,
            "--crop" => crop = true,
            // Back-compat no-ops: full range is now the default.
            "--no-crop" | "--full-x-range" => {}
            "-h" | "--help" => {
                println!("Usage: scalability [--replot] [--crop]");
                println!();
                println!("  --replot     Skip the bench; redraw SVGs from the existing");
                println!("               report/scalability_results.json.");
                println!(
                    "  --crop       Zoom the x-axis to >= {} MP (default is the full",
                    DEFAULT_X_AXIS_CROP_MP
                );
                println!("               range so the small-image regime is visible).");
                std::process::exit(0);
            }
            other => {
                eprintln!("Unknown argument: {other}");
                eprintln!("Run with --help for usage.");
                std::process::exit(2);
            }
        }
    }
    CliOpts { replot, crop }
}

fn main() {
    let opts = parse_cli();
    let x_axis_crop: Option<f64> = if opts.crop {
        Some(DEFAULT_X_AXIS_CROP_MP)
    } else {
        None
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
    println!(
        "Workload: SYNTHETIC gradient raster; aspect 1.42:1 matches the \
         California South page (4608x3240 pts)."
    );
    #[cfg(feature = "pdfium")]
    println!(
        "Real-content series: rasterized PDF fixture \
         (fixtures/cc_licenses_mapping.pdf) via PdfiumStripSource streaming, \
         charted as 'streaming-pdf'."
    );
    println!(
        "Sizes: {} points from 512x360 to {}x{}",
        sizes.len(),
        sizes.last().unwrap().0,
        sizes.last().unwrap().1,
    );
    println!(
        "Tile size: {TILE_SIZE}, streaming budget floor: {STREAMING_BUDGET_FLOOR} bytes (auto-scaled per width)",
    );
    if has_vips {
        println!("libvips CLI: included");
    } else {
        println!("libvips CLI: not found, skipping");
    }
    // Mismatched-oracle guard (#33) on the default container CMD: if this run
    // measured a different libvips than the container was pinned to build, its
    // numbers are not comparable to a pinned-oracle run — warn loudly. Only
    // fires on a genuine parsed mismatch, never on a host run without libvips.
    let prov = Provenance::capture();
    if let OracleMatch::Mismatch { measured, pinned } = prov.libvips_oracle_match() {
        eprintln!(
            "WARNING: measured libvips {}.{} != pinned oracle {}.{} — this \
             scalability run measured a different libvips than the container \
             was pinned to build (issue #33); its numbers are NOT comparable \
             to a pinned-oracle run.",
            measured.0, measured.1, pinned.0, pinned.1
        );
    }
    println!();

    let mut all_points: Vec<ScalabilityPoint> = Vec::new();

    // Matched thread budgets: run EVERY engine — including libvips, via a
    // matched `VIPS_CONCURRENCY` — at both a single thread and all cores, so
    // no engine is silently pinned to a different thread count than another
    // (issue #156). The two levels are charted separately, never mixed.
    let ncpu = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let concurrency_levels: Vec<usize> = if ncpu > 1 { vec![1, ncpu] } else { vec![1] };
    println!("Thread budgets: {concurrency_levels:?} (1 and num_cpus)");
    println!();

    for &conc in &concurrency_levels {
        println!("--- thread budget: {conc} ---");
        for &(w, h) in &sizes {
            let src = gradient_raster(w, h);
            let mp = w as f64 * h as f64 / 1_000_000.0;
            print!("[c{conc}] {w}x{h} ({mp:.1} MP): ");

            // libvips: prefer in-process FFI, fall back to CLI. Both honour
            // the matched thread budget (`concurrency_set` / VIPS_CONCURRENCY).
            // `vips_done` is only reassigned under the `libvips` feature.
            #[cfg_attr(not(feature = "libvips"), allow(unused_mut))]
            let mut vips_done = false;
            #[cfg(feature = "libvips")]
            {
                if let Some(r) =
                    libviprs_bench::bench_libvips_inprocess(&src, TILE_SIZE, conc, "vips")
                {
                    print!(
                        "vips={:.0}ms/{:.1}MB(rss)  ",
                        r.wall_time_ms(),
                        r.peak_rss_mb()
                    );
                    all_points.push(to_point(
                        w,
                        h,
                        "libvips",
                        conc,
                        r.wall_time,
                        r.tracked_memory_bytes,
                        r.peak_rss_bytes,
                        r.tiles_produced,
                    ));
                    vips_done = true;
                }
            }
            if !vips_done && has_vips {
                let png_path = write_temp_png(&src);
                if let Some(r) = bench_libvips(&png_path, w, h, TILE_SIZE, conc, "vips") {
                    print!(
                        "vips={:.0}ms/{:.1}MB(rss)  ",
                        r.wall_time_ms(),
                        r.peak_rss_mb()
                    );
                    all_points.push(to_point(
                        w,
                        h,
                        "libvips",
                        conc,
                        r.wall_time,
                        r.tracked_memory_bytes,
                        r.peak_rss_bytes,
                        r.tiles_produced,
                    ));
                }
                let _ = fs::remove_file(&png_path);
            }

            // Monolithic
            let run = run_monolithic(&src, TILE_SIZE, conc);
            print!(
                "mono={:.0}ms/{:.1}MB(trk)  ",
                run.dur.as_secs_f64() * 1000.0,
                run.tracked_bytes as f64 / (1024.0 * 1024.0),
            );
            all_points.push(to_point(
                w,
                h,
                "monolithic",
                conc,
                run.dur,
                run.tracked_bytes,
                run.rss_bytes,
                run.tiles,
            ));

            // Streaming + MapReduce share a budget chosen per-width so the
            // tile-aligned minimum strip always fits.
            let budget = streaming_budget_for(w, TILE_SIZE);

            // Streaming
            let run = run_streaming(&src, TILE_SIZE, budget, conc);
            print!(
                "stream={:.0}ms/{:.1}MB(trk)  ",
                run.dur.as_secs_f64() * 1000.0,
                run.tracked_bytes as f64 / (1024.0 * 1024.0),
            );
            all_points.push(to_point(
                w,
                h,
                "streaming",
                conc,
                run.dur,
                run.tracked_bytes,
                run.rss_bytes,
                run.tiles,
            ));

            // MapReduce
            let run = run_mapreduce(&src, TILE_SIZE, budget, conc);
            println!(
                "mr={:.0}ms/{:.1}MB(trk)",
                run.dur.as_secs_f64() * 1000.0,
                run.tracked_bytes as f64 / (1024.0 * 1024.0),
            );
            all_points.push(to_point(
                w,
                h,
                "mapreduce",
                conc,
                run.dur,
                run.tracked_bytes,
                run.rss_bytes,
                run.tiles,
            ));

            // Real-content counterpart (issue #31): rasterize the committed PDF
            // fixture to ~this width via PdfiumStripSource (streaming) and
            // pyramid it through the streaming engine, as the separate
            // "streaming-pdf" series. Feature-gated, so the default build is
            // unaffected.
            #[cfg(feature = "pdfium")]
            if let Some(p) = run_pdf_streaming(w, conc, TILE_SIZE) {
                println!(
                    "        pdf={:.0}ms/{:.1}MB(rss)  ({}x{} @ {}dpi)",
                    p.wall_time_ms,
                    p.peak_rss_mb,
                    p.width,
                    p.height,
                    pdf_dpi_for_width(w),
                );
                all_points.push(p);
            }
        }
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
        "{:<14} {:<12} {:>10} {:>12} {:>10} {:>8} {:>12} {:>14}",
        "Size",
        "Engine",
        "Time (ms)",
        "Tracked MB",
        "RSS MB",
        "Tiles",
        "T/s/RSS-MB",
        "RSS-MB\u{00b7}s/tile",
    );
    println!("{}", "-".repeat(92));
    for p in &all_points {
        println!(
            "{:<14} {:<12} {:>10.1} {:>12.2} {:>10.2} {:>8} {:>12.1} {:>14.4}",
            format!("{}x{}", p.width, p.height),
            p.engine,
            p.wall_time_ms,
            p.tracked_memory_mb,
            p.peak_rss_mb,
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
            "  Tracked working set: {:.1} MB — dominated by the full canvas allocation",
            mono.tracked_memory_mb,
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
            streaming_budget_for(largest.0, TILE_SIZE) as f64 / (1024.0 * 1024.0),
        );
        println!(
            "  Tracked working set: {:.1} MB — bounded by strip height, not canvas area.",
            stream.tracked_memory_mb,
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
            streaming_budget_for(largest.0, TILE_SIZE) as f64 / (1024.0 * 1024.0),
        );
        println!(
            "  Tracked working set: {:.1} MB — same strip-bounded model as streaming.",
            mr.tracked_memory_mb,
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
            vips.peak_rss_mb,
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
            let mem_scale = l.peak_rss_mb / s.peak_rss_mb.max(0.01);
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
