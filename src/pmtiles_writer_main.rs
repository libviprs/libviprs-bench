//! `pmtiles-writer`: the PMTiles writer sweep (libviprs-bench #132).
//!
//! ```text
//! pmtiles-writer [--profile ci|full|xl] [--out <path>]
//! ```
//!
//! It emits the storage family's own document envelope, so the same
//! `storage-aggregate --check`, `--archive` and `--verify` that gate a storage
//! sweep gate this one. It is its own binary for the reason `storage` and
//! `engines` are: it builds with no cargo features, and it re-executes itself as
//! the hidden `--writer-single` child that gives every repetition a process that
//! has touched nothing else, which is where the peak RSS comes from.
//!
//! What it measures, and why the baseline is a revision rather than a release,
//! is in `libviprs_bench::storage::writer_sweep`.

use std::path::PathBuf;
use std::process::ExitCode;

use libviprs_bench::storage::{self, cells::Profile, writer_sweep};

/// The counting allocator. A `#[global_allocator]` belongs to a binary, and this
/// is a binary whose scenarios publish heap numbers. Off until a repetition
/// arms it, so the parent and everything unarmed pay one relaxed load.
#[global_allocator]
static HEAP: storage::heap::Counting = storage::heap::Counting;

fn main() -> ExitCode {
    // The child path first: a child must never fall through into a sweep.
    if let Some(code) = writer_sweep::maybe_run_single_subcommand() {
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
                println!("pmtiles-writer [--profile ci|full|xl] [--out <path>]");
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
        "pmtiles-writer: profile {}, writing {}",
        profile.label(),
        path.display()
    );
    if !profile.publishable() {
        eprintln!("pmtiles-writer: the ci profile proves the harness runs; it is never published");
    }

    let document = writer_sweep::run_sweep(profile);
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!("pmtiles-writer: cannot make {}: {e}", parent.display());
        return ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::write(&path, document.to_json()) {
        eprintln!("pmtiles-writer: cannot write {}: {e}", path.display());
        return ExitCode::FAILURE;
    }
    eprintln!("pmtiles-writer: {} cells written", document.cells.len());
    ExitCode::SUCCESS
}

/// The document's path when `--out` does not say: the storage family's report
/// directory, beside `storage-results.json`.
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
                .join(writer_sweep::DOCUMENT_NAME)
        }
    }
}
