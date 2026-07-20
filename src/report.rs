//! Comprehensive benchmark report generator.
//!
//! Runs all three engines across a matrix of image sizes and concurrency
//! levels, collects metrics (CPU time, memory, throughput), and writes:
//!
//!   report/benchmark_results.json  — raw metrics for this run
//!   report/benchmark_history.json  — versioned history across releases
//!   report/comparison_table.txt    — human-readable summary
//!   report/chart_wall_time.svg     — grouped bar chart of wall time
//!   report/chart_peak_memory.svg   — grouped bar chart of peak memory
//!   report/chart_throughput.svg    — grouped bar chart of throughput
//!   report/chart_history_*.svg     — trend lines across versions
//!
//! Run: cargo run --release --bin report
//!
//! Use --release for meaningful timing numbers.

use std::fs;
use std::path::Path;

use libviprs_bench::harness::{self, Engine};
use libviprs_bench::provenance::{OracleMatch, Provenance};
use libviprs_bench::{
    BENCH_STREAMING_BUDGET, BENCH_TILE_SIZE, DEFAULT_CONCURRENCY, DEFAULT_SIZES, core_git_sha,
    core_version, create_snapshot, executive_verdict, generate_charts, generate_history_chart,
    load_history, print_comparison_table, print_savings_summary, save_history, vips_available,
};

