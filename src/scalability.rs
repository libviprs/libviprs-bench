//! Engine scalability benchmark.
//!
//! Generates a SYNTHETIC gradient raster (see `gradient_raster`) at
//! progressively larger sizes — the actual `43551_California_South.pdf`
//! fixture is not committed, so the workload is a stand-in sized to that
//! page's 1.42:1 aspect, NOT a rasterized blueprint. Runs the family's engines
//! (monolithic, streaming, MapReduce, plus libvips for the `vips` family) at
//! each size and at matched thread budgets (1 and num_cpus), measuring how wall
//! time, peak RSS, and efficiency scale with image area.
//!
//! Every cell runs in its OWN child process (`harness::spawn_single_cell`), so
//! the peak RSS each row reports is that child's `ru_maxrss` and nobody else's.
//! It did not always: the sweep read `getrusage(RUSAGE_SELF)` with all three
//! engines in one process, and since that watermark is process-wide and never
//! comes back down, monolithic set it and every engine after it published
//! monolithic's number (issue #74).
//!
//! Run: cargo run --release --bin scalability [-- --family <name>]
//!
//! Output: report/<family>/scalability_results.json. This binary emits JSON only; the
//! `scalability_*.svg` line charts render from that JSON via
//! `tools/charts/render.mjs` (run-bench.sh invokes it after this writes the
//! JSON) — the plotters dependency is gone (issue #42).

// The chart plumbing threads fixed-arity metric tuples and borrowed series
// slices through a few local closures; naming each as a `type` would add
// noise without aiding readers, so the complexity lint is allowed here.
#![allow(clippy::type_complexity)]

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use libviprs_bench::family::{ALL_FAMILIES, DEFAULT_FAMILY, Family};
use libviprs_bench::harness::{self, CellSpec, Engine};
use libviprs_bench::provenance::Provenance;
use libviprs_bench::{
    RunMetrics, format_thousands, parse_concurrency, parse_sizes, streaming_budget_for,
    vips_available,
};

/// Measure one `(engine, size, concurrency)` cell in its OWN child process and
/// take that child's `ru_maxrss` as its peak RSS.
///
/// This is the whole of issue #74. The sweep used to run every engine in one
/// process and read `getrusage(RUSAGE_SELF).ru_maxrss`, a process-wide
/// watermark that never comes back down: monolithic holds the entire canvas, it
/// sets the watermark, and every engine measured afterwards reported
/// monolithic's number as its own. The first full `engines` capture published
/// byte-identical peak RSS for all three engines in twenty of twenty
/// (megapixel, concurrency) groups, to seven decimal places, and
/// `tiles_per_second_per_mb` and `resource_cost` are derived from it, so two
/// more published columns carried the same one number.
///
/// [`harness::spawn_single_cell`] is the fix that already existed — the `report`
/// binary has been measuring through it since #157 — and it is reached here
/// exactly as `report` reaches it: re-invoke this binary with the hidden
/// `--single` subcommand, read the child's metrics JSON, and reap it with
/// `wait4`. Reordering the engines so monolithic runs last would make the
/// numbers look plausible without making them measurements, and the next person
/// to add an engine would put it back.
///
/// `None` is a skipped cell (an engine fault, or libvips not present on a
/// `vips` build); the child logs the reason and the sweep drops that one point
/// rather than aborting (issue #46).
fn measure_cell(exe: &Path, engine: Engine, w: u32, h: u32, concurrency: usize) -> Option<RunMetrics> {
    harness::spawn_single_cell(
        exe,
        CellSpec {
            engine,
            width: w,
            height: h,
            concurrency,
            tile_size: TILE_SIZE,
            // A FLOOR, not the effective budget: `bench_streaming` /
            // `bench_mapreduce` size it up per canvas through
            // `streaming_budget_for`, which is the same value this binary used
            // to compute for itself before handing it to the engine.
            budget_bytes: STREAMING_BUDGET_FLOOR,
        },
    )
}

const TILE_SIZE: u32 = 256;
/// Floor on the streaming engine's memory budget, passed as the `floor` to the
/// shared [`streaming_budget_for`]. Keeps small images in the "true streaming"
/// regime; wider canvases are auto-scaled up so at least one tile-aligned strip
/// fits and the strict `BudgetPolicy::Error` never trips. This binary's non-
/// centred DeepZoom plans have `canvas_width == width`, so passing the image
/// width here is identical to sizing from `plan.canvas_width` (RGB8, bpp = 3).
const STREAMING_BUDGET_FLOOR: u64 = 4_000_000; // 4 MB

