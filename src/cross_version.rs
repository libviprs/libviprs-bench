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

use libviprs_bench::provenance::OracleMatch;
use libviprs_bench::version_id::{ordered_version_keys, version_key};
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

    // Flag any snapshot whose measured libvips diverged from the oracle it was
    // pinned to build (issue #33). `fingerprint()` already sorts such a run
    // into its own measured-version column, but call the mismatch out by name
    // so a reader knows that column is an artifact of a mispinned build, not a
    // real libvips under test.
    for snap in &history {
        if let OracleMatch::Mismatch { measured, pinned } = snap.provenance.libvips_oracle_match() {
            eprintln!(
                "WARNING: snapshot {} ({}) measured libvips {}.{} but was pinned \
                 to {}.{} — mismatched oracle (issue #33); its column is not \
                 comparable to pinned-oracle snapshots.",
                snap.version, snap.timestamp, measured.0, measured.1, pinned.0, pinned.1
            );
        }
    }

    let mut df = build_dataframe(&history)
        .unwrap_or_else(|e| panic!("failed to assemble cross-version frame: {e}"));

    // Stable ordering: by (size, engine, concurrency, version_key). The
    // version axis is keyed by `version@short_sha` so two builds of the same
    // version don't collapse, and the human-facing column order is set
    // separately by semver/timestamp (below), not this lexicographic sort.
    df.sort_in_place(
        ["width", "height", "engine", "concurrency", "version_key"],
        SortMultipleOptions::default(),
    )
    .expect("sort failed");

    let csv_path = report_dir.join("cross_version_metrics.csv");
    write_csv(&mut df, &csv_path);

    // Release axis ordered by (semver, timestamp) rather than lexicographically,
    // so `0.9.0` precedes `0.10.0` and re-measured versions order by time
    // (issue #19).
    let versions = ordered_version_keys(&history);
    let md_path = report_dir.join("cross_version_report.md");
    write_markdown_report(&df, &versions, &md_path);

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
    let raw =
        fs::read_to_string(path).map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
    let mut history: Vec<BenchmarkSnapshot> = serde_json::from_str(&raw)
        .map_err(|e| format!("couldn't parse {}: {e}", path.display()))?;
    // Same forward-migration the report binary applies: normalize legacy
    // labels and map old field names so a mixed-schema history file loads.
    for snap in &mut history {
        libviprs_bench::migrate_snapshot(snap);
    }
    Ok(history)
}

