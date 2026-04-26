//! Cross-version benchmark report.
//!
//! Loads `report/benchmark_history.json` (the cumulative
//! `BenchmarkSnapshot` log written by the `report` binary) and uses
//! polars to produce per-(size × engine × concurrency) views across
//! every version on record:
//!
//!   * `report/cross_version_metrics.csv` — flat long-form CSV with
//!     one row per (version, size, engine, concurrency) and the full
//!     metric set as columns. Useful as a stable join key for any
//!     external dashboard.
//!   * `report/cross_version_report.md` — markdown tables grouped by
//!     `(width × height × concurrency)`, with engines as rows and
//!     versions as columns. Each cell shows the metric value plus the
//!     percentage delta vs the previous version, so regressions /
//!     improvements stand out at a glance.
//!
//! The full polars stack is feature-gated (`--features polars`) so the
//! everyday bench builds stay light. Run:
//!
//!   cargo run --release --features polars --bin cross_version
//!
//! The binary is a no-op if the history file has fewer than two
//! versions — there's nothing to compare against until you've run
//! `report` on more than one libviprs version.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use libviprs_bench::{BenchmarkSnapshot, RunMetrics};
use polars::prelude::*;

fn main() {
    let report_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("report");
    let history_path = report_dir.join("benchmark_history.json");

    let history = load_history(&history_path).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        eprintln!();
        eprintln!("Run `cargo run --release --bin report` against multiple");
        eprintln!("libviprs versions first; each run appends a snapshot to");
        eprintln!("{}.", history_path.display());
        std::process::exit(1);
    });

    if history.is_empty() {
        eprintln!("History file is empty — nothing to compare.");
        std::process::exit(1);
    }

    let mut df = build_dataframe(&history)
        .unwrap_or_else(|e| panic!("failed to assemble cross-version frame: {e}"));

    // Stable ordering: by (size, engine, concurrency, version) so the
    // CSV reads naturally and the markdown tables iterate in a sensible
    // order.
    df.sort_in_place(
        ["width", "height", "engine", "concurrency", "version"],
        SortMultipleOptions::default(),
    )
    .expect("sort failed");

    let csv_path = report_dir.join("cross_version_metrics.csv");
    write_csv(&mut df, &csv_path);

    let md_path = report_dir.join("cross_version_report.md");
    write_markdown_report(&df, &md_path);

    println!("Cross-version report:");
    println!("  CSV:      {}", csv_path.display());
    println!("  Markdown: {}", md_path.display());
    println!();
    println!("{} snapshots, {} rows total.", history.len(), df.height());
    if history.len() < 2 {
        println!();
        println!("note: only one snapshot in history — re-run `report` on a different");
        println!("      libviprs version to populate version-over-version deltas.");
    }
}

fn load_history(path: &Path) -> Result<Vec<BenchmarkSnapshot>, String> {
    let raw = fs::read_to_string(path).map_err(|e| {
        format!(
            "couldn't read {}: {e}",
            path.display()
        )
    })?;
    serde_json::from_str(&raw).map_err(|e| format!("couldn't parse {}: {e}", path.display()))
}