/// The swept image sizes. Gradient rasters at progressively larger sizes, at
/// the 1.42:1 aspect ratio of 43551_California_South.pdf (4608x3240 pts). The
/// grid intentionally spans the sub-megapixel "noise" regime (where fixed setup
/// costs dominate) through ~280 MP, so the log-log charts (rendered by
/// tools/charts/render.mjs; `--linear` selects linear axes) show a full trend
/// rather than a cluster of dots. Memory: monolithic peak is about
/// `w x h x 3 x 1.25` bytes, capped here at ~1.7 GB so the default 4 GB Docker
/// container still has headroom for libvips alongside.
///
/// `--sizes` overrides it. That override is not a measurement knob: it exists so
/// a test can drive this binary over one or two cells in seconds, which nothing
/// could do before #74.
const SWEEP_SIZES: &[(u32, u32)] = &[
    (512, 360),
    (1024, 720),
    (2048, 1440),
    (4096, 2880),
    (4608, 3240),   // full California South page at 72 DPI (14.93 MP)
    (8192, 5760),   // beyond the PDF — pure scaling (47.18 MP)
    (10000, 7000),  // 70 MP
    (12000, 8400),  // 100.8 MP
    (16384, 11520), // 188.7 MP
    (20000, 14000), // 280 MP — mono peak is about 1.05 GB
];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScalabilityPoint {
    width: u32,
    height: u32,
    megapixels: f64,
    engine: String,
    /// Thread budget this point was measured at (`VIPS_CONCURRENCY` for
    /// libvips; engine concurrency for the libviprs engines). Points at
    /// different thread caps are NEVER mixed on one chart line set (issue
    /// #156). Defaults to 0 for pre-#156 history that predates the field.
    #[serde(default)]
    concurrency: usize,
    wall_time_ms: f64,
    /// Engine-tracked working set (libviprs engines; 0 for libvips). Kept in a
    /// separate field from `peak_rss_mb` so the two memory bases are never
    /// conflated (issue #153). Defaults to 0 for pre-#153 history.
    #[serde(default)]
    tracked_memory_mb: f64,
    /// Peak RSS of the child process this one cell ran in — the
    /// cross-engine-comparable memory basis, and a true per-run peak rather
    /// than a watermark shared with whatever ran before it (issue #74).
    /// The `peak_memory_mb` alias lets pre-#153 scalability history (which
    /// used that field name) deserialize unchanged.
    #[serde(alias = "peak_memory_mb")]
    peak_rss_mb: f64,
    tiles_produced: u64,
    tiles_per_second: f64,
    /// Tiles/s per RSS-MB (common basis).
    tiles_per_second_per_mb: f64,
    /// RSS-MB-seconds per tile (common basis).
    resource_cost: f64,
}

#[allow(clippy::too_many_arguments)]
fn to_point(
    w: u32,
    h: u32,
    engine: &str,
    concurrency: usize,
    dur: std::time::Duration,
    tracked_bytes: u64,
    rss_bytes: u64,
    tiles: u64,
) -> ScalabilityPoint {
    let mp = w as f64 * h as f64 / 1_000_000.0;
    let secs = dur.as_secs_f64();
    let ms = secs * 1000.0;
    let tracked_mb = tracked_bytes as f64 / (1024.0 * 1024.0);
    let rss_mb = rss_bytes as f64 / (1024.0 * 1024.0);
    let tps = if secs > 0.0 { tiles as f64 / secs } else { 0.0 };
    // Efficiency and resource-cost use the common RSS basis so every engine's
    // number means the same thing (issue #153).
    let tps_mb = if rss_mb > 0.0 { tps / rss_mb } else { 0.0 };
    let cost = if tiles > 0 {
        (rss_mb * secs) / tiles as f64
    } else {
        0.0
    };

    ScalabilityPoint {
        width: w,
        height: h,
        megapixels: mp,
        engine: engine.to_string(),
        concurrency,
        wall_time_ms: ms,
        tracked_memory_mb: tracked_mb,
        peak_rss_mb: rss_mb,
        tiles_produced: tiles,
        tiles_per_second: tps,
        tiles_per_second_per_mb: tps_mb,
        resource_cost: cost,
    }
}

