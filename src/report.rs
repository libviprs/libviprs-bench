//! Benchmark report generator, one family at a time.
//!
//! Runs a [`Family`](libviprs_bench::family::Family)'s engines across a matrix
//! of image sizes and concurrency levels, collects metrics (CPU time, memory,
//! throughput), and writes, under `report/<family>/`:
//!
//!   benchmark_results.json  — raw metrics for this run
//!   benchmark_history.json  — versioned history across releases
//!   comparison_table.txt    — human-readable summary
//!   verdict_table.txt       — the per-configuration executive verdict
//!
//! The default family is `engines` (monolithic vs streaming vs mapreduce),
//! which needs no cargo features and no libvips anywhere — not the headers, not
//! the FFI, not a `vips` binary on `PATH`. `--family vips` is the old
//! comparison and is refused, loudly and with a non-zero exit, on a build
//! without `--features libvips` (issue #64).
//!
//! One directory per family means two families can never append to each other's
//! history or overwrite each other's charts, and the JS chart renderer draws
//! either of them from nothing but a `--report-dir`.
//!
//! The SVG charts — grouped-bar comparison `chart_*.svg` and history-trend
//! `chart_history_*.svg` — render from that JSON via `tools/charts/render.mjs`
//! (run-bench.sh invokes it after this binary writes the JSON). This binary
//! emits JSON only; the plotters dependency is gone (issue #42).
//!
//! Run: cargo run --release --bin report
//!     cargo run --release --features libvips --bin report -- --family vips
//!
//! Use --release for meaningful timing numbers.

use std::fs;
use std::path::{Path, PathBuf};

use libviprs_bench::family::{ALL_FAMILIES, DEFAULT_FAMILY, Family};
use libviprs_bench::harness;
use libviprs_bench::provenance::Provenance;
use libviprs_bench::{
    BENCH_STREAMING_BUDGET, BENCH_TILE_SIZE, DEFAULT_CONCURRENCY, DEFAULT_SIZES, comparison_table,
    core_git_sha, core_version, create_snapshot, executive_verdict, load_history,
    print_comparison_table, print_savings_summary, push_snapshot, save_history,
};

/// Everything the run is parameterised on. The suite constants are the
/// defaults; the overrides exist so a test (and a smoke run) can drive the real
/// binary over a 256x256 image in a second instead of the full matrix.
struct ReportOpts {
    family: Family,
    report_dir: PathBuf,
    sizes: Vec<(u32, u32)>,
    concurrency: Vec<usize>,
    iters: u32,
    warmup: u32,
}

fn usage() {
    println!("Usage: report [--family <name>] [options]");
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
    println!("Options:");
    println!("  --family <name>        Which family to measure (default: {DEFAULT_FAMILY})");
    println!("  --report-dir <dir>     Write artifacts here instead of report/<family>/");
    println!("  --sizes <WxH,WxH>      Override the swept image sizes");
    println!("  --concurrency <n,n>    Override the swept concurrency levels (0 = serial)");
    println!("  --iters <n>            Timed iterations per cell (env: BENCH_ITERS)");
    println!("  --warmup <n>           Discarded warm-up iterations (env: BENCH_WARMUP)");
    println!("  -h, --help             Show this help and exit");
}

/// Parse `WxH,WxH` into sizes, or die with a message naming the bad token.
fn parse_sizes(raw: &str) -> Vec<(u32, u32)> {
    raw.split(',')
        .map(|token| {
            let token = token.trim();
            let bad = || {
                eprintln!("--sizes wants WxH pairs, comma separated; got {token:?}");
                std::process::exit(2);
            };
            let Some((w, h)) = token.split_once('x') else {
                bad()
            };
            match (w.trim().parse::<u32>(), h.trim().parse::<u32>()) {
                (Ok(w), Ok(h)) if w > 0 && h > 0 => (w, h),
                _ => bad(),
            }
        })
        .collect()
}

fn parse_concurrency(raw: &str) -> Vec<usize> {
    raw.split(',')
        .map(|token| {
            token.trim().parse::<usize>().unwrap_or_else(|_| {
                eprintln!(
                    "--concurrency wants non-negative integers, comma separated; got {token:?}"
                );
                std::process::exit(2);
            })
        })
        .collect()
}

fn parse_u32(flag: &str, raw: Option<String>) -> u32 {
    raw.and_then(|v| v.parse::<u32>().ok()).unwrap_or_else(|| {
        eprintln!("{flag} wants a non-negative integer");
        std::process::exit(2);
    })
}

