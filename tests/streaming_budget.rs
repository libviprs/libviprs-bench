//! Report-path memory-budget sizing for the streaming / mapreduce engines (#38).
//!
//! The everyday `report` run drives every streaming and mapreduce cell through
//! [`bench_streaming`](libviprs_bench::bench_streaming) /
//! [`bench_mapreduce`](libviprs_bench::bench_mapreduce) with a single flat
//! ~1 MB budget ([`BENCH_STREAMING_BUDGET`](libviprs_bench::BENCH_STREAMING_BUDGET))
//! under the strict `BudgetPolicy::Error`. That budget is smaller than the
//! worst-case tile-aligned strip (`width × 2·tile_size × bpp`) the engines must
//! admit once the canvas reaches 1024 px, so at the large report sizes
//! (1024 / 2048 / 4096) both engines rejected the run up front with
//! `BudgetExceeded` — and the harness `.unwrap()` turned that into a panic,
//! silently dropping the streaming and mapreduce rows from the report while the
//! monolithic and libvips rows survived.
//!
//! These guards pin that a large cell now completes under the report's default
//! budget and yields a valid pyramid, and that the budget the cell actually ran
//! with was sized large enough to admit that worst-case strip. They need no
//! libvips, so they run unconditionally.

use libviprs::{Layout, PyramidPlanner};
use libviprs_bench::{
    BENCH_STREAMING_BUDGET, BENCH_TILE_SIZE, RunMetrics, bench_mapreduce, bench_streaming,
    gradient_raster,
};

/// A large-enough canvas that the report's flat 1 MB budget cannot fit the
/// worst-case tile-aligned strip: `1024 × 2·256 × 3 = 1_572_864` B > 1 MB.
/// Kept at exactly 1024² (a handful of tiles) so the run stays fast.
const W: u32 = 1024;
const H: u32 = 1024;
/// [`gradient_raster`] is RGB8, so the engine's pre-flight admits a worst-case
/// strip of `width × 2·tile_size × bpp` at 3 bytes per pixel.
const BPP: u64 = 3;

/// The minimum aligned strip the streaming/mapreduce pre-flight must admit at
/// [`W`] — mirror of the engine's own `canvas_width × 2·tile_size × bpp`.
fn worst_case_strip_bytes() -> u64 {
    W as u64 * 2 * BENCH_TILE_SIZE as u64 * BPP
}

/// Shared assertions: the cell produced a valid, non-empty pyramid and ran
/// under a budget sized to admit the worst-case tile-aligned strip.
fn assert_valid_large_pyramid(m: &RunMetrics, engine: &str) {
    assert_eq!(m.engine, engine, "row must report its own engine series");
    assert!(
        m.tiles_produced > 0,
        "{engine} must emit a non-empty pyramid at {W}x{H} under the report's \
         default budget, got {} tiles",
        m.tiles_produced
    );
    assert!(
        !m.per_level_tiles.is_empty(),
        "per-level grid must be populated"
    );
    assert_eq!(
        m.per_level_tiles.iter().sum::<u64>(),
        m.tiles_produced,
        "per-level tiles must sum to the total"
    );
    assert!(
        m.strips > 0,
        "the {engine} path must render at least one strip"
    );
    // Regression guard: the budget the cell actually ran with (reported back in
    // `memory_budget_bytes`) must be large enough to admit the worst-case
    // tile-aligned strip, otherwise the strict `BudgetPolicy::Error` would have
    // tripped `BudgetExceeded`.
    assert!(
        m.memory_budget_bytes >= worst_case_strip_bytes(),
        "sized budget {} must admit the worst-case tile-aligned strip {} \
         (width x 2*tile_size x bpp)",
        m.memory_budget_bytes,
        worst_case_strip_bytes()
    );
}

/// The report's default 1 MB budget must not make the streaming engine panic
/// with `BudgetExceeded` at 1024²; it must degrade to a properly-sized budget
/// and produce a valid pyramid. RED against the pre-fix code, where
/// `bench_streaming` `.unwrap()`s the over-budget engine run and panics.
#[test]
fn report_default_budget_admits_large_streaming_pyramid() {
    let src = gradient_raster(W, H);
    let plan = PyramidPlanner::new(W, H, BENCH_TILE_SIZE, 0, Layout::DeepZoom)
        .expect("plan the pyramid")
        .plan();

    // Exactly the budget/policy the report drives every streaming cell with.
    let m = bench_streaming(&src, &plan, 0, BENCH_STREAMING_BUDGET, "budget38_stream")
        .expect("the sized budget must admit the streaming run, not error");

    assert_valid_large_pyramid(&m, "streaming");
}

/// The mapreduce counterpart: the same flat budget must not panic the
/// mapreduce engine at 1024², which likewise rejected the run up front before
/// the budget was sized per image.
#[test]
fn report_default_budget_admits_large_mapreduce_pyramid() {
    let src = gradient_raster(W, H);
    let plan = PyramidPlanner::new(W, H, BENCH_TILE_SIZE, 0, Layout::DeepZoom)
        .expect("plan the pyramid")
        .plan();

    let m = bench_mapreduce(&src, &plan, 0, BENCH_STREAMING_BUDGET, "budget38_mr")
        .expect("the sized budget must admit the mapreduce run, not error");

    assert_valid_large_pyramid(&m, "mapreduce");
}