/// Flatten the snapshot list into a long-form polars DataFrame. One
/// row per `(snapshot, run)` pair so polars can group by any axis.
fn build_dataframe(history: &[BenchmarkSnapshot]) -> PolarsResult<DataFrame> {
    let total: usize = history.iter().map(|s| s.runs.len()).sum();

    let mut version: Vec<String> = Vec::with_capacity(total);
    let mut git_sha: Vec<String> = Vec::with_capacity(total);
    let mut version_key_col: Vec<String> = Vec::with_capacity(total);
    let mut fingerprint: Vec<String> = Vec::with_capacity(total);
    let mut wall_ci95: Vec<f64> = Vec::with_capacity(total);
    let mut rss_ci95: Vec<f64> = Vec::with_capacity(total);
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
    let mut tracked_memory_mb: Vec<f64> = Vec::with_capacity(total);
    let mut peak_rss_mb: Vec<f64> = Vec::with_capacity(total);
    let mut tiles_produced: Vec<u64> = Vec::with_capacity(total);
    let mut tiles_per_second: Vec<f64> = Vec::with_capacity(total);
    let mut tiles_per_second_per_mb: Vec<f64> = Vec::with_capacity(total);

    for snap in history {
        let fp = snap.provenance.fingerprint();
        let vkey = version_key(&snap.version, &snap.git_sha);
        for run in &snap.runs {
            version.push(snap.version.clone());
            git_sha.push(snap.git_sha.clone());
            version_key_col.push(vkey.clone());
            fingerprint.push(fp.clone());
            wall_ci95.push(run.stats.as_ref().map(|s| s.wall_ms_ci95).unwrap_or(0.0));
            rss_ci95.push(run.stats.as_ref().map(|s| s.rss_mb_ci95).unwrap_or(0.0));
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
            tracked_memory_mb.push(run.tracked_memory_mb());
            peak_rss_mb.push(run.peak_rss_mb());
            tiles_produced.push(run.tiles_produced);
            tiles_per_second.push(run.tiles_per_second());
            tiles_per_second_per_mb.push(run.tiles_per_second_per_mb());
        }
    }

    df!(
        "version" => version,
        "git_sha" => git_sha,
        "version_key" => version_key_col,
        "fingerprint" => fingerprint,
        "wall_ci95" => wall_ci95,
        "rss_ci95" => rss_ci95,
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
        "tracked_memory_mb" => tracked_memory_mb,
        "peak_rss_mb" => peak_rss_mb,
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
fn write_markdown_report(df: &DataFrame, versions: &[String], path: &PathBuf) {
    let mut out = String::new();
    out.push_str("# Cross-version benchmark report\n\n");
    out.push_str(
        "Generated by `cross_version` from `benchmark_history.json`. \
         Each cell shows the metric value followed by the percentage \
         delta vs the previous version. Columns are one per \
         `version@short_sha` on record, ordered by (semver, timestamp).\n\n",
    );

    // The size axis is derived from the frame; the version (column) axis is
    // supplied pre-ordered by (semver, timestamp) so releases read in release
    // order rather than lexicographically.
    let sizes = unique_size_concurrency_combos(df);

    if versions.len() < 2 {
        out.push_str(
            "_Only one version in the history file — version-over-version deltas will be empty._\n\n",
        );
    }

    for (w, h, c) in &sizes {
        let mp = (*w as f64) * (*h as f64) / 1_000_000.0;
        out.push_str(&format!("## {w}×{h} ({mp:.1} MP), concurrency = {c}\n\n",));
        for metric in [
            ("wall_time_ms", "Wall time (ms)", false, "wall_ci95"),
            ("tracked_memory_mb", "Tracked working set (MB)", false, ""),
            ("peak_rss_mb", "Peak RSS (MB)", false, "rss_ci95"),
            ("tiles_per_second", "Throughput (tiles/s)", true, ""),
            (
                "tiles_per_second_per_mb",
                "Efficiency (tiles/s/RSS-MB)",
                true,
                "",
            ),
        ] {
            let (col, title, higher_is_better, ci_col) = metric;
            out.push_str(&format!("### {title}\n\n"));
            out.push_str(&render_metric_table(
                df,
                *w,
                *h,
                *c,
                col,
                ci_col,
                versions,
                higher_is_better,
            ));
            out.push('\n');
        }
    }

    let mut file = fs::File::create(path).expect("open md");
    file.write_all(out.as_bytes()).expect("write md");
}

fn unique_size_concurrency_combos(df: &DataFrame) -> Vec<(u32, u32, u32)> {
    // Use lazy + group_by to get distinct triples, then materialise.
    let lf = df
        .clone()
        .lazy()
        .select([col("width"), col("height"), col("concurrency")])
        .unique(None, UniqueKeepStrategy::Any);
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
        .filter_map(|i| Some((widths.get(i)?, heights.get(i)?, concs.get(i)?)))
        .collect()
}

fn unique_string_column(df: &DataFrame, name: &str) -> Vec<String> {
    let s = df.column(name).unwrap().unique_stable().expect("unique");
    let chunked = s.str().expect("str column");
    chunked.into_iter().flatten().map(String::from).collect()
}

/// Render one metric as a markdown table for a fixed (w, h, c). Rows
/// are engines; columns are versions; cells are `value (Δ%)`.
#[allow(clippy::too_many_arguments)]
fn render_metric_table(
    df: &DataFrame,
    w: u32,
    h: u32,
    c: u32,
    metric_col: &str,
    ci_col: &str,
    versions: &[String],
    higher_is_better: bool,
) -> String {
    // Filter to the (w, h, c) slice; keep the metric, CI, fingerprint, and
    // grouping cols. The version axis is keyed by `version@short_sha`.
    let mut cols = vec![
        col("engine"),
        col("version_key"),
        col("fingerprint"),
        col(metric_col),
    ];
    if !ci_col.is_empty() {
        cols.push(col(ci_col));
    }
    let filtered = df
        .clone()
        .lazy()
        .filter(
            col("width")
                .eq(lit(w))
                .and(col("height").eq(lit(h)))
                .and(col("concurrency").eq(lit(c))),
        )
        .select(cols)
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
        // Track the previous version's value, CI, and environment so a
        // delta is only a *regression call* when the change clears both
        // versions' confidence intervals AND was measured in the same
        // environment (issues #155, #159).
        let mut prev: Option<(f64, f64, String)> = None;
        for v in versions {
            let cell = lookup_row(&filtered, engine, v, metric_col, ci_col);
            match cell {
                Some((value, ci, fp)) => {
                    let delta_str = match &prev {
                        Some((p, prev_ci, prev_fp)) if *p != 0.0 => {
                            let d = ((value - p) / p) * 100.0;
                            let sign = if d >= 0.0 { "+" } else { "" };
                            let abs_delta = (value - p).abs();
                            let ci_sum = ci + prev_ci;
                            let arrow = if prev_fp != &fp {
                                // Cross-environment: not apples-to-apples.
                                " (env≠)"
                            } else if d.abs() < 0.05 {
                                ""
                            } else if ci_sum > 0.0 && abs_delta <= ci_sum {
                                // Within combined CI — statistically a tie.
                                " ≈"
                            } else if (d > 0.0) == higher_is_better {
                                " ✅"
                            } else {
                                " ⚠️"
                            };
                            format!(" ({sign}{d:.1}%{arrow})")
                        }
                        _ => String::new(),
                    };
                    out.push_str(&format!("| {value:.2}{delta_str} "));
                    prev = Some((value, ci, fp));
                }
                None => out.push_str("| — "),
            }
        }
        out.push_str("|\n");
    }
    out
}

/// Look up (metric value, CI half-width, environment fingerprint) for one
/// (engine, version_key) cell. CI is 0 when the metric has no CI column.
///
/// If a `version_key` collides — the same `version@short_sha` benchmarked in two
/// separate snapshots, e.g. HEAD re-run with no new commit — this keeps the
/// first matching row (`get(0)`) and the other measurement does not inform the
/// cell. Distinct commits already get distinct keys via the `@short_sha`
/// suffix, so a collision only happens for genuine re-runs of the identical
/// build.
fn lookup_row(
    df: &DataFrame,
    engine: &str,
    version_key: &str,
    metric_col: &str,
    ci_col: &str,
) -> Option<(f64, f64, String)> {
    let frame = df
        .clone()
        .lazy()
        .filter(
            col("engine")
                .eq(lit(engine))
                .and(col("version_key").eq(lit(version_key))),
        )
        .collect()
        .ok()?;
    if frame.height() == 0 {
        return None;
    }
    let value = frame.column(metric_col).ok()?.f64().ok()?.get(0)?;
    let ci = if ci_col.is_empty() {
        0.0
    } else {
        frame
            .column(ci_col)
            .ok()
            .and_then(|s| s.f64().ok().and_then(|c| c.get(0)))
            .unwrap_or(0.0)
    };
    let fp = frame
        .column("fingerprint")
        .ok()
        .and_then(|s| s.str().ok().and_then(|c| c.get(0)))
        .unwrap_or("unknown")
        .to_string();
    Some((value, ci, fp))
}

// Suppress an unused import warning when both items get used inside a
// helper that inlines them.
#[allow(dead_code)]
fn _ensure_uses(_: &RunMetrics) {}
