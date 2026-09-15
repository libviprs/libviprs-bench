//! Per-run RSS isolation, on both paths that measure memory (issues #157, #74).
//!
//! Peak RSS used to be `getrusage(RUSAGE_SELF).ru_maxrss` — a monotonic,
//! process-wide high-water mark shared by every in-process engine. In that
//! world a SMALL-image run performed after a LARGE-image run reported the
//! LARGE run's peak, because the watermark never comes back down. Every
//! memory number was therefore contaminated by whatever ran earlier.
//!
//! The fix runs each cell in its own child process and reads that child's
//! `ru_maxrss` via `wait4`.
//!
//! This file used to guard only [`spawn_single_cell`] — the function the fix
//! landed in. That is why #74 survived: the `scalability` binary, which is what
//! the `engines` family runs and what the published charts are drawn from,
//! never adopted it and went on measuring every engine with one process-wide
//! watermark. Its first full capture reported byte-identical peak RSS for all
//! three engines in twenty of twenty (megapixel, concurrency) groups. So the
//! second half of this file drives the real `scalability` binary and reads the
//! JSON it publishes, which is the only place the defect was ever visible.
//!
//! Covered here: `harness::spawn_single_cell` (the fixed function) and the
//! `scalability` binary's four gradient series (monolithic, streaming,
//! mapreduce, and the libvips row on a `vips` build). NOT covered: the
//! `pdfium`-gated `streaming-pdf` series, which still measures in-process and
//! still says so in the binary's own output.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use serde_json::Value;

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

// ---------------------------------------------------------------------------
// The path that publishes: the `scalability` binary (issue #74).
//
// Everything below drives the real binary as a child process and reads
// `scalability_results.json` — the file the site's charts and the published
// tables are built from. A library call that happens to sit next to it proves
// nothing here: the whole point of #74 is that the library was fixed and the
// binary was not.
// ---------------------------------------------------------------------------

/// The big cell. Tall and narrow on purpose: monolithic memory scales with
/// canvas *area* while streaming memory is bounded by canvas *width* times
/// strip height, so this shape buys an order-of-magnitude gap between the two
/// engines' working sets for ten megapixels of work rather than the ~190 MP the
/// published sweep needs to reach the same ratio. These tests are not a
/// measurement, so the cheap shape is the right one.
const BIG: (u32, u32) = (1024, 10_000);
/// The small cell, measured AFTER the big one (see the sweep's `--sizes`).
const SMALL: (u32, u32) = (512, 360);

/// One sweep of the real `scalability` binary, shared by every test in this
/// section so the suite pays for it once.
///
/// Large size first, then small: that ordering is what a process-wide
/// watermark cannot survive, and it is the order the sweep is asked for.
fn sweep_rows() -> &'static Vec<Value> {
    static ROWS: OnceLock<Vec<Value>> = OnceLock::new();
    ROWS.get_or_init(|| {
        let dir = std::env::temp_dir()
            .join("libviprs-bench-rss74")
            .join(format!("sweep_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the sweep's scratch directory");

        let sizes = format!("{}x{},{}x{}", BIG.0, BIG.1, SMALL.0, SMALL.1);
        let out = Command::new(env!("CARGO_BIN_EXE_scalability"))
            .args([
                "--family",
                "engines",
                "--report-dir",
                dir.to_str().unwrap(),
                "--sizes",
                &sizes,
                "--concurrency",
                "1",
            ])
            .output()
            .expect("run the scalability binary");
        assert!(
            out.status.success(),
            "the scalability sweep must run to completion:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );

        let path = dir.join("scalability_results.json");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("the sweep must write {}: {e}", path.display()));
        let rows: Vec<Value> =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()));
        assert!(!rows.is_empty(), "the sweep published no rows at all");
        rows
    })
}

/// The one row for `engine` at `size`. Panics naming what it could not find,
/// because a missing row means the sweep skipped an engine and every assertion
/// resting on it would otherwise vanish rather than fail.
fn row(engine: &str, size: (u32, u32)) -> &'static Value {
    sweep_rows()
        .iter()
        .find(|r| {
            r["engine"].as_str() == Some(engine)
                && r["width"].as_u64() == Some(size.0 as u64)
                && r["height"].as_u64() == Some(size.1 as u64)
        })
        .unwrap_or_else(|| {
            let seen: Vec<String> = sweep_rows()
                .iter()
                .map(|r| {
                    format!(
                        "{}@{}x{}",
                        r["engine"].as_str().unwrap_or("?"),
                        r["width"].as_u64().unwrap_or(0),
                        r["height"].as_u64().unwrap_or(0)
                    )
                })
                .collect();
            panic!(
                "the sweep must publish a {engine} row at {}x{}; it published: {}",
                size.0,
                size.1,
                seen.join(", ")
            )
        })
}