/// Turn one child's [`RunMetrics`] into a chart point.
///
/// The dimensions come from the metrics rather than from the requested size, so
/// a row is always plotted at what was actually measured — the same rule the
/// `streaming-pdf` series already follows.
fn point_from_metrics(engine: &str, concurrency: usize, m: &RunMetrics) -> ScalabilityPoint {
    to_point(
        m.width,
        m.height,
        engine,
        concurrency,
        m.wall_time,
        m.tracked_memory_bytes,
        m.peak_rss_bytes,
        m.tiles_produced,
    )
}

/// Path to the committed real-content PDF fixture (issue #30), resolved against
/// the crate manifest so it works regardless of the working directory.
#[cfg(feature = "pdfium")]
const PDF_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/cc_licenses_mapping.pdf"
);

/// Cap the real-content PDF series at this many megapixels. The committed
/// fixture is a ~1 MP vector page; driving it far past this is pure upsampling
/// of fixed content at ever-higher DPI (e.g. a 65 KB page rendered at >1000
/// DPI) — slow, and it adds no real-content signal the smaller points don't
/// already show. The four gradient series still run the FULL sweep; only the
/// PDF companion is capped. Overridable with `--pdf-max-mp <n>` (issue #22
/// review).
#[cfg(feature = "pdfium")]
const DEFAULT_PDF_MAX_MP: f64 = 50.0;

/// The committed fixture's page width in pixels at 72 DPI, read once from the
/// source itself so the DPI-for-width mapping tracks the *actual* fixture
/// rather than a hardcoded constant (issue #22 review). `None` when the source
/// cannot be opened (e.g. libpdfium unavailable), in which case the whole
/// real-content series is skipped up front with a single message instead of
/// per-cell.
#[cfg(feature = "pdfium")]
fn pdf_base_width() -> Option<u32> {
    use libviprs::StripSource;
    match libviprs::PdfiumStripSource::new_streaming(PDF_FIXTURE, 1, 72) {
        Ok(src) => Some(src.width()),
        Err(e) => {
            eprintln!(
                "Real-content PDF series disabled: could not open {PDF_FIXTURE} at 72 DPI ({e}). \
                 Set PDFIUM_PATH if libpdfium is not on the system library path."
            );
            None
        }
    }
}

/// Render DPI that scales the committed PDF fixture to approximately
/// `target_width` pixels wide, given the fixture's own 72-DPI width in pixels
/// (`base_width`, derived once at startup by [`pdf_base_width`]).
///
/// Scaling linearly from the fixture's *actual* 72-DPI width — instead of a
/// hardcoded page size — lands the rasterized-PDF series at the same image
/// sizes as the synthetic-gradient sweep, keeping the two workloads comparable
/// on the shared megapixel x-axis (issue #31) and staying correct even if the
/// committed fixture is later replaced with a differently-sized page (the
/// scenario `fixtures/PROVENANCE.md` explicitly permits; issue #22 review).
#[cfg(feature = "pdfium")]
fn pdf_dpi_for_width(target_width: u32, base_width: u32) -> u32 {
    let base = (base_width as f64).max(1.0);
    let dpi = 72.0 * target_width as f64 / base;
    (dpi.round() as u32).max(1)
}

/// Run the rasterized-PDF streaming workload for one sweep size and return its
/// scalability point (engine series `"streaming-pdf"`), or `None` if the pdfium
/// source could not be rendered (e.g. libpdfium unavailable) — the benchmark
/// then simply omits the real-content series for that point rather than
/// aborting the whole run.
///
/// The point is plotted at the PDF's *actual* rendered dimensions (from the
/// returned metrics), so its megapixel x-position is honest even when pdfium
/// rounds the page a pixel differently from the gradient target.
#[cfg(feature = "pdfium")]
fn run_pdf_streaming(
    target_width: u32,
    base_width: u32,
    concurrency: usize,
    tile_size: u32,
) -> Option<ScalabilityPoint> {
    let dpi = pdf_dpi_for_width(target_width, base_width);
    let label = format!("pdf_{target_width}_c{concurrency}");
    // Pass the streaming-regime FLOOR and let `bench_streaming_pdf` own the
    // RGBA-correct budget: it raises the budget to fit the worst-case 4-bpp
    // strip (wider than the 3-bpp gradient at the same width), so any
    // width-derived value handed in here would just be dominated. Passing the
    // floor makes that ownership explicit rather than dead input (issue #22
    // review).
    match libviprs_bench::bench_streaming_pdf(
        std::path::Path::new(PDF_FIXTURE),
        1,
        dpi,
        tile_size,
        concurrency,
        STREAMING_BUDGET_FLOOR,
        &label,
    ) {
        Ok(m) => Some(to_point(
            m.width,
            m.height,
            "streaming-pdf",
            concurrency,
            m.wall_time,
            m.tracked_memory_bytes,
            m.peak_rss_bytes,
            m.tiles_produced,
        )),
        // libpdfium unavailable (or the fixture unreadable) — the ONE
        // legitimately-skippable case: omit the real-content point and carry on.
        Err(e @ libviprs_bench::PdfBenchError::SourceUnavailable(_)) => {
            eprintln!("  [pdf] skipped {target_width}px @ {dpi} dpi: {e}");
            None
        }
        // A planner/engine failure is a genuine regression this benchmark
        // exists to catch — surface it loudly (the gradient runners' `.unwrap()`
        // panic on engine failure too) instead of silently dropping the series.
        Err(e) => panic!("real-content PDF workload failed at {target_width}px @ {dpi} dpi: {e}"),
    }
}

