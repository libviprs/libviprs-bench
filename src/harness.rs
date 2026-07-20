//! Per-cell child-process isolation + statistics for the fair benchmark.
//!
//! # Why a child per cell
//!
//! Peak RSS was measured with `getrusage(RUSAGE_SELF).ru_maxrss` — a
//! *monotonic, process-wide* high-water mark. Every in-process engine, and
//! the in-process libvips FFI, share that one watermark, so a small-image
//! run right after a large-image run inherited the large run's peak and
//! every memory number was contaminated (issue #157).
//!
//! The fix runs exactly one `(engine, size, concurrency)` cell per process.
//! [`run_single_cell`] is the child body (dispatched by the hidden
//! `--single` subcommand); it prints one [`RunMetrics`] as JSON on stdout.
//! [`spawn_single_cell`] is the parent: it spawns that child, reads the
//! JSON, and reaps the child with `wait4`, taking the child's `ru_maxrss`
//! as the authoritative per-run RSS. Because the watermark is scoped to a
//! fresh process, it is a true per-run peak on one basis for *every* engine
//! — libviprs and the libvips FFI alike.
//!
//! # Statistics
//!
//! [`measure_cell`] runs each cell `>= 7` times after a discarded warm-up
//! and summarizes the samples into [`RunStats`] (median / min / IQR / CI95,
//! for both wall time and RSS). [`run_isolated_suite`] interleaves engine
//! order within a size so slow drift affects all engines equally, and gates
//! each configuration on cross-engine output equivalence before trusting
//! the timings.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{RunMetrics, RunStats};

/// Default timed iterations per cell (after the warm-up).
pub const DEFAULT_ITERS: u32 = 7;
/// Default discarded warm-up iterations per cell.
pub const DEFAULT_WARMUP: u32 = 1;

/// One benchmark cell: a single engine at one size and concurrency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSpec {
    pub engine: Engine,
    pub width: u32,
    pub height: u32,
    pub concurrency: usize,
    pub tile_size: u32,
    pub budget_bytes: u64,
}

/// The engines the suite compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Monolithic,
    Streaming,
    MapReduce,
    Libvips,
}

impl Engine {
    pub fn as_str(self) -> &'static str {
        match self {
            Engine::Monolithic => "monolithic",
            Engine::Streaming => "streaming",
            Engine::MapReduce => "mapreduce",
            Engine::Libvips => "libvips",
        }
    }

    pub fn parse(s: &str) -> Option<Engine> {
        match s {
            "monolithic" => Some(Engine::Monolithic),
            "streaming" => Some(Engine::Streaming),
            "mapreduce" => Some(Engine::MapReduce),
            "libvips" => Some(Engine::Libvips),
            _ => None,
        }
    }
}

/// Run exactly one cell in *this* process and return its metrics.
///
/// This is the child body behind `--single`. Each engine is exercised once
/// (single shot); the parent handles warm-up, repetition, and aggregation.
pub fn run_single_cell(spec: CellSpec) -> Option<RunMetrics> {
    use libviprs::{Layout, PyramidPlanner};

    let src = crate::gradient_raster(spec.width, spec.height);
    let label = format!(
        "{}x{}_c{}_{}",
        spec.width,
        spec.height,
        spec.concurrency,
        spec.engine.as_str()
    );

    match spec.engine {
        Engine::Libvips => {
            #[cfg(feature = "libvips")]
            {
                if let Some(m) =
                    crate::bench_libvips_inprocess(&src, spec.tile_size, spec.concurrency, &label)
                {
                    return Some(m);
                }
            }
            // CLI fallback.
            let png = crate::write_temp_png(&src);
            let m = crate::bench_libvips(
                &png,
                spec.width,
                spec.height,
                spec.tile_size,
                spec.concurrency,
                &label,
            );
            let _ = std::fs::remove_file(&png);
            m
        }
        other => {
            let planner =
                PyramidPlanner::new(spec.width, spec.height, spec.tile_size, 0, Layout::DeepZoom)
                    .ok()?;
            let plan = planner.plan();
            Some(match other {
                Engine::Monolithic => {
                    crate::bench_monolithic(&src, &plan, spec.concurrency, &label)
                }
                Engine::Streaming => {
                    crate::bench_streaming(&src, &plan, spec.concurrency, spec.budget_bytes, &label)
                }
                Engine::MapReduce => {
                    crate::bench_mapreduce(&src, &plan, spec.concurrency, spec.budget_bytes, &label)
                }
                Engine::Libvips => unreachable!(),
            })
        }
    }
}

