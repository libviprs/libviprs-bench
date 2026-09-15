//! The `engines` family's document runner: monolithic against streaming
//! against mapreduce, with repetitions and provenance.
//!
//! One family, one binary, the same split the `storage` family has. It is also
//! its own child: [`harness::maybe_run_single_subcommand`] dispatches the
//! hidden `--single` subcommand, which runs exactly one engine at one canvas
//! and thread budget in a process that has touched nothing else and prints its
//! metrics on stdout. That is where the per-run peak RSS comes from, and it is
//! why this binary re-executes itself rather than measuring in a loop.
//!
//! ```text
//! engines --family engines --profile ci
//! ```
//!
//! The document lands in the family's own report directory,
//! `report/engines/engines-results.json`, beside the `scalability_results.json`
//! the old sweep writes. The two do not replace each other here: this one is
//! the archivable artefact, that one is the bare array the charts still read
//! until the page moves.

use std::path::PathBuf;
use std::process::ExitCode;

use libviprs_bench::engines::{self, cells::Profile};
use libviprs_bench::family::Family;
use libviprs_bench::harness;

fn main() -> ExitCode {
    // The child path first: a child must never fall through into a sweep.
    if let Some(code) = harness::maybe_run_single_subcommand() {
        return ExitCode::from(code as u8);
    }
    if let Some(code) = harness::maybe_run_print_core_subcommand() {
        return ExitCode::from(code as u8);
    }

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut profile = Profile::from_env();
    let mut out: Option<PathBuf> = None;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--family" => {
                i += 1;
                let Some(name) = argv.get(i) else {
                    eprintln!("--family wants a family name");
                    return ExitCode::from(2);
                };
                // This binary measures one family. Asking it for another is
                // refused rather than quietly measured as engines, which is the
                // rule every runner in this crate follows.
                match Family::parse(name) {
                    Some(Family::Engines) => {}
                    Some(Family::Storage) => {
                        eprintln!(
                            "the storage family is measured by the `storage` binary: run \
                             `cargo run --release --bin storage -- --family storage`."
                        );
                        return ExitCode::from(2);
                    }
                    Some(other) => {
                        eprintln!(
                            "the {other} family is measured by `report`, `scalability` and \
                             `version_matrix`, not by this one: run \
                             `cargo run --release --bin report -- --family {other}`."
                        );
                        return ExitCode::from(2);
                    }
                    None => {
                        eprintln!(
                            "unknown benchmark family {name:?}. This binary measures engines"
                        );
                        return ExitCode::from(2);
                    }
                }
            }
            "--profile" => {
                i += 1;
                match argv.get(i).and_then(|v| Profile::parse(v)) {
                    Some(p) => profile = p,
                    None => {
                        eprintln!("--profile takes ci, full or xl");
                        return ExitCode::from(2);
                    }
                }
            }
            "--out" => {
                i += 1;
                match argv.get(i) {
                    Some(path) => out = Some(PathBuf::from(path)),
                    None => {
                        eprintln!("--out takes a path");
                        return ExitCode::from(2);
                    }
                }
            }
            "-h" | "--help" => {
                println!("engines [--family engines] [--profile ci|full|xl] [--out <path>]");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument {other:?}");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }

    let path = out.unwrap_or_else(default_out);
    eprintln!(
        "engines: family {}, profile {}, writing {}",
        Family::Engines,
        profile.label(),
        path.display()
    );
    if !profile.publishable() {
        eprintln!("engines: the ci profile proves the harness runs; it is never published");
    }

    let document = engines::run_sweep(profile);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("engines: cannot make {}: {e}", parent.display());
            return ExitCode::FAILURE;
        }
    }
    if let Err(e) = std::fs::write(&path, document.to_json()) {
        eprintln!("engines: cannot write {}: {e}", path.display());
        return ExitCode::FAILURE;
    }
    eprintln!("engines: {} cells written", document.cells.len());
    ExitCode::SUCCESS
}

/// The document's path when `--out` does not say.
fn default_out() -> PathBuf {
    match std::env::var("LIBVIPRS_BENCH_JSON") {
        Ok(path) if !path.is_empty() => PathBuf::from(path),
        _ => engines::default_output_path(&report_root()),
    }
}

fn report_root() -> PathBuf {
    match std::env::var("LIBVIPRS_BENCH_REPORT_DIR") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("report"),
    }
}
