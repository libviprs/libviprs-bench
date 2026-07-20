//! #46: streaming / mapreduce bench cells must fail SOFT (no panic, no leak).
//!
//! [`bench_streaming`](libviprs_bench::bench_streaming) and
//! [`bench_mapreduce`](libviprs_bench::bench_mapreduce) run under the strict
//! [`BudgetPolicy::Error`]. Before #46 they ended in `.run().unwrap()`, so a
//! genuine (non-budget) engine fault — a sink IO error, a source error, or the
//! entry-time plan/source dimension guard — PANICKED the whole report/sweep
//! and, because the panic fired before the `remove_dir_all`, leaked the temp
//! output dir under `$TMPDIR`. #38 removed only the *budget-driven* trigger of
//! that panic; the fragile pattern itself remained.
//!
//! These guards drive a real, deterministic engine fault — a plan built for
//! larger dimensions than the source raster, which every engine kind rejects
//! up front with [`EngineError::PlanSourceMismatch`](libviprs::EngineError)
//! (validated at `run()` entry) — and pin that the bench cell:
//!   * does NOT unwind — so the report/sweep driver degrades it to a skipped
//!     row instead of aborting the whole engine series; and
//!   * leaves NO temp output dir behind — the reclaim runs on the error path.
//!
//! They need no libvips (pure-Rust libviprs engines), so they run
//! unconditionally. They are written with [`catch_unwind`] so the SAME
//! assertion — "the call returned rather than unwinding" — is meaningful
//! against both the pre-#46 panicking signature and the post-#46 `Result` one.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use libviprs::{Layout, PyramidPlan, PyramidPlanner, Raster};
use libviprs_bench::{
    BENCH_STREAMING_BUDGET, BENCH_TILE_SIZE, bench_mapreduce, bench_streaming, gradient_raster,
};

/// A 256×256 source paired with a plan built for 1024×1024. The engine's
/// entry-time plan/source dimension guard rejects this up front with
/// `PlanSourceMismatch` — a genuine, non-budget engine fault that is fully
/// deterministic and needs no filesystem tampering to provoke.
fn mismatched_source_and_plan() -> (Raster, PyramidPlan) {
    let src = gradient_raster(256, 256);
    let plan = PyramidPlanner::new(1024, 1024, BENCH_TILE_SIZE, 0, Layout::DeepZoom)
        .expect("plan the (deliberately mismatched) pyramid")
        .plan();
    (src, plan)
}

/// The temp output dir a cell roots its tiles at — mirrors the crate's private
/// `fs_sink_dir` layout (`TMPDIR/libviprs-bench/engine_{pid}_{label}`) so the
/// test can assert the cell left nothing behind on the error path.
fn cell_out_dir(label: &str) -> PathBuf {
    std::env::temp_dir()
        .join("libviprs-bench")
        .join(format!("engine_{}_{label}", std::process::id()))
}

#[test]
fn streaming_cell_fault_does_not_panic_or_leak() {
    let (src, plan) = mismatched_source_and_plan();
    let label = "budget46_streaming_fault";
    let out_dir = cell_out_dir(label);
    let _ = std::fs::remove_dir_all(&out_dir);

    // #46: a genuine engine fault must be RETURNED, not unwound up through the
    // report/sweep driver (which would drop the whole streaming series).
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        bench_streaming(&src, &plan, 0, BENCH_STREAMING_BUDGET, label)
    }));
    assert!(
        outcome.is_ok(),
        "bench_streaming unwound on an engine fault instead of returning \
         (issue #46: the whole report/sweep would abort)"
    );

    // #46: and the temp output dir must be reclaimed on the error path.
    assert!(
        !out_dir.exists(),
        "bench_streaming leaked its temp output dir {} on the error path (issue #46)",
        out_dir.display()
    );
}

#[test]
fn mapreduce_cell_fault_does_not_panic_or_leak() {
    let (src, plan) = mismatched_source_and_plan();
    let label = "budget46_mapreduce_fault";
    let out_dir = cell_out_dir(label);
    let _ = std::fs::remove_dir_all(&out_dir);

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        bench_mapreduce(&src, &plan, 0, BENCH_STREAMING_BUDGET, label)
    }));
    assert!(
        outcome.is_ok(),
        "bench_mapreduce unwound on an engine fault instead of returning \
         (issue #46: the whole report/sweep would abort)"
    );
    assert!(
        !out_dir.exists(),
        "bench_mapreduce leaked its temp output dir {} on the error path (issue #46)",
        out_dir.display()
    );
}
