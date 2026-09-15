//! `storage-aggregate`: the door to the archive.
//!
//! It refuses rather than reports. That is the whole design, and it is a
//! reaction to a specific artefact: `docs/pmtiles-benchmarks.md` publishes 480
//! lines of numbers, records no platform, and reads as authoritative because
//! nothing in it says otherwise. An aggregator that had averaged that run in
//! with a footnote would have produced the same page. One that refuses it
//! produces no page at all, which is the correct output for evidence that
//! cannot be checked.
//!
//! ```text
//! storage-aggregate --check    <document.json>
//! storage-aggregate --archive  <document.json> [--root <dir>]
//! storage-aggregate --verify   <document.json | archived run>
//! storage-aggregate --provenance [--scratch <dir>]
//! ```
//!
//! `--check` says whether a document would be archived and why not. `--archive`
//! admits it, seals it with the four digests and files it under a run id
//! derived from the document's own evidence. `--verify` recomputes all four
//! digests and names the block that moved. `--provenance` prints the block this
//! binary would stamp right now, with the warnings, so a sweep that is going to
//! be refused can be refused before it costs forty minutes rather than after.
//!
//! Exit status is 0 when the document is admissible or verifies, 1 when it is
//! refused, and 2 when the invocation itself was wrong. A refusal is a 1 and
//! not a 2 on purpose: it is an answer, not a failure to run.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use libviprs_bench::provenance::Provenance;
use libviprs_bench::storage::archive::{self, ArchiveError};
use libviprs_bench::storage::integrity;
use serde_json::{Value, json};

/// Exit 0: the document is admissible, or it verifies.
const OK: u8 = 0;
/// Exit 1: the document is refused, or a digest moved. An answer, not a crash.
const REFUSED: u8 = 1;
/// Exit 2: the invocation was wrong, or a file could not be read.
const USAGE: u8 = 2;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("storage-aggregate: {message}");
            ExitCode::from(USAGE)
        }
    }
}

fn run(args: &[String]) -> Result<u8, String> {
    let mut mode: Option<&str> = None;
    let mut document: Option<PathBuf> = None;
    // `None` until `--root` says otherwise, and then derived from the
    // document's own `family`. A flat default would file an `engines` document
    // under `archive/storage`, where the next `storage` run started in the same
    // second against the same commit would collide with it on a run id neither
    // is wrong about.
    let mut root: Option<PathBuf> = None;
    let mut scratch = std::env::temp_dir();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--check" | "--archive" | "--verify" => {
                if mode.is_some() {
                    return Err(
                        "give exactly one of --check, --archive, --verify, --provenance"
                            .to_string(),
                    );
                }
                mode = Some(match args[i].as_str() {
                    "--check" => "check",
                    "--archive" => "archive",
                    _ => "verify",
                });
                i += 1;
                document =
                    Some(PathBuf::from(args.get(i).ok_or_else(|| {
                        format!("{} needs a document path", args[i - 1])
                    })?));
            }
            "--provenance" => {
                if mode.is_some() {
                    return Err("give exactly one mode".to_string());
                }
                mode = Some("provenance");
            }
            "--root" => {
                i += 1;
                root = Some(PathBuf::from(
                    args.get(i).ok_or("--root needs a directory")?,
                ));
            }
            "--scratch" => {
                i += 1;
                scratch = PathBuf::from(args.get(i).ok_or("--scratch needs a directory")?);
            }
            "-h" | "--help" => {
                print_usage();
                return Ok(OK);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
        i += 1;
    }

    match mode {
        Some("provenance") => provenance(&scratch),
        Some("check") => check(&document.expect("parsed with the mode")),
        Some("archive") => do_archive(&document.expect("parsed with the mode"), root.as_deref()),
        Some("verify") => verify(&document.expect("parsed with the mode")),
        _ => {
            print_usage();
            Err("no mode given".to_string())
        }
    }
}

fn print_usage() {
    eprintln!(
        "storage-aggregate --check <document.json>\n\
         storage-aggregate --archive <document.json> [--root <dir>]\n\
         storage-aggregate --verify <document.json>\n\
         storage-aggregate --provenance [--scratch <dir>]\n\
         \n\
         Exit 0 admissible or verified, 1 refused, 2 bad invocation."
    );
}