fn main() {
    // Hidden per-cell child subcommand (`--single …`). When invoked this
    // way the process runs exactly one cell and prints its metrics as JSON;
    // the parent harness spawns these and reads each child's true per-run
    // RSS via wait4 (issue #157). Not a `--single` invocation → fall through
    // to the normal report run.
    if let Some(code) = harness::maybe_run_single_subcommand() {
        std::process::exit(code);
    }

    // Hidden `--print-core`: the version-matrix runner rebuilds this binary per
    // tag and asks it which core it linked, to verify the measured artifact's
    // identity matches the ref before recording a snapshot (issue #19).
    if let Some(code) = harness::maybe_run_print_core_subcommand() {
        std::process::exit(code);
    }

    let report_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("report");
    fs::create_dir_all(&report_dir).unwrap();

    // The canonical suite, shared with the version-matrix runner so the
    // everyday axis and the release-history axis measure the identical sizes,
    // concurrency, tile size, and budget (issue #19).
    let sizes: &[(u32, u32)] = DEFAULT_SIZES;
    let concurrency_levels: &[usize] = DEFAULT_CONCURRENCY;
    let tile_size: u32 = BENCH_TILE_SIZE;
    let streaming_budget: u64 = BENCH_STREAMING_BUDGET; // 1 MB

    // Statistics: >= 7 timed iterations after a discarded warm-up, each cell
    // in its own child process, engine order interleaved within a size
    // (issue #155). Override with BENCH_ITERS / BENCH_WARMUP for a fast
    // smoke run.
    let iters: u32 = std::env::var("BENCH_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(harness::DEFAULT_ITERS);
    let warmup: u32 = std::env::var("BENCH_WARMUP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(harness::DEFAULT_WARMUP);

    let prov = Provenance::capture();
    println!("=== libviprs vs libvips comparison benchmark ===");
    println!(
        "    measured libviprs core: {} ({})",
        core_version(),
        core_git_sha()
    );
    println!("    bench harness: {}", env!("CARGO_PKG_VERSION"));
    println!("    environment:  {}", prov.fingerprint());
    println!("    cpu: {} ({} cpus)", prov.host.cpu_model, prov.host.ncpu);
    println!(
        "    libvips oracle: measured {} / pinned {}",
        prov.libvips_version, prov.pinned_libvips_version
    );
    // Mismatched-oracle guard (#33): if this run measured a different libvips
    // than the environment was pinned to build, its numbers are not comparable
    // to a pinned-oracle run — say so loudly on stderr. Only fires on a genuine
    // parsed mismatch, never on a host run that simply has no libvips.
    if let OracleMatch::Mismatch { measured, pinned } = prov.libvips_oracle_match() {
        eprintln!(
            "WARNING: measured libvips {}.{} != pinned oracle {}.{} — this run \
             measured a different libvips than the environment was pinned to \
             build (issue #33); its numbers are NOT comparable to a \
             pinned-oracle run.",
            measured.0, measured.1, pinned.0, pinned.1
        );
    }
    println!();
    println!("Tile size: {tile_size}, memory budget: {streaming_budget} bytes");
    println!("Iterations: {iters} timed + {warmup} warm-up per cell (child-isolated)");
    println!(
        "Image sizes: {:?}",
        sizes
            .iter()
            .map(|(w, h)| format!("{w}x{h}"))
            .collect::<Vec<_>>()
    );
    println!("Concurrency levels: {concurrency_levels:?}");
    println!();

    // Engine set: libviprs engines always; libvips only when present.
    let mut engines = vec![Engine::Monolithic, Engine::Streaming, Engine::MapReduce];
    if vips_available() {
        eprintln!("libvips detected — including in benchmarks");
        engines.push(Engine::Libvips);
    } else {
        eprintln!("libvips not found — skipping libvips benchmarks");
    }

    let exe = harness::current_exe();
    let results = harness::run_isolated_suite(
        &exe,
        sizes,
        concurrency_levels,
        &engines,
        tile_size,
        streaming_budget,
        iters,
        warmup,
    );

    // Print full table
    print_comparison_table(&results);

    // Print savings summary
    print_savings_summary(&results);

    // Executive verdict: per size, the winning engine on each axis plus
    // every engine's ratio vs libvips in the *same* snapshot (issue #160).
    let verdict = executive_verdict(&results);
    println!();
    print!("{verdict}");
    fs::write(report_dir.join("verdict_table.txt"), &verdict).unwrap();

    // Write JSON for this run
    let json_path = report_dir.join("benchmark_results.json");
    let json = serde_json::to_string_pretty(&results).unwrap();
    fs::write(&json_path, &json).unwrap();
    println!();
    println!("JSON results written to {}", json_path.display());

    // Write text report
    let txt_path = report_dir.join("comparison_table.txt");
    let mut txt = String::new();
    txt.push_str(&format!(
        "libviprs engine comparison benchmark (monolithic / streaming / mapreduce)\n\
         measured libviprs core: {} ({})\n\
         bench harness: {}\n\
         Tile size: {tile_size}, memory budget: {streaming_budget} bytes\n\n",
        core_version(),
        core_git_sha(),
        env!("CARGO_PKG_VERSION"),
    ));

    txt.push_str(&format!(
        "{:<24} {:<12} {:>10} {:>12} {:>10} {:>8} {:>8} {:>10} {:>12}\n",
        "Label",
        "Engine",
        "Time (ms)",
        "Tracked MB",
        "RSS MB",
        "Tiles",
        "T/s",
        "T/s/RSS-MB",
        "RSS-MB\u{00b7}s/tile"
    ));
    txt.push_str(&format!("{}\n", "-".repeat(112)));

    for r in &results {
        txt.push_str(&format!(
            "{:<24} {:<12} {:>10.1} {:>12.2} {:>10.2} {:>8} {:>8.0} {:>10.1} {:>12.4}\n",
            r.label,
            r.engine,
            r.wall_time_ms(),
            r.tracked_memory_mb(),
            r.peak_rss_mb(),
            r.tiles_produced,
            r.tiles_per_second(),
            r.tiles_per_second_per_mb(),
            r.resource_cost_per_tile(),
        ));
    }
    fs::write(&txt_path, &txt).unwrap();
    println!("Text report written to {}", txt_path.display());

    // --- Generate SVG charts ---
    generate_charts(&results, &report_dir);
    println!();
    println!("Charts written:");
    println!("  {}", report_dir.join("chart_wall_time.svg").display());
    println!("  {}", report_dir.join("chart_peak_memory.svg").display());
    println!(
        "  {}",
        report_dir.join("chart_tracked_memory.svg").display()
    );
    println!("  {}", report_dir.join("chart_throughput.svg").display());
    println!("  {}", report_dir.join("chart_efficiency.svg").display());
    println!("  {}", report_dir.join("chart_resource_cost.svg").display());

    // --- Versioned benchmark history ---
    //
    // If the existing history file is corrupt, I refuse to overwrite it:
    // appending a fresh snapshot would clobber every prior run. I keep
    // the old file in place, report the problem, and skip this run's
    // append so the accumulated history survives for inspection/repair.
    let history_path = report_dir.join("benchmark_history.json");
    println!();
    match load_history(&history_path) {
        Ok(mut history) => {
            let snapshot = create_snapshot(results.clone(), tile_size, streaming_budget);
            history.push(snapshot);
            match save_history(&history_path, &history) {
                Ok(()) => {
                    println!(
                        "Benchmark history updated: {} entries in {}",
                        history.len(),
                        history_path.display()
                    );

                    // Generate history trend charts if we have multiple versions
                    if history.len() >= 2 {
                        for &(w, h) in sizes {
                            for &conc in concurrency_levels {
                                generate_history_chart(&history, &report_dir, (w, h), conc);
                            }
                        }
                        println!("History trend charts written to {}", report_dir.display());
                    } else {
                        println!("(run again on a different version to generate trend charts)");
                    }
                }
                Err(e) => {
                    eprintln!("warning: {e}");
                    eprintln!("This run's snapshot was not persisted; prior history is intact.");
                }
            }
        }
        Err(e) => {
            eprintln!("warning: {e}");
            eprintln!(
                "Leaving {} untouched so prior history is not discarded.",
                history_path.display()
            );
            eprintln!("Fix or move the file, then re-run to resume appending snapshots.");
            eprintln!("Skipping history trend charts for this run.");
        }
    }
}
