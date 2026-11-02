//! Measure the `format` family's synthetic control and print one JSON row per cell, plus the
//! content classes that cannot be measured yet and why.

use libviprs_bench::format::runner::{ClassStatus, content_classes, run_synthetic_control};
use serde_json::json;

fn main() -> std::process::ExitCode {
    for (class, status) in content_classes() {
        if let ClassStatus::Blocked(why) = status {
            println!(
                "{}",
                json!({"class": class, "status": "blocked", "why": why})
            );
        }
    }
    match run_synthetic_control(5) {
        Ok(rows) => {
            for r in rows {
                println!(
                    "{}",
                    json!({
                        "class": r.class, "level": r.level, "format": r.format, "tiles": r.tiles,
                        "bytes_before_dedupe": r.bytes_before_dedupe,
                        "bytes_after_dedupe": r.bytes_after_dedupe,
                        "distinct_payloads": r.distinct_payloads,
                        "encode_ms_p50": r.encode_ms_p50, "encode_ms_p99": r.encode_ms_p99,
                        "cpu_secs": r.cpu_secs, "exact_match_rate": r.exact_match_rate,
                        "ink_iou": r.ink_iou, "edge_displacement": r.edge_displacement,
                        "platform": std::env::consts::ARCH,
                    })
                );
            }
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}