fn f(row: &Value, key: &str) -> f64 {
    row[key]
        .as_f64()
        .unwrap_or_else(|| panic!("row is missing a numeric {key}: {row}"))
}

/// RED against the implementation #74 is about: `scalability.rs` measuring
/// every engine with its own `process_peak_rss()` —
/// `getrusage(RUSAGE_SELF).ru_maxrss` — with all three engines running in one
/// process. That watermark is process-wide and never comes back down, so
/// monolithic sets it and every engine measured afterwards reports monolithic's
/// number as its own. The first full capture agreed to seven decimal places in
/// twenty of twenty groups, and nothing was red, because no test anywhere
/// asserted that three engines produce three different figures.
#[test]
fn a_sweep_publishes_a_distinct_peak_rss_for_each_engine() {
    let mono = row("monolithic", BIG);
    let stream = row("streaming", BIG);
    let mapreduce = row("mapreduce", BIG);

    // Positive control, before anything rests on the RSS column: these really
    // are two workloads whose memory differs by an order of magnitude, and the
    // engines' OWN accounting says so, in the same JSON, from the same sweep.
    // Without this an assertion that two numbers differ is worth nothing — it
    // could be measuring noise between two runs that should agree.
    let mono_tracked = f(mono, "tracked_memory_mb");
    let stream_tracked = f(stream, "tracked_memory_mb");
    assert!(
        mono_tracked > 0.0 && stream_tracked > 0.0,
        "control: both engines must report a tracked working set, got \
         monolithic={mono_tracked:.2} MB streaming={stream_tracked:.2} MB"
    );
    assert!(
        mono_tracked / stream_tracked >= 8.0,
        "control: at {}x{} monolithic must hold an order of magnitude more than \
         streaming before a peak-RSS difference means anything; got \
         monolithic={mono_tracked:.2} MB streaming={stream_tracked:.2} MB",
        BIG.0,
        BIG.1,
    );

    let mono_rss = f(mono, "peak_rss_mb");
    let stream_rss = f(stream, "peak_rss_mb");
    let mapreduce_rss = f(mapreduce, "peak_rss_mb");

    // The defect exactly as it was captured: byte-identical figures.
    assert_ne!(
        mono_rss, stream_rss,
        "monolithic and streaming published the SAME peak RSS ({mono_rss} MB) — \
         that is one process-wide watermark reported three times, not three \
         measurements (issue #74)"
    );
    assert_ne!(
        mono_rss, mapreduce_rss,
        "monolithic and mapreduce published the SAME peak RSS ({mono_rss} MB) — \
         that is one process-wide watermark reported three times (issue #74)"
    );

    // And the gap must be the canvas, not rounding noise: the monolithic
    // canvas here is ~30 MB of RGB8 before the downscale buffer.
    assert!(
        mono_rss - stream_rss > 20.0,
        "monolithic must peak far above streaming at {}x{}; got \
         monolithic={mono_rss:.2} MB streaming={stream_rss:.2} MB",
        BIG.0,
        BIG.1,
    );
}

/// The positive control for the test above, and the #157 guard applied to the
/// binary that publishes.
///
/// Its first job is to show that `peak_rss_mb` in this JSON is a live
/// measurement at all: a column that cannot produce two different numbers for a
/// 55x change in canvas area cannot be trusted to have produced two different
/// numbers for two engines either.
///
/// Its second job is #157 on this path: the SMALL size is swept AFTER the large
/// one, so a monotonic process-wide watermark hands the small cell the large
/// cell's peak. RED against that implementation.
#[test]
fn a_sweeps_peak_rss_follows_the_canvas_and_does_not_leak_across_sizes() {
    let big = f(row("monolithic", BIG), "peak_rss_mb");
    let small = f(row("monolithic", SMALL), "peak_rss_mb");

    assert!(
        big > 0.0 && small > 0.0,
        "both cells must report a peak RSS, got big={big:.2} MB small={small:.2} MB"
    );
    assert!(
        big - small > 20.0,
        "peak RSS must follow the canvas: {}x{} against {}x{} is a 55x area \
         change and must move the column by more than rounding; got \
         big={big:.2} MB small={small:.2} MB",
        BIG.0,
        BIG.1,
        SMALL.0,
        SMALL.1,
    );
}

