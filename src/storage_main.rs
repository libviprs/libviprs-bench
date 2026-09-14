//! The `storage` family runner: PMTiles against a directory tree.
//!
//! Run it and it writes one document per sweep. It is also its own child: the
//! hidden `--storage-single` subcommand runs exactly one scenario against one
//! `(backend, cell)` and prints the result on stdout, which is how a cell gets
//! a process that has touched nothing else.
//!
//! ```text
//! storage --profile ci --out report/storage-results.json
//! ```
//!
//! This binary does not link libvips and does not need it. The comparison it
//! makes is libviprs against libviprs.

use std::path::PathBuf;
use std::process::ExitCode;

use libviprs_bench::storage::{self, cells::Profile};

fn main() -> ExitCode {
    // The child path first: a child must never fall through into a sweep.
    if let Some(code) = storage::maybe_run_single_subcommand() {
        return ExitCode::from(code as u8);
    }

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut profile = Profile::from_env();
    let mut out: Option<PathBuf> = None;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
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
                println!("storage [--profile ci|full|xl] [--out <path>]");
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
        "storage: profile {}, writing {}",
        profile.label(),
        path.display()
    );
    if !profile.publishable() {
        eprintln!("storage: the ci profile proves the harness runs; it is never published");
    }

    let document = storage::run_sweep(profile);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("storage: cannot make {}: {e}", parent.display());
            return ExitCode::FAILURE;
        }
    }
    if let Err(e) = std::fs::write(&path, document.to_json()) {
        eprintln!("storage: cannot write {}: {e}", path.display());
        return ExitCode::FAILURE;
    }
    eprintln!("storage: {} cells written", document.cells.len());
    ExitCode::SUCCESS
}

fn default_out() -> PathBuf {
    match std::env::var("LIBVIPRS_BENCH_JSON") {
        Ok(path) if !path.is_empty() => PathBuf::from(path),
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("report/storage-results.json"),
    }
}