/// If `argv` begins with the hidden `--print-core` subcommand, print the
/// core crate identity this harness was built against — one line,
/// `version\tshort_sha` from the `build.rs` stamp ([`crate::core_version`] /
/// [`crate::core_git_sha`]) — and return `Some(0)`; otherwise `None`.
///
/// This is the artifact's own answer to "which core did you link?", used by the
/// version-matrix runner to verify a per-tag rebuild actually measured the ref
/// it is about to be recorded under, rather than trusting a side-channel read
/// (issue #19). Keep it ahead of the normal `main` body, like `--single`.
pub fn maybe_run_print_core_subcommand() -> Option<i32> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("--print-core") {
        return None;
    }
    println!("{}\t{}", crate::core_version(), crate::core_git_sha());
    Some(0)
}

/// If `argv` begins with the hidden `--single` subcommand, run that one
/// cell, print its [`RunMetrics`] JSON on stdout, and return `Some(exit)`.
/// The caller (a `main`) should `std::process::exit` with the code.
/// Returns `None` when this is not a `--single` invocation, so normal
/// `main` continues.
///
/// Usage: `--single <engine> <w> <h> <conc> [tile_size] [budget_bytes]`.
pub fn maybe_run_single_subcommand() -> Option<i32> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("--single") {
        return None;
    }
    let parse = || -> Option<CellSpec> {
        Some(CellSpec {
            engine: Engine::parse(args.get(2)?)?,
            width: args.get(3)?.parse().ok()?,
            height: args.get(4)?.parse().ok()?,
            concurrency: args.get(5)?.parse().ok()?,
            tile_size: args.get(6).and_then(|s| s.parse().ok()).unwrap_or(256),
            budget_bytes: args
                .get(7)
                .and_then(|s| s.parse().ok())
                .unwrap_or(1_000_000),
        })
    };
    let Some(spec) = parse() else {
        eprintln!("usage: --single <engine> <w> <h> <conc> [tile_size] [budget_bytes]");
        return Some(2);
    };
    match run_single_cell(spec) {
        Some(metrics) => {
            println!("{}", serde_json::to_string(&metrics).unwrap());
            Some(0)
        }
        None => {
            eprintln!("cell produced no metrics (engine unavailable?)");
            Some(1)
        }
    }
}

/// Spawn one `--single` child for `spec`, read its metrics, and reap it
/// with `wait4` to capture the child's true per-run peak RSS.
///
/// `exe` is the harness binary to re-invoke (typically
/// [`std::env::current_exe`]). Returns `None` if the child failed or
/// emitted no parseable metrics.
pub fn spawn_single_cell(exe: &Path, spec: CellSpec) -> Option<RunMetrics> {
    let mut child = Command::new(exe)
        .arg("--single")
        .arg(spec.engine.as_str())
        .arg(spec.width.to_string())
        .arg(spec.height.to_string())
        .arg(spec.concurrency.to_string())
        .arg(spec.tile_size.to_string())
        .arg(spec.budget_bytes.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .ok()?;

    let pid = child.id() as i32;

    // Drain stdout to EOF (the child closes it on exit).
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;

    // Reap the child ourselves with wait4 so we get its rusage. We must
    // NOT also call `child.wait()` (double reap); `std::process::Child`'s
    // Drop neither waits nor kills, so letting `child` drop is safe.
    let child_rss = wait4_maxrss(pid);

    let mut metrics: RunMetrics = serde_json::from_str(out.trim()).ok()?;
    // The parent-observed child RSS is authoritative — it is scoped to the
    // child's fresh address space, so it is a true per-run peak. Prefer it
    // over the child's own self-report; fall back to the self-report if
    // wait4 gave nothing.
    if let Some(rss) = child_rss {
        if rss > 0 {
            metrics.peak_rss_bytes = rss;
        }
    }
    Some(metrics)
}

/// `wait4` the given pid and return its peak RSS in bytes, normalizing the
/// platform units of `ru_maxrss` (bytes on macOS/BSD, kilobytes on Linux).
fn wait4_maxrss(pid: i32) -> Option<u64> {
    use std::mem::MaybeUninit;
    let mut status: libc::c_int = 0;
    let mut ru = MaybeUninit::<libc::rusage>::uninit();
    let ret = unsafe { libc::wait4(pid, &mut status, 0, ru.as_mut_ptr()) };
    if ret <= 0 {
        return None;
    }
    let ru = unsafe { ru.assume_init() };
    let maxrss = ru.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        Some(maxrss) // bytes
    } else {
        Some(maxrss * 1024) // kilobytes → bytes
    }
}

