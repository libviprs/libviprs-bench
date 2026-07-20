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

use libviprs::streaming::BudgetPolicy;
use libviprs::{
    EngineBuilder, EngineConfig, EngineKind, Layout, MemorySink, PyramidPlan, PyramidPlanner,
    Raster, RasterStripSource,
};
use libviprs_bench::{
    BENCH_STREAMING_BUDGET, BENCH_TILE_SIZE, bench_mapreduce, bench_streaming, gradient_raster,
    streaming_budget_for,
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

// ---------------------------------------------------------------------------
// #47: the re-derived budget-admission formula, pinned to core reality.
// ---------------------------------------------------------------------------

/// Whether the REAL streaming engine admits a run at `budget` — the observable
/// image of the core's up-front pre-flight gate. Uses an in-memory sink so the
/// probe touches no filesystem.
fn streaming_admits(plan: &PyramidPlan, src: &Raster, budget: u64) -> bool {
    let sink = MemorySink::new();
    EngineBuilder::new(RasterStripSource::new(src), plan.clone(), &sink)
        .with_engine(EngineKind::Streaming)
        .with_config(EngineConfig::default())
        .with_memory_budget(budget)
        .with_budget_policy(BudgetPolicy::Error)
        .run()
        .is_ok()
}

/// #47 is resolved WON'T-FIX: [`streaming_budget_for`] keeps re-deriving the
/// streaming / mapreduce engines' pre-flight admission invariant
/// (`canvas_width × 2·tile_size × bpp`) because the core exposes no public
/// inverse to adopt and coupling the deliberately-raw, saturation-guarded,
/// plan-less-callable helper to a `&PyramidPlan` is out of proportion to the
/// coupling (see the helper's docs). This pin gives the duplication teeth by
/// tying it to the engine's ACTUAL behaviour rather than a same-shaped formula:
/// the real engine must REJECT a budget one byte below the invariant and ADMIT
/// one exactly at it. If the core ever changes its admission rule, this goes
/// red — the signal to revisit the "won't-fix". The mapreduce engine gates on
/// the identical invariant, so pinning streaming pins the shared formula.
#[test]
fn streaming_budget_min_strip_agrees_with_core_preflight() {
    let src = gradient_raster(1024, 1024);
    let plan = PyramidPlanner::new(1024, 1024, BENCH_TILE_SIZE, 0, Layout::DeepZoom)
        .expect("plan the pyramid")
        .plan();
    let bpp = src.format().bytes_per_pixel() as u32;

    // The invariant the bench re-derives in `streaming_budget_for`.
    let min_strip = plan.canvas_width as u64 * 2 * plan.tile_size as u64 * bpp as u64;

    // Reality check: the real engine rejects one byte below the invariant and
    // admits exactly at it, so the bench's re-derived threshold IS the core's
    // pre-flight gate — not merely a same-shaped formula (issue #47).
    assert!(
        !streaming_admits(&plan, &src, min_strip - 1),
        "core must reject a budget one byte below its worst-case-strip pre-flight gate"
    );
    assert!(
        streaming_admits(&plan, &src, min_strip),
        "core must admit a budget exactly at its worst-case-strip pre-flight gate"
    );

    // And the shared sizing helper sizes strictly above that gate (the fixed 2×
    // margin), so every streaming-family bench path clears pre-flight with slack.
    let sized = streaming_budget_for(0, plan.canvas_width, plan.tile_size, bpp);
    assert_eq!(sized, min_strip * 2, "the fixed 2× pre-flight margin");
    assert!(
        sized >= min_strip,
        "the sized budget must admit the gate strip"
    );
}