fn parse_cli() -> ReportOpts {
    let mut family_name = DEFAULT_FAMILY.as_str().to_string();
    let mut report_dir: Option<PathBuf> = None;
    let mut sizes = DEFAULT_SIZES.to_vec();
    let mut concurrency = DEFAULT_CONCURRENCY.to_vec();
    // Statistics: >= 7 timed iterations after a discarded warm-up, each cell in
    // its own child process, engine order interleaved within a size (issue
    // #155). The env vars predate the flags and still work; a flag wins.
    let mut iters: u32 = std::env::var("BENCH_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(harness::DEFAULT_ITERS);
    let mut warmup: u32 = std::env::var("BENCH_WARMUP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(harness::DEFAULT_WARMUP);

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                usage();
                std::process::exit(0);
            }
            "--family" => {
                family_name = args.next().unwrap_or_else(|| {
                    eprintln!("--family wants a family name");
                    std::process::exit(2);
                });
            }
            "--report-dir" => {
                report_dir = Some(PathBuf::from(args.next().unwrap_or_else(|| {
                    eprintln!("--report-dir wants a directory");
                    std::process::exit(2);
                })));
            }
            "--sizes" => sizes = parse_sizes(&args.next().unwrap_or_default()),
            "--concurrency" => concurrency = parse_concurrency(&args.next().unwrap_or_default()),
            "--iters" => iters = parse_u32("--iters", args.next()),
            "--warmup" => warmup = parse_u32("--warmup", args.next()),
            other => {
                eprintln!("Unknown argument: {other}");
                eprintln!("Run with --help for usage.");
                std::process::exit(2);
            }
        }
    }

    // The refusal is the point of `resolve`: asking for `vips` on a build with
    // no libvips in it used to run whatever it could and write a
    // comparison-shaped report with nothing to compare against.
    let family = Family::resolve(&family_name).unwrap_or_else(|refusal| {
        eprintln!("{refusal}");
        std::process::exit(refusal.exit_code());
    });

    let report_dir = report_dir.unwrap_or_else(|| {
        family.report_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("report"))
    });

    ReportOpts {
        family,
        report_dir,
        sizes,
        concurrency,
        iters,
        warmup,
    }
}

