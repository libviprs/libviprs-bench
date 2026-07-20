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
        // Pixel-level output-equivalence spot-check, once per size (the source
        // is concurrency-independent). Its min PSNR is stamped onto every
        // libviprs row for the size below; a sub-threshold score is logged
        // loudly inside the call.
        let psnr_check = equivalence_psnr_for_size(w, h, tile_size);
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

            let mut aggregated: Vec<RunMetrics> =
                per_engine.into_iter().filter_map(aggregate).collect();

            // Output-equivalence gate for this configuration: tile GEOMETRY
            // (count + per-level grid) here, plus the pixel-level PSNR score
            // from the per-size spot-check stamped onto each libviprs row.
            check_output_equivalence(w, h, conc, &aggregated);
            if let Some(check) = psnr_check {
                for run in aggregated.iter_mut().filter(|r| r.engine != "libvips") {
                    run.equivalence_psnr_db = Some(check.min_psnr_db);
                }
            }

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

// ---------------------------------------------------------------------------
// Pixel-level output-equivalence spot-check (PSNR / SSIM) — issue #23 / #32
//
// The geometry gate above ([`check_output_equivalence`]) proves each engine
// emits the same *number* of correctly-sized tiles, but not that those tiles
// carry the right *pixels* — a fast engine that wrote garbage tiles of the
// right shape passed. This section adds a cheap pixel-level spot-check: decode
// a few mid-pyramid tiles from the libvips `dzsave` reference and the libviprs
// candidate and assert their PSNR clears a documented near-lossless threshold.
// ---------------------------------------------------------------------------

/// Minimum acceptable PSNR (dB) between a libviprs engine tile and the libvips
/// `dzsave` reference tile in the mid-pyramid spot-check.
///
/// Both engines downsample with the **same** 2x2 box-average and encode
/// **lossless** PNG, so a correctly-tiled pyramid is bit-identical to libvips
/// (PSNR clamps to [`PSNR_CLAMP_DB`]) or differs only by ±1-LSB rounding at a
/// handful of pixels (> 48 dB). 40 dB is the textbook "near-lossless" line: it
/// leaves comfortable headroom for those benign encoder/rounding differences
/// while a corrupted or wrong-content tile — whose pixels are uncorrelated with
/// the reference — scores well under 20 dB (an inverted tile ≈ 4 dB). See the
/// negative test in `tests/output_equivalence.rs`.
pub const MIN_TILE_PSNR_DB: f64 = 40.0;

/// Finite ceiling substituted for the (infinite) PSNR of two identical tiles,
/// so the score stays serializable and averageable.
pub const PSNR_CLAMP_DB: f64 = 100.0;

/// Upper bound on tiles decoded per spot-check, so it stays a cheap *spot*
/// check on a large mid level rather than decoding the whole grid.
const MAX_SPOT_TILES: usize = 16;

/// Peak signal-to-noise ratio (dB) between two equal-length 8-bit sample
/// buffers. Self-contained — no image-quality crate.
///
/// Returns [`PSNR_CLAMP_DB`] for identical buffers (infinite PSNR, clamped) and
/// `0.0` for a length mismatch (a definitive non-equivalence, not a partial
/// compare). Finite results are capped at [`PSNR_CLAMP_DB`].
pub fn psnr(a: &[u8], b: &[u8]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut se = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let d = *x as f64 - *y as f64;
        se += d * d;
    }
    let mse = se / a.len() as f64;
    if mse <= 0.0 {
        return PSNR_CLAMP_DB;
    }
    (10.0 * (255.0f64 * 255.0 / mse).log10()).min(PSNR_CLAMP_DB)
}

/// Global (single-window) structural similarity between two equal-length 8-bit
/// buffers, in `[-1, 1]` (1.0 = identical).
///
/// A cheap companion to [`psnr`]: one pass for the means, one for the
/// (co)variances, with the standard SSIM stabilizers `C1 = (0.01·255)²`,
/// `C2 = (0.03·255)²`. Returns `0.0` on a length mismatch. Surfaced alongside
/// PSNR for context; PSNR remains the gate.
pub fn ssim(a: &[u8], b: &[u8]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let n = a.len() as f64;
    let (mut sa, mut sb) = (0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b.iter()) {
        sa += *x as f64;
        sb += *y as f64;
    }
    let (ma, mb) = (sa / n, sb / n);
    let (mut va, mut vb, mut cov) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b.iter()) {
        let da = *x as f64 - ma;
        let db = *y as f64 - mb;
        va += da * da;
        vb += db * db;
        cov += da * db;
    }
    va /= n;
    vb /= n;
    cov /= n;
    let c1 = (0.01 * 255.0f64).powi(2);
    let c2 = (0.03 * 255.0f64).powi(2);
    ((2.0 * ma * mb + c1) * (2.0 * cov + c2)) / ((ma * ma + mb * mb + c1) * (va + vb + c2))
}