/// Measure one cell `iters` times (after `warmup` discarded runs), each in
/// its own child, and fold the samples into an aggregated [`RunMetrics`]
/// whose top-line wall time / RSS are the medians and whose
/// [`RunMetrics::stats`] carries the full spread.
///
/// Returns `None` if no timed iteration produced metrics.
pub fn measure_cell(exe: &Path, spec: CellSpec, iters: u32, warmup: u32) -> Option<RunMetrics> {
    for _ in 0..warmup {
        let _ = spawn_single_cell(exe, spec);
    }
    let mut samples: Vec<RunMetrics> = Vec::with_capacity(iters as usize);
    for _ in 0..iters.max(1) {
        if let Some(m) = spawn_single_cell(exe, spec) {
            samples.push(m);
        }
    }
    aggregate(samples)
}

/// Fold repeated single-shot samples of one cell into a median-representative
/// [`RunMetrics`] carrying [`RunStats`]. Public for the interleaved driver
/// and for tests.
pub fn aggregate(mut samples: Vec<RunMetrics>) -> Option<RunMetrics> {
    if samples.is_empty() {
        return None;
    }
    let paired: Vec<(f64, f64)> = samples
        .iter()
        .map(|m| (m.wall_time_ms(), m.peak_rss_mb()))
        .collect();
    let stats = RunStats::from_samples(&paired);

    // Representative sample = the one whose wall time is nearest the median.
    let target = stats.wall_ms_median;
    samples.sort_by(|a, b| {
        (a.wall_time_ms() - target)
            .abs()
            .partial_cmp(&(b.wall_time_ms() - target).abs())
            .unwrap()
    });
    let mut rep = samples.remove(0);
    rep.stats = Some(stats);
    Some(rep)
}

/// Run the full isolated comparison suite: every engine at every size and
/// concurrency, each cell child-isolated and measured over multiple
/// iterations, with engine order interleaved within a size and a
/// cross-engine output-equivalence gate per configuration.
///
/// `engines` is the ordered engine set (callers drop `Libvips` when vips is
/// unavailable). Returns metrics grouped naturally (size-major).
#[allow(clippy::too_many_arguments)]
pub fn run_isolated_suite(
    exe: &Path,
    sizes: &[(u32, u32)],
    concurrency_levels: &[usize],
    engines: &[Engine],
    tile_size: u32,
    budget_bytes: u64,
    iters: u32,
    warmup: u32,
) -> Vec<RunMetrics> {
    let mut results = Vec::new();

    for &(w, h) in sizes {
        for &conc in concurrency_levels {
            // Collect `iters` interleaved samples per engine: outer loop is
            // the iteration, inner loop the engine, so any slow machine
            // drift over the size's wall-clock window hits every engine
            // roughly equally rather than penalizing whichever ran last.
            let specs: Vec<CellSpec> = engines
                .iter()
                .map(|&engine| CellSpec {
                    engine,
                    width: w,
                    height: h,
                    concurrency: conc,
                    tile_size,
                    budget_bytes,
                })
                .collect();

            let mut per_engine: Vec<Vec<RunMetrics>> = specs.iter().map(|_| Vec::new()).collect();

            for _ in 0..warmup {
                for spec in &specs {
                    let _ = spawn_single_cell(exe, *spec);
                }
            }
            for _ in 0..iters.max(1) {
                for (i, spec) in specs.iter().enumerate() {
                    if let Some(m) = spawn_single_cell(exe, *spec) {
                        per_engine[i].push(m);
                    }
                }
            }

            let aggregated: Vec<RunMetrics> =
                per_engine.into_iter().filter_map(aggregate).collect();

            // Output-equivalence gate for this configuration.
            check_output_equivalence(w, h, conc, &aggregated);

            results.extend(aggregated);
        }
    }

    results
}

