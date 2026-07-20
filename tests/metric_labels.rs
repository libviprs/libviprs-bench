//! Metric-labelling guard (#25).
//!
//! Every figure the report prints must state its UNIT and its DIRECTION (higher-
//! or lower-is-better) so a reader never has to guess. In particular the primary
//! throughput metric is TILES per second (never pixels per second), and the
//! ratio metrics (efficiency, resource cost) must say which way is better. This
//! pins the human-facing comparison table and the executive verdict.

use std::time::Duration;

use libviprs_bench::{RunMetrics, RunStats, comparison_table, executive_verdict};

/// One config, two engines. `tiles_produced` is large enough to exercise the
/// thousands separator in the table.
fn sample() -> Vec<RunMetrics> {
    vec![
        run(
            "2048x2048_c0_monolithic",
            "monolithic",
            1_250.0,
            349_525,
            512.0,
        ),
        run("2048x2048_c0_vips", "libvips", 900.0, 349_525, 300.0),
    ]
}

fn run(label: &str, engine: &str, wall_ms: f64, tiles: u64, rss_mb: f64) -> RunMetrics {
    RunMetrics {
        label: label.to_string(),
        width: 2048,
        height: 2048,
        engine: engine.to_string(),
        measurement_path: String::new(),
        wall_time: Duration::from_secs_f64(wall_ms / 1000.0),
        tracked_memory_bytes: 16_777_216,
        peak_rss_bytes: (rss_mb * 1024.0 * 1024.0) as u64,
        stats: Some(RunStats {
            n: 7,
            wall_ms_median: wall_ms,
            wall_ms_min: wall_ms - 3.0,
            wall_ms_iqr: 3.0,
            wall_ms_ci95: 1.5,
            rss_mb_median: rss_mb,
            rss_mb_min: rss_mb - 2.0,
            rss_mb_iqr: 2.0,
            rss_mb_ci95: 1.0,
        }),
        per_level_tiles: vec![256, 64, 16, 4, 1],
        equivalence_psnr_db: Some(48.0),
        tiles_produced: tiles,
        levels_processed: 5,
        tiles_skipped: 0,
        strips: 0,
        batches: 0,
        inflight_strips: 0,
        concurrency: 0,
        memory_budget_bytes: 0,
    }
}

/// The comparison table names the throughput unit as tiles-per-second and spells
/// out every metric's direction, so neither the unit (tiles vs pixels) nor the
/// direction (higher/lower is better) is ever left to a guess.
#[test]
fn comparison_table_states_units_and_direction() {
    let table = comparison_table(&sample());

    // The throughput column + the tiles-not-pixels disambiguation.
    assert!(table.contains("T/s"), "throughput column present");
    assert!(
        table.to_lowercase().contains("tiles per second")
            || table.to_lowercase().contains("tiles/second")
            || table.to_lowercase().contains("tiles written per second"),
        "the throughput unit is spelled out as tiles per second"
    );
    assert!(
        !table.to_lowercase().contains("pixels per second")
            && !table.to_lowercase().contains("pixels/s"),
        "throughput is tiles/s, never pixels/s"
    );

    // Direction for both a higher-is-better and a lower-is-better metric.
    assert!(
        table.contains("higher is better"),
        "throughput / efficiency direction stated"
    );
    assert!(
        table.contains("lower is better"),
        "wall time / resource cost direction stated"
    );

    // The resource-cost and efficiency units appear.
    assert!(
        table.contains("RSS-MB\u{00b7}s/tile"),
        "resource-cost unit present"
    );
    assert!(table.contains("T/s/RSS-MB"), "efficiency unit present");
}

/// Large tile counts read with thousands separators for legibility.
#[test]
fn comparison_table_groups_thousands() {
    let table = comparison_table(&sample());
    assert!(
        table.contains("349,525"),
        "tile counts carry thousands separators, got:\n{table}"
    );
}

/// The executive verdict states the direction of every axis it ranks (wall / RSS
/// lower-is-better, efficiency higher-is-better) and names the efficiency unit.
#[test]
fn executive_verdict_states_units_and_direction() {
    let verdict = executive_verdict(&sample());
    assert!(
        verdict.contains("lower is better"),
        "wall/RSS direction stated"
    );
    assert!(
        verdict.contains("higher is better"),
        "efficiency direction stated"
    );
    assert!(
        verdict.contains("T/s/RSS-MB"),
        "the efficiency unit names its full RSS-MB basis (T/s/RSS-MB), not the ambiguous T/s/MB"
    );
    assert!(
        verdict.contains("T = pyramid tiles"),
        "the T = tiles key is restated so the T/s shorthand is unambiguous"
    );
}