fn main() {
    // Hidden per-cell child subcommand (`--single …`). When invoked this
    // way the process runs exactly one cell and prints its metrics as JSON;
    // the parent harness spawns these and reads each child's true per-run
    // RSS via wait4 (issue #157). Not a `--single` invocation → fall through
    // to the normal report run.
    if let Some(code) = harness::maybe_run_single_subcommand() {
        std::process::exit(code);
    }

    // Hidden `--print-core`: the version-matrix runner rebuilds this binary per
    // tag and asks it which core it linked, to verify the measured artifact's
    // identity matches the ref before recording a snapshot (issue #19).
    if let Some(code) = harness::maybe_run_print_core_subcommand() {
        std::process::exit(code);
    }

    let opts = parse_cli();
    let family = opts.family;
    let report_dir = opts.report_dir.clone();
    fs::create_dir_all(&report_dir).unwrap();

    // The canonical suite, shared with the version-matrix runner so the
    // everyday axis and the release-history axis measure the identical sizes,
    // concurrency, tile size, and budget (issue #19).
    let sizes: &[(u32, u32)] = &opts.sizes;
    let concurrency_levels: &[usize] = &opts.concurrency;
    let tile_size: u32 = BENCH_TILE_SIZE;
    let streaming_budget: u64 = BENCH_STREAMING_BUDGET; // 1 MB
    let iters = opts.iters;
    let warmup = opts.warmup;

    let prov = Provenance::capture();
    match family {
        Family::Vips => println!("=== libviprs vs libvips comparison benchmark ({family}) ==="),
        _ => println!("=== libviprs engine benchmark ({family}) ==="),
    }
    println!("    family: {family} — {}", family.summary());
    println!(
        "    measured libviprs core: {} ({})",
        core_version(),
        core_git_sha()
    );
    println!("    bench harness: {}", env!("CARGO_PKG_VERSION"));
    println!("    environment:  {}", prov.fingerprint());
    println!("    cpu: {} ({} cpus)", prov.host.cpu_model, prov.host.ncpu);
    println!("    host load (1/5/15m): {}", prov.load_average_line());
    if family.measures_libvips() {
        println!(
            "    libvips oracle: measured {} / pinned {}",
            prov.libvips_version, prov.pinned_libvips_version
        );
    }
    // Measurement-condition guards: a run measured while the box was busy or
    // thermally throttled, or against a different libvips than the environment
    // was pinned to build (#33), is not comparable to a clean pinned-oracle run
    // — say so loudly on stderr. The wording lives on `Provenance` so the
    // `report` and `scalability` binaries share one source and can never drift
    // (issue #25 review). A missing load/thermal sample, or a host run with no
    // libvips, never trips these.
    for warning in prov.measurement_condition_warnings() {
        eprintln!("{warning}");
    }
    println!();
    println!(
        "Tile size: {tile_size}, streaming/mapreduce budget floor: {streaming_budget} bytes \
         (auto-scaled per width to admit the worst-case tile-aligned strip, so cross-size rows \
         are not under one identical budget — see each row's effective budget in the JSON)"
    );
    println!("Iterations: {iters} timed + {warmup} warm-up per cell (child-isolated)");
    println!(
        "Image sizes: {:?}",
        sizes
            .iter()
            .map(|(w, h)| format!("{w}x{h}"))
            .collect::<Vec<_>>()
    );
    println!("Concurrency levels: {concurrency_levels:?}");
    println!();

    // Engine set: a pure function of the family. It is NOT conditioned on
    // whether a `vips` binary happens to be on PATH — an `engines` run measures
    // the same three engines on a box that has libvips installed as on one that
    // does not, which is what makes two runs of the family comparable at all
    // (issue #64).
    let engines = family.engines();
    eprintln!(
        "engines under test: {}",
        engines
            .iter()
            .map(|e| e.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let exe = harness::current_exe();
    let results = harness::run_isolated_suite(
        &exe,
        sizes,
        concurrency_levels,
        &engines,
        tile_size,
        streaming_budget,
        iters,
        warmup,
    );

    // Print full table
    print_comparison_table(&results);

    // Print savings summary
    print_savings_summary(&results);

    // Output-equivalence: pixel-level PSNR spot-check vs libvips. The geometry
    // gate (tile count + per-level grid) runs inside the suite; this surfaces
    // each engine's own pixel-fidelity score so a fast-but-visually-wrong
    // engine is visible rather than silently passing on tile count alone
    // (issue #23 / #32). Each (engine, size, concurrency) carries its OWN
    // score, so every scored row is printed on its own line.
    if family.measures_libvips() {
        print_equivalence_section(&results);
    }

    // Executive verdict: per size, the winning engine on each axis plus
    // every engine's ratio vs libvips in the *same* snapshot (issue #160).
    let verdict = executive_verdict(&results);
    println!();
    print!("{verdict}");
    fs::write(report_dir.join("verdict_table.txt"), &verdict).unwrap();

    // Write JSON for this run
    let json_path = report_dir.join("benchmark_results.json");
    let json = serde_json::to_string_pretty(&results).unwrap();
    fs::write(&json_path, &json).unwrap();
    println!();
    println!("JSON results written to {}", json_path.display());

    // Write text report. The metadata header records what the numbers are and
    // the conditions they were measured under (environment fingerprint, host
    // load, cold-vs-warm iteration policy), then the shared `comparison_table`
    // renders the same table stdout printed — with its units/direction legend —
    // so the committed artifact is self-describing and can never drift from the
    // console output.
    let txt_path = report_dir.join("comparison_table.txt");
    let mut txt = String::new();
    txt.push_str(&format!(
        "libviprs benchmark, {family} family — {}\n\
         engines: {}\n\
         measured libviprs core: {} ({})\n\
         bench harness: {}\n\
         environment: {}\n\
         host load (1/5/15m): {}\n\
         Tile size: {tile_size}, streaming/mapreduce budget floor: {streaming_budget} bytes \
         (auto-scaled per width; per-row effective budget in benchmark_results.json)\n\
         Iterations: {iters} timed + {warmup} warm-up per cell (child-isolated).\n\n",
        family.summary(),
        engines
            .iter()
            .map(|e| e.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        core_version(),
        core_git_sha(),
        env!("CARGO_PKG_VERSION"),
        prov.fingerprint(),
        prov.load_average_line(),
    ));

    txt.push_str(&comparison_table(&results));
    fs::write(&txt_path, &txt).unwrap();
    println!("Text report written to {}", txt_path.display());

    // --- SVG charts ---
    //
    // The grouped-bar comparison charts (chart_*.svg) now render from
    // benchmark_results.json by tools/charts/render.mjs — the JS chart migration
    // is finished and the plotters dependency is dropped (issue #42). run-bench.sh
    // invokes render.mjs after this binary writes the JSON, so a normal run
    // refreshes both the comparison and history-trend SVGs.
    println!();
    println!(
        "Comparison charts render from {} via tools/charts/render.mjs",
        json_path.display()
    );

    // --- Versioned benchmark history ---
    //
    // If the existing history file is corrupt, I refuse to overwrite it:
    // appending a fresh snapshot would clobber every prior run. I keep
    // the old file in place, report the problem, and skip this run's
    // append so the accumulated history survives for inspection/repair.
    let history_path = report_dir.join("benchmark_history.json");
    println!();
    match load_history(&history_path) {
        Ok(mut history) => {
            // Persist the SAME provenance captured at the top of the run (before
            // any timed work), so the load/thermal the history file records is
            // the ambient pre-run condition its own doc promises — and equals the
            // value this run printed and wrote to comparison_table.txt. Capturing
            // fresh here would instead record the tail of the benchmark's OWN load
            // curve, disagreeing with the printed/txt figure (issue #25 review).
            let snapshot = create_snapshot(
                family,
                prov.clone(),
                results.clone(),
                tile_size,
                streaming_budget,
            );
            // `push_snapshot`, not a bare push: a history file holds one
            // family, because a trend across two engine sets is not a trend
            // (issue #64). This is where a `--report-dir` pointed at another
            // family's directory is caught.
            if let Err(e) = push_snapshot(&mut history, snapshot) {
                eprintln!("error: {e}");
                eprintln!("This run's snapshot was not persisted; prior history is intact.");
                eprintln!(
                    "The measurements this run produced are still in {}.",
                    json_path.display()
                );
                std::process::exit(3);
            }
            match save_history(&history_path, &history) {
                Ok(()) => {
                    println!(
                        "Benchmark history updated: {} entries in {}",
                        history.len(),
                        history_path.display()
                    );

                    // History trend SVGs are rendered from benchmark_history.json by
                    // tools/charts/render.mjs (invoked by run-bench.sh after this
                    // binary writes the JSON). A trend needs >= 2 snapshots.
                    if history.len() < 2 {
                        println!("(run again on a different version to generate trend charts)");
                    } else {
                        println!(
                            "History trend charts render from {} via tools/charts/render.mjs",
                            history_path.display()
                        );
                    }
                }
                Err(e) => {
                    eprintln!("warning: {e}");
                    eprintln!("This run's snapshot was not persisted; prior history is intact.");
                }
            }
        }
        Err(e) => {
            eprintln!("warning: {e}");
            eprintln!(
                "Leaving {} untouched so prior history is not discarded.",
                history_path.display()
            );
            eprintln!("Fix or move the file, then re-run to resume appending snapshots.");
            eprintln!("Skipping history trend charts for this run.");
        }
    }
}

/// The pixel-level PSNR spot-check against the libvips reference. Only the
/// `vips` family has a reference to spot-check against, so this whole section
/// is skipped for the libviprs-only families rather than printed empty.
fn print_equivalence_section(results: &[libviprs_bench::RunMetrics]) {
    println!();
    println!("=== Output-equivalence: mid-pyramid tile PSNR vs libvips ===");
    let mut any = false;
    for r in results {
        let Some(psnr) = r.equivalence_psnr_db else {
            continue;
        };
        any = true;
        let verdict = if psnr >= harness::MIN_TILE_PSNR_DB {
            "OK"
        } else {
            "FAIL"
        };
        let key = format!("{}x{} c{} {}", r.width, r.height, r.concurrency, r.engine);
        println!("  {key:<28} {psnr:>7.1} dB  [{verdict}]");
    }
    if any {
        println!(
            "  threshold: {:.0} dB (near-lossless), advisory only",
            harness::MIN_TILE_PSNR_DB
        );
    } else if libviprs_bench::vips_available() {
        // libvips ran but no size produced a comparable multi-tile mid level
        // (e.g. a smoke run over tiny images) — distinct from libvips absent.
        println!("  (no comparable mid level for the configured sizes — pixel spot-check skipped)");
    } else {
        println!("  (libvips unavailable — pixel spot-check skipped)");
    }
}