/// Assert that every engine produced the same pyramid for one
/// configuration: equal level count and equal per-level PNG tile grid.
///
/// libvips `dzsave` emits one extra 1×1 "apex" level above the libviprs
/// engines' top level; that single known delta is annotated rather than
/// treated as a failure. Any *other* mismatch is a loud warning — the
/// timings for that configuration are not comparing equal work.
pub fn check_output_equivalence(w: u32, h: u32, conc: usize, runs: &[RunMetrics]) {
    let with_grid: Vec<&RunMetrics> = runs
        .iter()
        .filter(|r| !r.per_level_tiles.is_empty())
        .collect();
    let Some(reference) = with_grid
        .iter()
        .find(|r| r.engine != "libvips")
        .or_else(|| with_grid.first())
    else {
        return;
    };
    let ref_grid = &reference.per_level_tiles;

    for run in &with_grid {
        if &run.per_level_tiles == ref_grid {
            continue;
        }
        // Known libvips apex delta: identical grids except libvips has one
        // extra leading 1-tile apex level.
        if run.engine == "libvips" && is_apex_extended(ref_grid, &run.per_level_tiles) {
            eprintln!(
                "note: {w}x{h} c{conc}: libvips has the known extra 1x1 apex level \
                 (grid {:?} vs reference {:?}); annotated, not a failure.",
                run.per_level_tiles, ref_grid
            );
            continue;
        }
        eprintln!(
            "WARNING: {w}x{h} c{conc}: output-equivalence gate FAILED for engine \
             '{}': per-level PNG grid {:?} != reference '{}' {:?}. \
             Timings for this configuration are NOT comparing equal work.",
            run.engine, run.per_level_tiles, reference.engine, ref_grid
        );
    }
}

/// True if `extended` equals `reference` with exactly one extra leading
/// apex level whose tile count is 1 (the libvips dzsave convention).
fn is_apex_extended(reference: &[u64], extended: &[u64]) -> bool {
    extended.len() == reference.len() + 1
        && extended.first() == Some(&1)
        && &extended[1..] == reference
}

/// The harness binary to re-invoke for child cells. Falls back to the
/// literal "self" only if the current exe cannot be resolved.
pub fn current_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("self"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RunStats;

    #[test]
    fn apex_extension_recognized() {
        // libvips = libviprs grid with one extra leading 1-tile apex level.
        assert!(is_apex_extended(&[4, 2, 1], &[1, 4, 2, 1]));
        // Identical grids are not "extended".
        assert!(!is_apex_extended(&[4, 2, 1], &[4, 2, 1]));
        // A genuinely different grid is not an apex extension.
        assert!(!is_apex_extended(&[4, 2, 1], &[9, 4, 2, 1]));
        assert!(!is_apex_extended(&[4, 2, 1], &[1, 4, 3, 1]));
    }

    #[test]
    fn engine_roundtrip() {
        for e in [
            Engine::Monolithic,
            Engine::Streaming,
            Engine::MapReduce,
            Engine::Libvips,
        ] {
            assert_eq!(Engine::parse(e.as_str()), Some(e));
        }
        assert_eq!(Engine::parse("nope"), None);
    }

    #[test]
    fn aggregate_picks_median_representative_and_attaches_stats() {
        // Three fabricated samples with distinct wall times; the aggregate
        // should carry stats and a wall time equal to the median sample.
        let mk = |wall_ns: u64| RunMetrics {
            label: "x".into(),
            width: 8,
            height: 8,
            engine: "monolithic".into(),
            measurement_path: String::new(),
            wall_time: std::time::Duration::from_nanos(wall_ns),
            tracked_memory_bytes: 0,
            peak_rss_bytes: 1024 * 1024,
            stats: None,
            per_level_tiles: vec![1],
            tiles_produced: 1,
            levels_processed: 1,
            tiles_skipped: 0,
            strips: 0,
            batches: 0,
            inflight_strips: 0,
            concurrency: 0,
            memory_budget_bytes: 0,
        };
        let samples = vec![mk(1_000_000), mk(3_000_000), mk(2_000_000)];
        let agg = aggregate(samples).unwrap();
        let stats: &RunStats = agg.stats.as_ref().unwrap();
        assert_eq!(stats.n, 3);
        assert!((agg.wall_time_ms() - stats.wall_ms_median).abs() < 1e-6);
    }
}
