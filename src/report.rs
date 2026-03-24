//! Comprehensive benchmark report generator.
//!
//! Runs both engines across a matrix of image sizes and concurrency levels,
//! collects metrics (CPU time, memory, throughput), and writes:
//!
//!   report/benchmark_results.json  — raw metrics for external tooling
//!   report/comparison_table.txt    — human-readable summary
//!
//! Run: cargo run --release --bin report
//!
//! Use --release for meaningful timing numbers.

use std::fs;
use std::path::Path;

use libviprs_bench::{comparison_suite, print_comparison_table, print_savings_summary};

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

    println!("=== libviprs engine comparison benchmark ===");
    println!();
    println!("Tile size: {tile_size}, streaming budget: {streaming_budget} bytes");
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

    // Write JSON for external tooling
    let json_path = report_dir.join("benchmark_results.json");
    let json = serde_json::to_string_pretty(&results).unwrap();
    fs::write(&json_path, &json).unwrap();
    println!();
    println!("JSON results written to {}", json_path.display());

    // Write text report
    let txt_path = report_dir.join("comparison_table.txt");
    let mut txt = String::new();
    txt.push_str("libviprs engine comparison benchmark\n");
    txt.push_str(&format!(
        "Tile size: {tile_size}, streaming budget: {streaming_budget} bytes\n\n"
    ));

    txt.push_str(&format!(
        "{:<24} {:<12} {:>10} {:>12} {:>10} {:>10} {:>8}\n",
        "Label", "Engine", "Time (ms)", "Memory (MB)", "Tiles", "Tiles/s", "Strips"
    ));
    txt.push_str(&format!("{}\n", "-".repeat(90)));

    for r in &results {
        txt.push_str(&format!(
            "{:<24} {:<12} {:>10.1} {:>12.2} {:>10} {:>10.0} {:>8}\n",
            r.label,
            r.engine,
            r.wall_time_ms(),
            r.peak_memory_mb(),
            r.tiles_produced,
            r.tiles_per_second(),
            r.strips,
        ));
    }
    fs::write(&txt_path, &txt).unwrap();
    println!("Text report written to {}", txt_path.display());
}
