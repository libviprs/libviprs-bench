//! `version_matrix` — build + benchmark the harness across a series of libviprs
//! releases in one invocation, appending the release-history axis to
//! `benchmark_history.json` (issues #19, #26, #27).
//!
//! Usage:
//!   cargo run --release --bin version_matrix -- --versions v0.2.0,v0.3.1,HEAD
//!   cargo run --release --bin version_matrix -- --versions HEAD \
//!       --history /tmp/hist.json --sizes 512x512,1024x1024 --concurrency 0
//!
//! Per version it checks `../libviprs` out at the ref into a throwaway git
//! worktree, rebuilds the harness against it with the pinned `[profile.release]`,
//! runs the identical isolated suite, and appends one tagged,
//! environment-fingerprinted snapshot — producing the release-history axis in
//! one go instead of manual accretion. A version that will not build (an old
//! tag against today's deps) is skipped with a warning; the sweep continues.
//!
//! Set `BENCH_ITERS` / `BENCH_WARMUP` to override the per-cell iteration counts
//! (e.g. a fast smoke: `BENCH_ITERS=1 BENCH_WARMUP=0`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use libviprs_bench::harness;
use libviprs_bench::provenance::Provenance;
use libviprs_bench::version_matrix::{
    MatrixConfig, VersionOutcome, core_repo_dir, run_matrix, version_key,
};

const USAGE: &str = "\
usage: version_matrix --versions <tag,tag,HEAD> [options]

  --versions <list>      comma-separated refs (tags/SHAs/HEAD) to benchmark
  --history <path>       history JSON to append to
                         (default: <crate>/report/benchmark_history.json)
  --sizes <WxH,...>      image sizes (default: 512x512,1024x1024,2048x2048,4096x4096)
  --concurrency <N,...>  concurrency levels (default: 0,4)

env: BENCH_ITERS / BENCH_WARMUP override per-cell iteration counts.";

struct Options {
    versions: Vec<String>,
    history: PathBuf,
    sizes: Option<Vec<(u32, u32)>>,
    concurrency: Option<Vec<usize>>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Options, String> {
        let default_history = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("report")
            .join("benchmark_history.json");
        let mut versions: Option<Vec<String>> = None;
        let mut history = default_history;
        let mut sizes = None;
        let mut concurrency = None;

        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--versions" => versions = Some(parse_versions(next(args, &mut i, "--versions")?)?),
                "--history" => history = PathBuf::from(next(args, &mut i, "--history")?),
                "--sizes" => sizes = Some(parse_sizes(next(args, &mut i, "--sizes")?)?),
                "--concurrency" => {
                    concurrency = Some(parse_concurrency(next(args, &mut i, "--concurrency")?)?)
                }
                "-h" | "--help" => return Err("help".to_string()),
                other => return Err(format!("unknown argument: {other}")),
            }
            i += 1;
        }

        let versions = versions.ok_or("--versions is required")?;
        if versions.is_empty() {
            return Err("--versions must list at least one ref".to_string());
        }
        Ok(Options {
            versions,
            history,
            sizes,
            concurrency,
        })
    }
}

fn next<'a>(args: &'a [String], i: &mut usize, flag: &str) -> Result<&'a str, String> {
    *i += 1;
    args.get(*i)
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn parse_versions(s: &str) -> Result<Vec<String>, String> {
    let v: Vec<String> = s
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if v.is_empty() {
        return Err("--versions is empty".to_string());
    }
    Ok(v)
}

fn parse_sizes(s: &str) -> Result<Vec<(u32, u32)>, String> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let (w, h) = pair
                .split_once(['x', 'X'])
                .ok_or_else(|| format!("bad size '{pair}', expected WxH"))?;
            let w = w
                .trim()
                .parse()
                .map_err(|_| format!("bad width in '{pair}'"))?;
            let h = h
                .trim()
                .parse()
                .map_err(|_| format!("bad height in '{pair}'"))?;
            Ok((w, h))
        })
        .collect()
}

fn parse_concurrency(s: &str) -> Result<Vec<usize>, String> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|n| n.parse().map_err(|_| format!("bad concurrency '{n}'")))
        .collect()
}

fn main() -> ExitCode {
    // Hidden per-cell child subcommand: the built `report` harness is what runs
    // `--single` children, but a reused/aliased invocation of this binary could
    // land here too, so honour it for safety before anything else.
    if let Some(code) = harness::maybe_run_single_subcommand() {
        return ExitCode::from(code as u8);
    }

    let args: Vec<String> = std::env::args().collect();
    let opts = match Options::parse(&args[1..]) {
        Ok(o) => o,
        Err(msg) if msg == "help" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(msg) => {
            eprintln!("error: {msg}\n");
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    let mut cfg = MatrixConfig::default();
    if let Some(sizes) = opts.sizes {
        cfg.sizes = sizes;
    }
    if let Some(concurrency) = opts.concurrency {
        cfg.concurrency = concurrency;
    }
    cfg.iters = std::env::var("BENCH_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(cfg.iters);
    cfg.warmup = std::env::var("BENCH_WARMUP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(cfg.warmup);

    let repo = core_repo_dir();
    let prov = Provenance::capture();

    println!("=== libviprs version-matrix runner ===");
    println!("    core repo:   {}", repo.display());
    println!("    history:     {}", opts.history.display());
    println!("    versions:    {}", opts.versions.join(", "));
    println!("    environment: {}", prov.fingerprint());
    println!(
        "    cpu:         {} ({} cpus)",
        prov.host.cpu_model, prov.host.ncpu
    );
    println!(
        "    suite:       sizes={:?} concurrency={:?} iters={}+{} warmup",
        cfg.sizes, cfg.concurrency, cfg.iters, cfg.warmup
    );
    println!();

    if let Some(parent) = opts.history.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!(
                "error: cannot create history directory {}: {e}",
                parent.display()
            );
            return ExitCode::FAILURE;
        }
    }

    let outcomes = run_matrix(&repo, &opts.versions, &cfg, &opts.history);

    println!();
    println!("=== version-matrix summary ===");
    let mut appended = 0usize;
    let mut skipped = 0usize;
    for outcome in &outcomes {
        match outcome {
            VersionOutcome::Appended {
                refname,
                version,
                short_sha,
                entries,
            } => {
                appended += 1;
                println!(
                    "  ok    {refname:<16} -> {} ({entries} on record)",
                    version_key(version, short_sha)
                );
            }
            VersionOutcome::Skipped { refname, reason } => {
                skipped += 1;
                println!("  skip  {refname:<16} -> {reason}");
            }
        }
    }
    println!();
    println!(
        "{appended} appended, {skipped} skipped. History: {}",
        opts.history.display()
    );

    if appended == 0 {
        eprintln!("no versions were successfully benchmarked");
        return ExitCode::FAILURE;
    }
    if skipped > 0 {
        // Partial success: surface a non-zero code so scripts/CI notice, but
        // the snapshots that did land are already persisted.
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