/// The two derived columns must be recomputed from the per-child figure, not
/// left standing on whatever RSS basis they were built with (#74 acceptance).
///
/// RED against a build that isolates the cells but keeps deriving efficiency
/// and cost from the parent's process-wide watermark — which would leave two
/// published columns contaminated while the column they are named after looked
/// fixed.
#[test]
fn a_sweeps_derived_columns_are_recomputed_from_the_published_peak_rss() {
    for r in sweep_rows() {
        let engine = r["engine"].as_str().unwrap_or("?");
        let rss = f(r, "peak_rss_mb");
        let tps = f(r, "tiles_per_second");
        let tiles = r["tiles_produced"].as_u64().unwrap_or(0);
        let secs = f(r, "wall_time_ms") / 1000.0;
        assert!(rss > 0.0, "{engine}: peak_rss_mb must be measured");
        assert!(tiles > 0, "{engine}: a cell must produce tiles");

        let want_efficiency = tps / rss;
        let got_efficiency = f(r, "tiles_per_second_per_mb");
        assert!(
            (got_efficiency - want_efficiency).abs() <= want_efficiency.abs() * 1e-9,
            "{engine}: tiles_per_second_per_mb must be tiles_per_second / peak_rss_mb; \
             got {got_efficiency}, expected {want_efficiency} from tps={tps} rss={rss}"
        );

        let want_cost = (rss * secs) / tiles as f64;
        let got_cost = f(r, "resource_cost");
        assert!(
            (got_cost - want_cost).abs() <= want_cost.abs() * 1e-9,
            "{engine}: resource_cost must be peak_rss_mb x seconds / tiles; \
             got {got_cost}, expected {want_cost} from rss={rss} secs={secs} tiles={tiles}"
        );
    }
}

/// The sweep can only read a child's `ru_maxrss` if the binary answers the
/// hidden `--single` subcommand the parent re-invokes it with.
///
/// RED against the binary as it shipped, which had no `--single` at all: its
/// own argument parser refuses the unknown flag with exit 2, so every cell
/// would come back empty. Cheap enough to run anywhere — no sweep, one cell.
#[test]
fn the_scalability_binary_answers_the_single_cell_subcommand() {
    let out = Command::new(env!("CARGO_BIN_EXE_scalability"))
        .args(["--single", "monolithic", "256", "256", "1", "256", "1000000"])
        .output()
        .expect("run the scalability binary");
    assert!(
        out.status.success(),
        "scalability --single must run one cell and exit 0; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let metrics: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("--single must print one RunMetrics JSON, got {stdout:?}: {e}"));
    assert_eq!(metrics["engine"].as_str(), Some("monolithic"));
    assert!(
        metrics["tiles_produced"].as_u64().unwrap_or(0) > 0,
        "the child cell must have produced tiles"
    );
}

/// Source-level guard, in the style of `tests/pdf_scalability_series.rs`: it
/// fails the moment the publishing binary goes back to reading a rusage
/// watermark of its own, without needing a sweep to run.
///
/// The behavioural tests above are the real guard, and this one is only a
/// tripwire — it reads text, and text cannot tell code from the prose around
/// it. So it matches on the *call path* `libc::getrusage`, which is how the
/// deleted `process_peak_rss` reached the watermark, and not on `RUSAGE_SELF`,
/// which the comment explaining #74 quite reasonably contains. I wrote it the
/// other way round first and it failed on my own doc comment...
#[test]
fn the_scalability_binary_measures_through_the_child_harness() {
    const SCALABILITY: &str = include_str!("../src/scalability.rs");
    assert!(
        SCALABILITY.contains("spawn_single_cell"),
        "scalability must measure each cell through harness::spawn_single_cell \
         so every engine's ru_maxrss is its own child's (issue #74)"
    );
    assert!(
        SCALABILITY.contains("maybe_run_single_subcommand"),
        "scalability must answer the hidden --single subcommand, or the children \
         it spawns cannot run a cell (issue #74)"
    );
    assert!(
        !SCALABILITY.contains("libc::getrusage"),
        "scalability must not read a rusage watermark of its own: RUSAGE_SELF is \
         process-wide and every engine in the sweep shares it, which is exactly \
         how #74 shipped"
    );
}
