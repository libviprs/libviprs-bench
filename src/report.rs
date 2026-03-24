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

use libviprs_bench::{
    comparison_suite, create_snapshot, generate_charts, generate_history_chart, load_history,
    print_comparison_table, print_savings_summary, save_history,
};

fn main() {
    let report_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("report");
    fs::create_dir_all(&report_dir).unwrap();

    let sizes: &[(u32, u32)] = &[
        (512, 512),
        (1024, 1024),
        (2048, 2048),
        (4096, 4096),
    ];
    let concurrency_levels: &[usize] = &[0, 4];
    let tile_size: u32 = 256;
    let streaming_budget: u64 = 1_000_000; // 1 MB

    println!("=== libviprs engine comparison benchmark (monolithic / streaming / mapreduce) ===");
    println!("    version: {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Tile size: {tile_size}, memory budget: {streaming_budget} bytes");
    println!(
        "Image sizes: {:?}",
        sizes.iter().map(|(w, h)| format!("{w}x{h}")).collect::<Vec<_>>()
    );
    println!("Concurrency levels: {concurrency_levels:?}");
    println!();

    let results = comparison_suite(sizes, concurrency_levels, tile_size, streaming_budget);

    // Print full table
    print_comparison_table(&results);

    // Print savings summary
    print_savings_summary(&results);

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
         version: {}\n\
         Tile size: {tile_size}, memory budget: {streaming_budget} bytes\n\n",
        env!("CARGO_PKG_VERSION"),
    ));

    txt.push_str(&format!(
        "{:<24} {:<12} {:>10} {:>10} {:>8} {:>8} {:>10} {:>12}\n",
        "Label", "Engine", "Time (ms)", "Mem (MB)", "Tiles", "T/s", "T/s/MB", "MB\u{00b7}s/tile"
    ));
    txt.push_str(&format!("{}\n", "-".repeat(100)));

    for r in &results {
        txt.push_str(&format!(
            "{:<24} {:<12} {:>10.1} {:>10.2} {:>8} {:>8.0} {:>10.1} {:>12.4}\n",
            r.label,
            r.engine,
            r.wall_time_ms(),
            r.peak_memory_mb(),
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
    println!("  {}", report_dir.join("chart_throughput.svg").display());
    println!("  {}", report_dir.join("chart_efficiency.svg").display());
    println!("  {}", report_dir.join("chart_resource_cost.svg").display());

    // --- Versioned benchmark history ---
    let history_path = report_dir.join("benchmark_history.json");
    let mut history = load_history(&history_path);

    let snapshot = create_snapshot(results.clone(), tile_size, streaming_budget);
    history.push(snapshot);

    save_history(&history_path, &history);
    println!();
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