/// Flatten the snapshot list into a long-form polars DataFrame. One
/// row per `(snapshot, run)` pair so polars can group by any axis.
fn build_dataframe(history: &[BenchmarkSnapshot]) -> PolarsResult<DataFrame> {
    let total: usize = history.iter().map(|s| s.runs.len()).sum();

    let mut version: Vec<String> = Vec::with_capacity(total);
    let mut timestamp: Vec<String> = Vec::with_capacity(total);
    let mut snapshot_tile_size: Vec<u32> = Vec::with_capacity(total);
    let mut snapshot_memory_budget: Vec<u64> = Vec::with_capacity(total);
    let mut label: Vec<String> = Vec::with_capacity(total);
    let mut width: Vec<u32> = Vec::with_capacity(total);
    let mut height: Vec<u32> = Vec::with_capacity(total);
    let mut megapixels: Vec<f64> = Vec::with_capacity(total);
    let mut engine: Vec<String> = Vec::with_capacity(total);
    let mut concurrency: Vec<u32> = Vec::with_capacity(total);
    let mut wall_time_ms: Vec<f64> = Vec::with_capacity(total);
    let mut peak_memory_mb: Vec<f64> = Vec::with_capacity(total);
    let mut tiles_produced: Vec<u64> = Vec::with_capacity(total);
    let mut tiles_per_second: Vec<f64> = Vec::with_capacity(total);
    let mut tiles_per_second_per_mb: Vec<f64> = Vec::with_capacity(total);

    for snap in history {
        for run in &snap.runs {
            version.push(snap.version.clone());
            timestamp.push(snap.timestamp.clone());
            snapshot_tile_size.push(snap.tile_size);
            snapshot_memory_budget.push(snap.memory_budget_bytes);
            label.push(run.label.clone());
            width.push(run.width);
            height.push(run.height);
            megapixels.push(run.width as f64 * run.height as f64 / 1_000_000.0);
            engine.push(run.engine.clone());
            concurrency.push(run.concurrency as u32);
            wall_time_ms.push(run.wall_time_ms());
            peak_memory_mb.push(run.peak_memory_mb());
            tiles_produced.push(run.tiles_produced);
            tiles_per_second.push(run.tiles_per_second());
            tiles_per_second_per_mb.push(run.tiles_per_second_per_mb());
        }
    }

    df!(
        "version" => version,
        "timestamp" => timestamp,
        "snapshot_tile_size" => snapshot_tile_size,
        "snapshot_memory_budget_bytes" => snapshot_memory_budget,
        "label" => label,
        "width" => width,
        "height" => height,
        "megapixels" => megapixels,
        "engine" => engine,
        "concurrency" => concurrency,
        "wall_time_ms" => wall_time_ms,
        "peak_memory_mb" => peak_memory_mb,
        "tiles_produced" => tiles_produced,
        "tiles_per_second" => tiles_per_second,
        "tiles_per_second_per_mb" => tiles_per_second_per_mb,
    )
}

fn write_csv(df: &mut DataFrame, path: &PathBuf) {
    let mut file = fs::File::create(path).expect("open csv");
    CsvWriter::new(&mut file)
        .include_header(true)
        .finish(df)
        .expect("write csv");
}

/// Write a markdown report grouped by `(width × height × concurrency)`,
/// engines as rows, versions as columns. Each cell carries the metric
/// value plus the percentage change vs the previous (chronological)
/// version so a regression is obvious on a glance.
fn write_markdown_report(df: &DataFrame, path: &PathBuf) {
    let mut out = String::new();
    out.push_str("# Cross-version benchmark report\n\n");
    out.push_str(
        "Generated by `cross_version` from `benchmark_history.json`. \
         Each cell shows the metric value followed by the percentage \
         delta vs the previous version (newer is left-to-right in the \
         CSV; here we show one column per version on record).\n\n",
    );

    // Pull the unique sort axes out as Vecs we can iterate. Polars
    // returns a `Series` of unique values; we drop nulls and sort.
    let sizes = unique_size_concurrency_combos(df);
    let versions = unique_string_column(df, "version");

    if versions.len() < 2 {
        out.push_str(
            "_Only one version in the history file — version-over-version deltas will be empty._\n\n",
        );
    }

    for (w, h, c) in &sizes {
        let mp = (*w as f64) * (*h as f64) / 1_000_000.0;
        out.push_str(&format!(
            "## {w}×{h} ({mp:.1} MP), concurrency = {c}\n\n",
        ));
        for metric in [
            ("wall_time_ms", "Wall time (ms)", false),
            ("peak_memory_mb", "Peak memory (MB)", false),
            ("tiles_per_second", "Throughput (tiles/s)", true),
            ("tiles_per_second_per_mb", "Efficiency (tiles/s/MB)", true),
        ] {
            let (col, title, higher_is_better) = metric;
            out.push_str(&format!("### {title}\n\n"));
            out.push_str(&render_metric_table(df, *w, *h, *c, col, &versions, higher_is_better));
            out.push('\n');
        }
    }

    let mut file = fs::File::create(path).expect("open md");
    file.write_all(out.as_bytes()).expect("write md");
}