/// Result of the mid-pyramid tile spot-check between a libvips reference
/// pyramid and a libviprs candidate pyramid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TilePsnrCheck {
    /// Number of tiles actually decoded and compared.
    pub tiles_compared: usize,
    /// Minimum PSNR (dB) over the compared tiles — the gated figure.
    pub min_psnr_db: f64,
    /// Mean PSNR (dB) over the compared tiles.
    pub mean_psnr_db: f64,
    /// Minimum global SSIM over the compared tiles (informational).
    pub min_ssim: f64,
    /// Tile grid (cols, rows) of the compared mid level.
    pub level_cols: u32,
    pub level_rows: u32,
    /// Level directory index compared on each side. These differ when libvips
    /// carries its extra 1x1 apex level, which is why the check aligns levels
    /// by resolution rather than by raw index.
    pub reference_level: u32,
    pub candidate_level: u32,
}

impl TilePsnrCheck {
    /// Whether every compared tile cleared [`MIN_TILE_PSNR_DB`].
    pub fn passes(&self) -> bool {
        self.min_psnr_db >= MIN_TILE_PSNR_DB
    }
}

/// One pyramid level on disk: its directory index and `{col}_{row}.png` grid.
struct LevelGrid {
    index: u32,
    cols: u32,
    rows: u32,
}

/// Read the level directories under a DeepZoom `_files`-style tiles root,
/// returning each level's index and tile grid (derived from the
/// `{col}_{row}.png` names), sorted by index ascending.
fn read_level_grids(files_dir: &Path) -> Vec<LevelGrid> {
    let mut levels: Vec<LevelGrid> = Vec::new();
    let Ok(entries) = std::fs::read_dir(files_dir) else {
        return levels;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(index) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let (mut cols, mut rows) = (0u32, 0u32);
        if let Ok(tiles) = std::fs::read_dir(&path) {
            for tile in tiles.flatten() {
                let tp = tile.path();
                if tp.extension().and_then(|e| e.to_str()) != Some("png") {
                    continue;
                }
                if let Some((c, r)) = tp
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.split_once('_'))
                {
                    if let (Ok(c), Ok(r)) = (c.parse::<u32>(), r.parse::<u32>()) {
                        cols = cols.max(c + 1);
                        rows = rows.max(r + 1);
                    }
                }
            }
        }
        levels.push(LevelGrid { index, cols, rows });
    }
    levels.sort_by_key(|l| l.index);
    levels
}

/// Decode a PNG tile to an RGB8 byte buffer, or `None` if it cannot be read.
fn decode_rgb8(path: &Path) -> Option<Vec<u8>> {
    Some(image::open(path).ok()?.to_rgb8().into_raw())
}

