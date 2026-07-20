//! Regression test for per-run RSS isolation (issue #157).
//!
//! Peak RSS used to be `getrusage(RUSAGE_SELF).ru_maxrss` — a monotonic,
//! process-wide high-water mark shared by every in-process engine. In that
//! world a SMALL-image run performed after a LARGE-image run reported the
//! LARGE run's peak, because the watermark never comes back down. Every
//! memory number was therefore contaminated by whatever ran earlier.
//!
//! The fix runs each cell in its own child process and reads that child's
//! `ru_maxrss` via `wait4`. This test performs the exact scenario that used
//! to fail: measure a large cell, then a small cell, and assert the small
//! cell reports the SMALL peak — proving the watermark does not leak across
//! runs.

use std::path::Path;

use libviprs_bench::harness::{CellSpec, Engine, spawn_single_cell};

fn spec(engine: Engine, w: u32, h: u32) -> CellSpec {
    CellSpec {
        engine,
        width: w,
        height: h,
        concurrency: 1,
        tile_size: 256,
        budget_bytes: 1_000_000,
    }
}

#[test]
fn small_run_after_large_run_reports_small_rss() {
    let exe = Path::new(env!("CARGO_BIN_EXE_report"));

    // Large first, so a shared/monotonic watermark would be high when the
    // small run follows.
    let large = spawn_single_cell(exe, spec(Engine::Monolithic, 4096, 4096))
        .expect("large single cell must produce metrics");
    let small = spawn_single_cell(exe, spec(Engine::Monolithic, 256, 256))
        .expect("small single cell must produce metrics");

    let large_mb = large.peak_rss_mb();
    let small_mb = small.peak_rss_mb();

    assert!(
        large_mb > 0.0,
        "large RSS should be measured, got {large_mb}"
    );
    assert!(
        small_mb > 0.0,
        "small RSS should be measured, got {small_mb}"
    );

    // The whole point: the small run, executed AFTER the large run, reports
    // a strictly smaller peak. Under the old shared-watermark scheme these
    // would be equal (both the large peak).
    assert!(
        small_mb < large_mb,
        "small-after-large must report the small RSS (isolation): \
         small={small_mb:.1} MB, large={large_mb:.1} MB"
    );

    // And the gap should be on the order of the 4096² RGB canvas (~48 MB),
    // not rounding noise — proof the large canvas really did inflate only
    // the large run's process.
    assert!(
        large_mb - small_mb > 10.0,
        "expected a large-vs-small RSS gap from the 4096² canvas, \
         got small={small_mb:.1} MB large={large_mb:.1} MB"
    );
}

#[test]
fn single_cell_reports_expected_tile_grid() {
    // A single cell round-trips a real pyramid: PNG-only tile count is
    // non-zero and the per-level grid is populated (feeds the equivalence
    // gate).
    let exe = Path::new(env!("CARGO_BIN_EXE_report"));
    let m = spawn_single_cell(exe, spec(Engine::Monolithic, 1024, 1024))
        .expect("cell must produce metrics");
    assert!(m.tiles_produced > 0, "expected PNG tiles, got 0");
    assert!(
        !m.per_level_tiles.is_empty(),
        "per-level grid should be populated"
    );
    assert_eq!(
        m.per_level_tiles.iter().sum::<u64>(),
        m.tiles_produced,
        "per-level tiles must sum to the total"
    );
}
