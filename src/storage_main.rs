//! The `storage` family's runner: PMTiles against a directory tree.
//!
//! One family, one binary. `report`, `scalability` and `version_matrix` measure
//! engine sets and take `--family engines` or `--family vips`; this one
//! measures a storage comparison and takes `--family storage`. The split is not
//! tidiness: this binary builds with no cargo features at all, which is what
//! lets the family run in the mirrored `check` job rather than the
//! libvips-linked one, and folding it into `report` would put the FFI back in
//! the graph of a run that never calls it.
//!
//! It is also its own child: the hidden `--storage-single` subcommand runs
//! exactly one scenario against one `(backend, cell)` and prints the result on
//! stdout, which is how a cell gets a process that has touched nothing else.
//!
//! ```text
//! storage --family storage --profile ci
//! ```
//!
//! The document lands in the family's own report directory,
//! `report/storage/storage-results.json`, like every other family's artifacts.

use std::path::PathBuf;
use std::process::ExitCode;

use libviprs_bench::family::Family;
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
            "--family" => {
                i += 1;
                let Some(name) = argv.get(i) else {
                    eprintln!("--family wants a family name");
                    return ExitCode::from(2);
                };
                // This binary measures one family. Asking it for another is
                // refused rather than quietly measured as storage, which is
                // the same rule `report` follows in the other direction.
                match Family::parse(name) {
                    Some(Family::Storage) => {}
                    Some(other) => {
                        eprintln!(
                            "the `storage` binary measures the {} family. The {other} family is \
                             measured by `report`, `scalability` and `version_matrix`: run \
                             `cargo run --release --bin report -- --family {other}`.",
                            Family::Storage
                        );
                        return ExitCode::from(2);
                    }
                    None => {
                        eprintln!(
                            "unknown benchmark family {name:?}. This binary measures storage"
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
                println!("storage [--family storage] [--profile ci|full|xl] [--out <path>]");
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
        "storage: family {}, profile {}, writing {}",
        Family::Storage,
        profile.label(),
        path.display()
    );
    if !profile.publishable() {
        eprintln!("storage: the ci profile proves the harness runs; it is never published");
    }

    let document = storage::run_sweep(profile);
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!("storage: cannot make {}: {e}", parent.display());
        return ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::write(&path, document.to_json()) {
        eprintln!("storage: cannot write {}: {e}", path.display());
        return ExitCode::FAILURE;
    }
    eprintln!("storage: {} cells written", document.cells.len());
    ExitCode::SUCCESS
}

/// The document's path when `--out` does not say.
///
/// `LIBVIPRS_BENCH_JSON` names the file outright; `LIBVIPRS_BENCH_REPORT_DIR`
/// moves the report root and the family layout under it stays. Neither is a
/// flat path beside the family directories.
fn default_out() -> PathBuf {
    match std::env::var("LIBVIPRS_BENCH_JSON") {
        Ok(path) if !path.is_empty() => PathBuf::from(path),
        _ => storage::default_output_path(&report_root()),
    }
}

fn report_root() -> PathBuf {
    match std::env::var("LIBVIPRS_BENCH_REPORT_DIR") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("report"),
    }
}