/// Spot-check mid-pyramid tile fidelity between a libvips `dzsave` reference
/// pyramid (`reference_files`, a DeepZoom `_files` dir) and a libviprs
/// candidate pyramid (`candidate_files`).
///
/// Picks a *downsampled, multi-tile* mid level — the median such level, so the
/// check exercises the resampling pipeline (not just the full-resolution
/// pass-through) over a handful of real tiles — aligns it across the two
/// pyramids by counting down from full resolution (which absorbs the libvips
/// extra 1x1 apex level), and compares up to [`MAX_SPOT_TILES`] co-present
/// tiles by [`psnr`] / [`ssim`].
///
/// Returns `None` when there is no comparable multi-tile mid level (e.g. an
/// image too small to have one) or no tile is present on both sides.
pub fn spot_check_tile_psnr(
    reference_files: &Path,
    candidate_files: &Path,
) -> Option<TilePsnrCheck> {
    let refs = read_level_grids(reference_files);
    let cands = read_level_grids(candidate_files);
    if refs.is_empty() || cands.is_empty() {
        return None;
    }

    // Align by counting down from full resolution (the highest index on each
    // side): shared level `k` pairs `refs[len-1-k]` with `cands[len-1-k]`.
    // `k == 0` is full resolution; the libvips apex surplus sits at the small
    // end and is simply left unpaired.
    let shared = refs.len().min(cands.len());
    let mut mids: Vec<(&LevelGrid, &LevelGrid)> = Vec::new();
    for k in 1..shared {
        let r = &refs[refs.len() - 1 - k];
        let c = &cands[cands.len() - 1 - k];
        // Downsampled (k>=1), multi-tile, and matching grids on both sides.
        if r.cols == c.cols && r.rows == c.rows && (c.cols as u64 * c.rows as u64) > 1 {
            mids.push((r, c));
        }
    }
    if mids.is_empty() {
        return None;
    }
    // The median downsampled multi-tile level is our "mid-pyramid" level.
    let (r_level, c_level) = mids[mids.len() / 2];

    // Enumerate tile positions, capping the total decoded at MAX_SPOT_TILES by
    // an even stride so the sample is deterministic and spread across the grid
    // (position 0,0 is always taken).
    let (cols, rows) = (c_level.cols, c_level.rows);
    let total = cols as usize * rows as usize;
    let stride = total.div_ceil(MAX_SPOT_TILES).max(1);

    let mut compared = 0usize;
    let mut min_psnr = f64::INFINITY;
    let mut sum_psnr = 0.0f64;
    let mut min_ssim = f64::INFINITY;
    let mut idx = 0usize;
    for row in 0..rows {
        for col in 0..cols {
            let take = idx % stride == 0;
            idx += 1;
            if !take {
                continue;
            }
            let rp = reference_files.join(format!("{}/{col}_{row}.png", r_level.index));
            let cp = candidate_files.join(format!("{}/{col}_{row}.png", c_level.index));
            // Only compare tiles present on BOTH sides: a blank tile that one
            // engine legitimately skips is not a corruption.
            let (Some(ra), Some(ca)) = (decode_rgb8(&rp), decode_rgb8(&cp)) else {
                continue;
            };
            let p = psnr(&ra, &ca);
            min_psnr = min_psnr.min(p);
            sum_psnr += p;
            min_ssim = min_ssim.min(ssim(&ra, &ca));
            compared += 1;
        }
    }
    if compared == 0 {
        return None;
    }
    Some(TilePsnrCheck {
        tiles_compared: compared,
        min_psnr_db: min_psnr,
        mean_psnr_db: sum_psnr / compared as f64,
        min_ssim,
        level_cols: cols,
        level_rows: rows,
        reference_level: r_level.index,
        candidate_level: c_level.index,
    })
}

/// Pixel-level output-equivalence spot-check for one image size: regenerate a
/// libviprs (monolithic) pyramid and a libvips `dzsave` pyramid from the same
/// gradient source, [`spot_check_tile_psnr`] their mid-pyramid tiles, and log
/// the result loudly. The source is concurrency-independent, so this runs once
/// per size (outside the timed loop). Returns `None` — logging only — when
/// libvips is unavailable or there is no comparable mid level.
fn equivalence_psnr_for_size(w: u32, h: u32, tile_size: u32) -> Option<TilePsnrCheck> {
    if !crate::vips_available() {
        return None;
    }
    let src = crate::gradient_raster(w, h);
    let plan = libviprs::PyramidPlanner::new(w, h, tile_size, 0, libviprs::Layout::DeepZoom)
        .ok()?
        .plan();

    let root = std::env::temp_dir()
        .join("libviprs-bench")
        .join(format!("equiv_{w}x{h}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let candidate = crate::write_libviprs_pyramid(&src, &plan, &root.join("lv"));
    let png = crate::write_temp_png(&src);
    let check = crate::write_libvips_pyramid(&png, &root.join("vips"), tile_size)
        .and_then(|reference| spot_check_tile_psnr(&reference, &candidate));
    let _ = std::fs::remove_file(&png);
    let _ = std::fs::remove_dir_all(&root);

    match &check {
        Some(c) if c.passes() => eprintln!(
            "note: {w}x{h}: output-equivalence PSNR spot-check OK \
             ({:.1} dB min / {:.1} dB mean over {} mid tiles, SSIM {:.4}).",
            c.min_psnr_db, c.mean_psnr_db, c.tiles_compared, c.min_ssim
        ),
        Some(c) => eprintln!(
            "WARNING: {w}x{h}: output-equivalence PSNR spot-check FAILED \
             ({:.1} dB min < {MIN_TILE_PSNR_DB:.0} dB threshold over {} mid \
             tiles). A libviprs engine is producing visually-wrong tiles; its \
             timings are NOT comparing equal work.",
            c.min_psnr_db, c.tiles_compared
        ),
        None => {}
    }
    check
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
            equivalence_psnr_db: None,
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