fn unique_size_concurrency_combos(df: &DataFrame) -> Vec<(u32, u32, u32)> {
    // Use lazy + group_by to get distinct triples, then materialise.
    let lf = df.clone().lazy().select([
        col("width"),
        col("height"),
        col("concurrency"),
    ]).unique(None, UniqueKeepStrategy::Any);
    let frame = lf
        .sort_by_exprs(
            [col("width"), col("height"), col("concurrency")],
            SortMultipleOptions::default(),
        )
        .collect()
        .expect("collect distinct sizes");
    let widths = frame.column("width").unwrap().u32().unwrap();
    let heights = frame.column("height").unwrap().u32().unwrap();
    let concs = frame.column("concurrency").unwrap().u32().unwrap();
    (0..frame.height())
        .filter_map(|i| {
            Some((widths.get(i)?, heights.get(i)?, concs.get(i)?))
        })
        .collect()
}

fn unique_string_column(df: &DataFrame, name: &str) -> Vec<String> {
    let s = df
        .column(name)
        .unwrap()
        .unique_stable()
        .expect("unique");
    let chunked = s.str().expect("str column");
    chunked.into_iter().flatten().map(String::from).collect()
}

/// Render one metric as a markdown table for a fixed (w, h, c). Rows
/// are engines; columns are versions; cells are `value (Δ%)`.
fn render_metric_table(
    df: &DataFrame,
    w: u32,
    h: u32,
    c: u32,
    metric_col: &str,
    versions: &[String],
    higher_is_better: bool,
) -> String {
    // Filter to the (w, h, c) slice; keep only the metric + grouping cols.
    let filtered = df
        .clone()
        .lazy()
        .filter(
            col("width")
                .eq(lit(w))
                .and(col("height").eq(lit(h)))
                .and(col("concurrency").eq(lit(c))),
        )
        .select([col("engine"), col("version"), col(metric_col)])
        .collect()
        .expect("filter slice");

    let engines = unique_string_column(&filtered, "engine");

    let mut out = String::new();
    // Header
    out.push_str("| Engine ");
    for v in versions {
        out.push_str(&format!("| {v} "));
    }
    out.push_str("|\n");
    out.push_str("|---");
    for _ in versions {
        out.push_str("|---");
    }
    out.push_str("|\n");

    for engine in &engines {
        out.push_str(&format!("| {engine} "));
        let mut prev: Option<f64> = None;
        for v in versions {
            let cell = lookup_metric(&filtered, engine, v, metric_col);
            match cell {
                Some(value) => {
                    let delta = match prev {
                        Some(p) if p != 0.0 => Some(((value - p) / p) * 100.0),
                        _ => None,
                    };
                    let delta_str = match delta {
                        Some(d) => {
                            let sign = if d >= 0.0 { "+" } else { "" };
                            // Sub-0.05% deltas are noise from rounding —
                            // don't decorate. Otherwise mark the side
                            // that's better for the metric.
                            let arrow = if d.abs() < 0.05 {
                                ""
                            } else if (d > 0.0) == higher_is_better {
                                " ✅"
                            } else {
                                " ⚠️"
                            };
                            format!(" ({sign}{d:.1}%{arrow})")
                        }
                        None => String::new(),
                    };
                    out.push_str(&format!("| {value:.2}{delta_str} "));
                    prev = Some(value);
                }
                None => out.push_str("| — "),
            }
        }
        out.push_str("|\n");
    }
    out
}

fn lookup_metric(
    df: &DataFrame,
    engine: &str,
    version: &str,
    metric_col: &str,
) -> Option<f64> {
    let frame = df
        .clone()
        .lazy()
        .filter(
            col("engine")
                .eq(lit(engine))
                .and(col("version").eq(lit(version))),
        )
        .select([col(metric_col)])
        .collect()
        .ok()?;
    if frame.height() == 0 {
        return None;
    }
    let s = frame.column(metric_col).ok()?;
    s.f64().ok()?.get(0)
}

// Suppress an unused import warning when both items get used inside a
// helper that inlines them.
#[allow(dead_code)]
fn _ensure_uses(_: &RunMetrics) {}