/// Read a document as the bytes it is stored as.
///
/// Bytes rather than a parsed value, because every mode below needs the text:
/// `--verify` digests it and `--archive` seals it. Parsing it is sound because
/// `Cargo.toml` turns on serde_json's `float_roundtrip`; without that the reader
/// is not correctly rounded and a re-parsed document does not digest to what its
/// producer wrote. `the_json_reader_is_correctly_rounded` is the canary.
fn read_document(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map_err(|err| format!("{} could not be read: {err}", path.display()))
}

/// Say whether a document would be archived, and if not, every reason.
fn check(path: &Path) -> Result<u8, String> {
    let text = read_document(path)?;
    let refusals =
        archive::admit_text(&text).map_err(|err| format!("{}: {err}", path.display()))?;
    if refusals.is_empty() {
        println!("{}: admissible", path.display());
        return Ok(OK);
    }
    eprintln!("{}: refused for {} reasons", path.display(), refusals.len());
    for refusal in &refusals {
        eprintln!("  {refusal}");
    }
    Ok(REFUSED)
}

/// Admit, seal and file.
///
/// `root` is `None` unless `--root` said otherwise, and then the family the
/// document declares decides the directory. A family this build has never heard
/// of still gets a directory of its own rather than the storage one.
fn do_archive(path: &Path, root: Option<&Path>) -> Result<u8, String> {
    let text = read_document(path)?;
    let root = match root {
        Some(root) => root.to_path_buf(),
        None => archive::dir_for_document(
            &integrity::parse_document(&text)
                .map_err(|err| format!("{}: {err}", path.display()))?,
        ),
    };
    match archive::archive_text(&text, &root) {
        Ok(entry) if entry.written => {
            println!(
                "archived {} as {} ({})",
                path.display(),
                entry.run_id,
                entry.document_digest
            );
            Ok(OK)
        }
        Ok(entry) => {
            println!(
                "{} is already archived as {}, and its digest still matches, so nothing \
                 changed",
                path.display(),
                entry.run_id
            );
            Ok(OK)
        }
        Err(ArchiveError::Refused(refusals)) => {
            eprintln!(
                "{}: refused for {} reasons, and nothing was written",
                path.display(),
                refusals.len()
            );
            for refusal in &refusals {
                eprintln!("  {refusal}");
            }
            Ok(REFUSED)
        }
        Err(other) => {
            eprintln!("{}: {other}", path.display());
            Ok(REFUSED)
        }
    }
}

/// Recompute the four digests and name the block that moved.
fn verify(path: &Path) -> Result<u8, String> {
    let text = read_document(path)?;
    let report = integrity::verify_text(&text).map_err(|err| err.to_string())?;
    for line in report.lines() {
        if report.ok() {
            println!("{line}");
        } else {
            eprintln!("{line}");
        }
    }
    if !report.unchanged.is_empty() && !report.ok() {
        eprintln!(
            "  the {} block(s) did not move, so this is a change to the numbers rather than \
             to the environment they were measured in",
            report.unchanged.join(" and ")
        );
    }
    Ok(if report.ok() { OK } else { REFUSED })
}

/// Print the provenance block this binary would stamp, and the warnings.
///
/// This is the mode to run before a sweep. Everything it warns about is also a
/// refusal, and finding out after forty minutes of measuring is the expensive
/// way to learn it.
fn provenance(scratch: &Path) -> Result<u8, String> {
    std::fs::create_dir_all(scratch)
        .map_err(|err| format!("{} could not be created: {err}", scratch.display()))?;
    let provenance = Provenance::capture_for_document(scratch);
    let warnings = provenance.document_provenance_warnings();
    let block = provenance.to_document_block(
        &json!({
            "argv": std::env::args().collect::<Vec<_>>(),
            "command": "provenance",
            "cwd": std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default(),
            "env": Value::Null,
            "resolved": Value::Null,
        }),
        false,
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&block).map_err(|err| err.to_string())?
    );
    for warning in &warnings {
        eprintln!("{warning}");
    }
    for warning in provenance.measurement_condition_warnings() {
        eprintln!("{warning}");
    }
    Ok(if warnings.is_empty() { OK } else { REFUSED })
}