/// Parsed CLI options for the scalability binary.
struct CliOpts {
    /// Which benchmark family this sweep measures. Decides the engine set, and
    /// with it whether libvips is measured at all (issue #64).
    family: Family,
    /// Where the sweep writes `scalability_results.json`. Defaults to
    /// `report/<family>/`.
    report_dir: std::path::PathBuf,
    /// The swept image sizes. Defaults to [`SWEEP_SIZES`]; `--sizes` narrows
    /// them so a test can drive the real binary over one or two cells in
    /// seconds instead of the full grid up to 280 MP. Nothing in this crate
    /// could drive this binary cheaply before, which is a large part of why
    /// #74 survived a release.
    sizes: Vec<(u32, u32)>,
    /// The swept thread budgets. Empty means the default pair (1 and
    /// num_cpus); `--concurrency` overrides it.
    concurrency: Vec<usize>,
    /// Megapixel cap for the real-content PDF series (`--pdf-max-mp`, default
    /// [`DEFAULT_PDF_MAX_MP`]). Only meaningful on a `pdfium` build; the four
    /// gradient series always run the full sweep.
    #[cfg(feature = "pdfium")]
    pdf_max_mp: f64,
}

fn parse_cli() -> CliOpts {
    #[cfg(feature = "pdfium")]
    let mut pdf_max_mp = DEFAULT_PDF_MAX_MP;
    let mut family_name = DEFAULT_FAMILY.as_str().to_string();
    let mut report_dir: Option<std::path::PathBuf> = None;
    let mut sizes: Vec<(u32, u32)> = SWEEP_SIZES.to_vec();
    let mut concurrency: Vec<usize> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--pdf-max-mp" => {
                let val = args.next();
                #[cfg(feature = "pdfium")]
                {
                    pdf_max_mp = val
                        .as_deref()
                        .and_then(|v| v.parse::<f64>().ok())
                        .filter(|v| *v > 0.0)
                        .unwrap_or_else(|| {
                            eprintln!("--pdf-max-mp needs a positive numeric megapixel value");
                            std::process::exit(2);
                        });
                }
                // On a non-pdfium build the flag is accepted but inert (there
                // is no PDF series to cap); consume its value and move on.
                #[cfg(not(feature = "pdfium"))]
                let _ = val;
            }
            "--family" => {
                family_name = args.next().unwrap_or_else(|| {
                    eprintln!("--family wants a family name");
                    std::process::exit(2);
                });
            }
            "--report-dir" => {
                report_dir = Some(std::path::PathBuf::from(args.next().unwrap_or_else(|| {
                    eprintln!("--report-dir wants a directory");
                    std::process::exit(2);
                })));
            }
            "--sizes" => sizes = parse_sizes(&args.next().unwrap_or_default()),
            "--concurrency" => concurrency = parse_concurrency(&args.next().unwrap_or_default()),
            "-h" | "--help" => {
                println!(
                    "Usage: scalability [--family <name>] [--report-dir <dir>] [--sizes <WxH,WxH>] \
                     [--concurrency <n,n>] [--pdf-max-mp <n>]"
                );
                println!();
                println!("Families:");
                for family in ALL_FAMILIES {
                    let default = if family == DEFAULT_FAMILY {
                        "  (default)"
                    } else {
                        ""
                    };
                    println!("  {:<9} {}{default}", family.as_str(), family.summary());
                }
                println!();
                println!("  --family <name>   Which family to sweep (default: {DEFAULT_FAMILY})");
                println!("  --report-dir <d>  Write the sweep here instead of report/<family>/");
                println!("  --sizes <WxH,..>  Override the swept image sizes");
                println!(
                    "  --concurrency <n,..>  Override the swept thread budgets (default: 1 and num_cpus)"
                );
                println!("  --pdf-max-mp <n>  Cap the real-content PDF series at n megapixels");
                println!(
                    "                   (pdfium builds only; the gradient series are uncapped)."
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("Unknown argument: {other}");
                eprintln!("Run with --help for usage.");
                std::process::exit(2);
            }
        }
    }
    // Refuse `vips` on a build with no libvips in it, loudly and non-zero,
    // rather than sweeping three engines under a comparison's name (issue #64).
    let family = Family::resolve(&family_name).unwrap_or_else(|refusal| {
        eprintln!("{refusal}");
        std::process::exit(refusal.exit_code());
    });
    let report_dir = report_dir.unwrap_or_else(|| {
        family.report_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("report"))
    });

    CliOpts {
        family,
        report_dir,
        sizes,
        concurrency,
        #[cfg(feature = "pdfium")]
        pdf_max_mp,
    }
}

