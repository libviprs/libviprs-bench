//! `pvec-sink`: the PVec row of the storage family (libviprs/libviprs#1331).
//!
//! ```text
//! pvec-sink [--profile ci|full|xl] [--out <path>]
//! ```
//!
//! Emits the storage family's document envelope, so `storage-aggregate --check`, `--archive` and
//! `--verify` gate it like any storage sweep. It re-executes itself as the hidden `--pvec-single`
//! child that gives every repetition a process that has touched nothing else. What it measures is
//! in `libviprs_bench::storage::pvec_sweep`.

use std::path::PathBuf;
use std::process::ExitCode;

use libviprs_bench::storage::{self, cells::Profile, pvec_sweep};

/// The counting allocator, armed per repetition by the child.
#[global_allocator]
static HEAP: storage::heap::Counting = storage::heap::Counting;

fn main() -> ExitCode {
    if let Some(code) = pvec_sweep::maybe_run_single_subcommand() {
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
                println!("pvec-sink [--profile ci|full|xl] [--out <path>]");
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
    eprintln!("pvec-sink: profile {}, writing {}", profile.label(), path.display());
    if !profile.publishable() {
        eprintln!("pvec-sink: the ci profile proves the harness runs; it is never published");
    }
    let document = pvec_sweep::run_sweep(profile);
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!("pvec-sink: cannot make {}: {e}", parent.display());
        return ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::write(&path, document.to_json()) {
        eprintln!("pvec-sink: cannot write {}: {e}", path.display());
        return ExitCode::FAILURE;
    }
    eprintln!("pvec-sink: {} cells written", document.cells.len());
    ExitCode::SUCCESS
}

fn default_out() -> PathBuf {
    match std::env::var("LIBVIPRS_BENCH_JSON") {
        Ok(path) if !path.is_empty() => PathBuf::from(path),
        _ => {
            let root = match std::env::var("LIBVIPRS_BENCH_REPORT_DIR") {
                Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
                _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("report"),
            };
            libviprs_bench::family::Family::Storage
                .report_dir(&root)
                .join(pvec_sweep::DOCUMENT_NAME)
        }
    }
}