fn main() {
    // Hidden per-cell child subcommand (`--single ...`). Invoked this way the
    // process runs exactly one cell and prints its metrics as JSON; the parent
    // sweep spawns these and reads each child's true per-run RSS via `wait4`
    // (issues #157, #74). Not a `--single` invocation → fall through to the
    // normal sweep. It has to come first, ahead of this binary's own argument
    // parser, exactly as it does in `report`.
    if let Some(code) = harness::maybe_run_single_subcommand() {
        std::process::exit(code);
    }

    let opts = parse_cli();
    #[cfg(not(feature = "pdfium"))]
    let _ = &opts;

    let family = opts.family;
    let report_dir = opts.report_dir.clone();
    fs::create_dir_all(&report_dir).unwrap();

    // The binary to re-invoke per cell. Every engine's peak RSS is read off one
    // of these children rather than off this process (issue #74).
    let exe = harness::current_exe();

    // The family, not the environment, decides whether libvips is measured.
    let has_vips = family.measures_libvips() && vips_available();

    let sizes: &[(u32, u32)] = &opts.sizes;

    println!("=== Engine Scalability Benchmark ({family}) ===");
    println!("Family: {family} — {}", family.summary());
    println!(
        "Workload: SYNTHETIC gradient raster; aspect 1.42:1 matches the \
         California South page (4608x3240 pts)."
    );
    #[cfg(feature = "pdfium")]
    {
        println!(
            "Real-content series: rasterized PDF fixture \
             (fixtures/cc_licenses_mapping.pdf) via PdfiumStripSource streaming, \
             charted as 'streaming-pdf'."
        );
        println!(
            "  caveat: the PDF line is NOT a like-for-like engine comparison with the gradient series —"
        );
        println!(
            "    * end-to-end rasterize+pyramid (pdfium renders each strip inside the timed run) \
             vs the gradient's pyramid-only over a pre-materialised raster;"
        );
        println!(
            "    * RGBA (4 bpp) strips at a matched-but-larger budget, and pdfium serialises every \
             render (no strip-render parallelism);"
        );
        println!(
            "    * and it is the one series still measured IN THIS PROCESS: the four gradient \
             series each run in their own child, so their peak RSS is a true per-run figure \
             (issue #74), while the PDF line's is a shared high-water mark — use its \
             tracked_memory_mb column for the true per-run footprint."
        );
    }
    println!(
        "Sizes: {} points from 512x360 to {}x{}",
        sizes.len(),
        sizes.last().unwrap().0,
        sizes.last().unwrap().1,
    );
    println!(
        "Tile size: {TILE_SIZE}, streaming budget floor: {STREAMING_BUDGET_FLOOR} bytes (auto-scaled per width)",
    );
    if family.measures_libvips() {
        if has_vips {
            println!("libvips CLI: included");
        } else {
            println!("libvips CLI: not found, skipping");
        }
    } else {
        println!("libvips: not part of the {family} family");
    }
    // Measurement-condition guards (contended host / thermal / mismatched
    // oracle #33): a run measured under load, while thermally throttled, or
    // against a different libvips than the container was pinned to build is not
    // comparable to a clean pinned-oracle run — flag it loudly. The wording
    // lives on `Provenance` so this binary and `report` share one source and can
    // never drift (issue #25 review). Only genuine signals trip these; a host
    // run without libvips does not.
    let prov = Provenance::capture();
    println!("Host load (1/5/15m): {}", prov.load_average_line());
    for warning in prov.measurement_condition_warnings() {
        eprintln!("{warning}");
    }
    println!();

    let mut all_points: Vec<ScalabilityPoint> = Vec::new();

    // Derive the fixture's 72-DPI page width ONCE, from the source itself, so
    // the DPI-for-width mapping tracks the actual committed fixture. `None`
    // means the source could not be opened (libpdfium unavailable) — the whole
    // real-content series is then skipped up front (issue #22 review).
    #[cfg(feature = "pdfium")]
    let pdf_base = pdf_base_width();

    // Matched thread budgets: run EVERY engine — including libvips, via a
    // matched `VIPS_CONCURRENCY` — at both a single thread and all cores, so
    // no engine is silently pinned to a different thread count than another
    // (issue #156). The two levels are charted separately, never mixed.
    // `--concurrency` overrides the pair; it exists for the same reason
    // `--sizes` does.
    let ncpu = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let concurrency_levels: Vec<usize> = if !opts.concurrency.is_empty() {
        opts.concurrency.clone()
    } else if ncpu > 1 {
        vec![1, ncpu]
    } else {
        vec![1]
    };
    println!("Thread budgets: {concurrency_levels:?} (1 and num_cpus)");
    println!();

    // Whether the libvips row is measured at all. The family decides; on a
    // build with the FFI compiled in the child can measure it without a `vips`
    // binary on PATH, so the CLI probe is only half the answer.
    let measure_libvips = family.measures_libvips() && (cfg!(feature = "libvips") || has_vips);

    for &conc in &concurrency_levels {
        println!("--- thread budget: {conc} ---");
        for &(w, h) in sizes {
            let mp = w as f64 * h as f64 / 1_000_000.0;
            print!("[c{conc}] {w}x{h} ({mp:.1} MP): ");

            // Every engine below is measured in its OWN child process, so the
            // `ru_maxrss` each row reports is that child's and nobody else's
            // (issue #74). The child picks the libvips measurement path
            // (in-process FFI, else the `vips dzsave` CLI) exactly as the
            // `report` binary's children do, and both honour the matched thread
            // budget (`concurrency_set` / VIPS_CONCURRENCY).
            //
            // A cell that comes back empty is a skip, not an abort: the child
            // logs the engine fault (or the missing libvips) to the inherited
            // stderr and the sweep drops that one point (issue #46).
            if measure_libvips {
                if let Some(m) = measure_cell(&exe, Engine::Libvips, w, h, conc) {
                    print!(
                        "vips={:.0}ms/{:.1}MB(rss)  ",
                        m.wall_time_ms(),
                        m.peak_rss_mb()
                    );
                    all_points.push(point_from_metrics("libvips", conc, &m));
                }
            }

            for (engine, label, tag) in [
                (Engine::Monolithic, "monolithic", "mono"),
                (Engine::Streaming, "streaming", "stream"),
                (Engine::MapReduce, "mapreduce", "mr"),
            ] {
                match measure_cell(&exe, engine, w, h, conc) {
                    Some(m) => {
                        print!(
                            "{tag}={:.0}ms/{:.1}MB(trk)/{:.1}MB(rss)  ",
                            m.wall_time_ms(),
                            m.tracked_memory_mb(),
                            m.peak_rss_mb(),
                        );
                        all_points.push(point_from_metrics(label, conc, &m));
                    }
                    None => print!("{tag}=skipped  "),
                }
            }
            println!();

            // Real-content counterpart (issue #31): rasterize the committed PDF
            // fixture to ~this width via PdfiumStripSource (streaming) and
            // pyramid it through the streaming engine, as the separate
            // "streaming-pdf" series. Feature-gated, so the default build is
            // unaffected. Capped at `--pdf-max-mp` so the fixed vector page is
            // not rendered at absurd DPI where it is pure upsampling (issue #22
            // review).
            #[cfg(feature = "pdfium")]
            if let Some(base_w) = pdf_base {
                if mp <= opts.pdf_max_mp {
                    if let Some(p) = run_pdf_streaming(w, base_w, conc, TILE_SIZE) {
                        println!(
                            "        pdf={:.0}ms/{:.1}MB(rss)  ({}x{} @ {}dpi)",
                            p.wall_time_ms,
                            p.peak_rss_mb,
                            p.width,
                            p.height,
                            pdf_dpi_for_width(w, base_w),
                        );
                        all_points.push(p);
                    }
                } else {
                    println!(
                        "        pdf=skipped (> {:.0} MP cap: pure upsampling of the fixed vector page)",
                        opts.pdf_max_mp,
                    );
                }
            }
        }
    }

    // --- Charts render from scalability_results.json via
    // tools/charts/render.mjs (invoked by run-bench.sh after this writes JSON). ---

    // Save raw data
    let json_path = report_dir.join("scalability_results.json");
    let json = serde_json::to_string_pretty(&all_points).unwrap();
    fs::write(&json_path, &json).unwrap();

    // Print summary table
    println!();
    println!(
        "{:<14} {:<12} {:>10} {:>12} {:>10} {:>11} {:>12} {:>14}",
        "Size",
        "Engine",
        "Time (ms)",
        "Tracked MB",
        "RSS MB",
        "Tiles",
        "T/s/RSS-MB",
        "RSS-MB\u{00b7}s/tile",
    );
    println!("{}", "-".repeat(97));
    for p in &all_points {
        println!(
            "{:<14} {:<12} {:>10.1} {:>12.2} {:>10.2} {:>11} {:>12.1} {:>14.4}",
            format!("{}x{}", p.width, p.height),
            p.engine,
            p.wall_time_ms,
            p.tracked_memory_mb,
            p.peak_rss_mb,
            format_thousands(p.tiles_produced),
            p.tiles_per_second_per_mb,
            p.resource_cost,
        );
    }
    // Units & direction (T = pyramid tiles, never pixels) — phrased to match the
    // shared `COMPARISON_TABLE_LEGEND` so the two human-facing tables agree.
    println!(
        "  Time (ms) / Tracked MB / RSS MB: lower is better. \
         T/s/RSS-MB = throughput per peak-RSS MB (memory efficiency): higher is better."
    );
    println!(
        "  RSS-MB\u{00b7}s/tile = RSS-MB-seconds per tile (resource cost): lower is better. \
         Tracked MB is libviprs-internal (0 for libvips); RSS MB is the cross-engine peak."
    );

    // --- Memory bottleneck analysis ---
    println!();
    println!("=== Memory Bottleneck Analysis ===");
    println!();

    // Group by size and find the largest. By AREA, not by position: `--sizes`
    // takes whatever order it is given, and the analysis below is about the
    // biggest canvas in the sweep, not the last one that happened to be typed.
    let largest = sizes
        .iter()
        .max_by_key(|(w, h)| *w as u64 * *h as u64)
        .unwrap();
    let largest_mp = largest.0 as f64 * largest.1 as f64 / 1_000_000.0;

    // Monolithic bottleneck
    if let Some(mono) = all_points
        .iter()
        .find(|p| p.width == largest.0 && p.engine == "monolithic")
    {
        let canvas_bytes = largest.0 as f64 * largest.1 as f64 * 3.0; // RGB8 = 3 bpp
        let canvas_mb = canvas_bytes / (1024.0 * 1024.0);
        println!(
            "MONOLITHIC at {}x{} ({:.1} MP):",
            largest.0, largest.1, largest_mp,
        );
        println!(
            "  Tracked working set: {:.1} MB — dominated by the full canvas allocation",
            mono.tracked_memory_mb,
        );
        println!(
            "  The source raster ({:.1} MB) is cloned into a canvas-sized buffer.",
            canvas_mb,
        );
        println!("  During downscale, the current level + next level coexist in memory,",);
        println!(
            "  producing peak ≈ canvas + canvas/4 = {:.1} MB.",
            canvas_mb * 1.25,
        );
        println!("  This scales O(width × height) — doubling image dimensions quadruples memory.",);
    }

    // Streaming bottleneck
    if let Some(stream) = all_points
        .iter()
        .find(|p| p.width == largest.0 && p.engine == "streaming")
    {
        println!();
        println!(
            "STREAMING at {}x{} ({:.1} MP), budget {} MB:",
            largest.0,
            largest.1,
            largest_mp,
            streaming_budget_for(STREAMING_BUDGET_FLOOR, largest.0, TILE_SIZE, 3) as f64
                / (1024.0 * 1024.0),
        );
        println!(
            "  Tracked working set: {:.1} MB — bounded by strip height, not canvas area.",
            stream.tracked_memory_mb,
        );
        println!("  The engine holds: current strip + accumulator at each pyramid level",);
        println!("  (geometric series: strip + strip/4 + strip/16 + ...). Strip height is",);
        println!("  maximised within the budget. Memory scales O(width × strip_height),",);
        println!("  independent of image height. The bottleneck is strip width (= canvas width).",);
    }

    // MapReduce bottleneck
    if let Some(mr) = all_points
        .iter()
        .find(|p| p.width == largest.0 && p.engine == "mapreduce")
    {
        println!();
        println!(
            "MAPREDUCE at {}x{} ({:.1} MP), budget {} MB:",
            largest.0,
            largest.1,
            largest_mp,
            streaming_budget_for(STREAMING_BUDGET_FLOOR, largest.0, TILE_SIZE, 3) as f64
                / (1024.0 * 1024.0),
        );
        println!(
            "  Tracked working set: {:.1} MB — same strip-bounded model as streaming.",
            mr.tracked_memory_mb,
        );
        println!("  With K in-flight strips, peak = K × strip_cost + accumulator chain.",);
        println!("  The budget was too small for K>1 in-flight strips at this image width,",);
        println!("  so memory matches streaming. With a larger budget, K>1 trades memory",);
        println!("  for throughput by overlapping strip rendering.",);
    }

    // libvips bottleneck
    if let Some(vips) = all_points
        .iter()
        .find(|p| p.width == largest.0 && p.engine == "libvips")
    {
        println!();
        println!(
            "LIBVIPS at {}x{} ({:.1} MP):",
            largest.0, largest.1, largest_mp,
        );
        println!(
            "  Peak RSS: {:.1} MB — libvips uses a demand-driven pipeline where pixels",
            vips.peak_rss_mb,
        );
        println!("  are computed on demand per-region (O(tile_size²) working set). The RSS",);
        println!("  measured here includes the OS-level allocation footprint, which is higher",);
        println!("  than the logical working set due to memory mapping, page tables, and the",);
        println!("  decoded source image cache.",);
    }

    // Scaling comparison
    println!();
    println!("SCALING SUMMARY:");
    let smallest = sizes
        .iter()
        .min_by_key(|(w, h)| *w as u64 * *h as u64)
        .unwrap();
    let scale_factor =
        (largest.0 as f64 * largest.1 as f64) / (smallest.0 as f64 * smallest.1 as f64);

    for engine in &["libvips", "monolithic", "streaming", "mapreduce"] {
        let small = all_points
            .iter()
            .find(|p| p.width == smallest.0 && p.engine == *engine);
        let large = all_points
            .iter()
            .find(|p| p.width == largest.0 && p.engine == *engine);
        if let (Some(s), Some(l)) = (small, large) {
            let mem_scale = l.peak_rss_mb / s.peak_rss_mb.max(0.01);
            let time_scale = l.wall_time_ms / s.wall_time_ms.max(0.01);
            println!(
                "  {:<12} image area {:.0}x larger → memory {:.1}x, time {:.1}x",
                engine, scale_factor, mem_scale, time_scale,
            );
        }
    }

    println!();
    println!("JSON written to {}", json_path.display());
    println!(
        "Scalability charts (scalability_*.svg) render from that JSON via \
         tools/charts/render.mjs (run-bench.sh invokes it; this binary emits JSON only)."
    );
}

#[cfg(test)]
mod shape_tests {
    use super::ScalabilityPoint;

    /// PRODUCER half of the #44 drift guard for the `scalability`-binary-private
    /// [`ScalabilityPoint`]: the committed `golden_scalability.json` must carry
    /// exactly the field names this struct serializes, so a rename here without
    /// updating the golden (and `tools/charts/render.mjs`, its consumer) fails.
    /// `ScalabilityPoint` is flat (all scalars), so a sorted key-set comparison
    /// captures the whole shape.
    #[test]
    fn golden_scalability_matches_the_serializer_shape() {
        let point = ScalabilityPoint {
            width: 1000,
            height: 1000,
            megapixels: 1.0,
            engine: "monolithic".to_string(),
            concurrency: 1,
            wall_time_ms: 10.0,
            tracked_memory_mb: 2.0,
            peak_rss_mb: 10.0,
            tiles_produced: 16,
            tiles_per_second: 1600.0,
            tiles_per_second_per_mb: 160.0,
            resource_cost: 0.006_25,
        };
        let serialized = serde_json::to_value(&point).unwrap();
        let mut got: Vec<String> = serialized.as_object().unwrap().keys().cloned().collect();
        got.sort();

        let golden_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tools/charts/fixtures/golden_scalability.json");
        let text = std::fs::read_to_string(&golden_path).unwrap();
        let golden: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut want: Vec<String> = golden.as_array().unwrap()[0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        want.sort();

        assert_eq!(
            got, want,
            "golden_scalability.json[0] must mirror the ScalabilityPoint serde field names; if \
             this fails a ScalabilityPoint field changed — update the golden AND \
             tools/charts/render.mjs to match."
        );
    }
}
