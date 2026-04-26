//! Cached-vs-streaming `PdfiumStripSource` benchmark.
//!
//! Compares the two `PdfiumStripSource` constructors over a parametric
//! sweep of `(dpi, strip_count)` on a user-supplied PDF, producing
//! wall-time and peak-RSS numbers for each combination. The numbers
//! back the doc comments on `PdfiumStripSource::new_streaming` —
//! "vector-heavy PDFs scale ~linearly with N, raster-heavy approach
//! 1×" — with empirical data instead of assertion.
//!
//! # Usage
//!
//! ```sh
//! cargo run --release --bin pdfium_strip_source_bench -- \
//!     --pdf /path/to/blueprint.pdf \
//!     --page 1 \
//!     --dpis 72,150,300 \
//!     --strip-counts 4,16,64 \
//!     --output bench-strip-source.json
//! ```
//!
//! # Output
//!
//! Newline-delimited JSON: one `BenchRecord` per `(mode, dpi, strips)`
//! triple. Suitable for grep / jq / awk piping.

#![cfg(feature = "pdfium")]

use std::path::PathBuf;
use std::time::Instant;

use libviprs::PdfiumStripSource;
use libviprs::streaming::StripSource;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct BenchRecord {
    fixture: String,
    page: usize,
    mode: &'static str,
    dpi: u32,
    strips: u32,
    wall_time_ms: f64,
    /// Peak resident set size of the whole process at the end of the run,
    /// in bytes. NOT the per-source allocation — getrusage `ru_maxrss`
    /// is a high-water-mark across the entire process lifetime, so it
    /// only meaningfully diff'd between fresh subprocess invocations.
    peak_rss_bytes: u64,
    /// Raster width reported by the source.
    width: u32,
    /// Raster height reported by the source.
    height: u32,
}

#[cfg(target_os = "macos")]
fn peak_rss_bytes() -> u64 {
    use libc::{RUSAGE_SELF, getrusage, rusage};
    unsafe {
        let mut usage: rusage = std::mem::zeroed();
        if getrusage(RUSAGE_SELF, &mut usage) == 0 {
            // ru_maxrss is bytes on macOS, KB on Linux.
            usage.ru_maxrss as u64
        } else {
            0
        }
    }
}

#[cfg(target_os = "linux")]
fn peak_rss_bytes() -> u64 {
    use libc::{RUSAGE_SELF, getrusage, rusage};
    unsafe {
        let mut usage: rusage = std::mem::zeroed();
        if getrusage(RUSAGE_SELF, &mut usage) == 0 {
            (usage.ru_maxrss as u64) * 1024
        } else {
            0
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peak_rss_bytes() -> u64 {
    0
}

struct Args {
    pdf_path: PathBuf,
    page: usize,
    dpis: Vec<u32>,
    strip_counts: Vec<u32>,
    output: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut pdf_path: Option<PathBuf> = None;
    let mut page: usize = 1;
    let mut dpis: Vec<u32> = vec![72, 150];
    let mut strip_counts: Vec<u32> = vec![4, 16];
    let mut output: Option<PathBuf> = None;

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--pdf" => {
                pdf_path = Some(PathBuf::from(iter.next().unwrap_or_else(|| {
                    eprintln!("--pdf requires a path argument");
                    std::process::exit(2);
                })));
            }
            "--page" => {
                page = iter.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| {
                    eprintln!("--page requires a positive integer");
                    std::process::exit(2);
                });
            }
            "--dpis" => {
                dpis = parse_csv_u32(&iter.next().unwrap_or_default());
            }
            "--strip-counts" => {
                strip_counts = parse_csv_u32(&iter.next().unwrap_or_default());
            }
            "--output" => {
                output = iter.next().map(PathBuf::from);
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: pdfium_strip_source_bench --pdf <path> [--page N] \
                     [--dpis 72,150,300] [--strip-counts 4,16,64] [--output file.jsonl]"
                );
                std::process::exit(0);
            }
            _ => {
                eprintln!("unknown argument: {arg}");
                std::process::exit(2);
            }
        }
    }

    let pdf_path = pdf_path.unwrap_or_else(|| {
        eprintln!("--pdf is required");
        std::process::exit(2);
    });

    Args {
        pdf_path,
        page,
        dpis,
        strip_counts,
        output,
    }
}

fn parse_csv_u32(s: &str) -> Vec<u32> {
    s.split(',').filter_map(|t| t.trim().parse().ok()).collect()
}

fn main() {
    let args = parse_args();

    let fixture_label = args
        .pdf_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let mut writer: Box<dyn std::io::Write> = match &args.output {
        Some(path) => Box::new(std::fs::File::create(path).unwrap_or_else(|e| {
            eprintln!("failed to create {}: {e}", path.display());
            std::process::exit(1);
        })),
        None => Box::new(std::io::stdout()),
    };

    eprintln!(
        "fixture={} page={} dpis={:?} strip_counts={:?}",
        fixture_label, args.page, args.dpis, args.strip_counts
    );

    for &dpi in &args.dpis {
        for &strips in &args.strip_counts {
            for &mode in &["cached", "streaming"] {
                let record = bench_one(&args.pdf_path, args.page, dpi, strips, mode);
                let json = serde_json::to_string(&BenchRecord {
                    fixture: fixture_label.clone(),
                    ..record
                })
                .expect("serialize record");
                writeln!(writer, "{json}").expect("write");
                eprintln!(
                    "{:>9} dpi={:>3} strips={:>2} ⇒ {:>7.1} ms",
                    mode, dpi, strips, record.wall_time_ms,
                );
            }
        }
    }
}

/// Run one bench iteration. Returns a partial `BenchRecord` (the
/// caller fills in `fixture`).
fn bench_one(
    pdf_path: &std::path::Path,
    page: usize,
    dpi: u32,
    strips: u32,
    mode: &'static str,
) -> BenchRecord {
    let start = Instant::now();
    let source = match mode {
        "cached" => PdfiumStripSource::new(pdf_path, page, dpi),
        "streaming" => PdfiumStripSource::new_streaming(pdf_path, page, dpi),
        _ => unreachable!("mode is selected from the literal slice above"),
    }
    .unwrap_or_else(|e| {
        eprintln!("source construction failed for {mode}: {e:?}");
        std::process::exit(1);
    });

    let h_total = source.height();
    let strip_h = (h_total / strips).max(1);
    let mut y = 0u32;
    while y < h_total {
        let cur_h = strip_h.min(h_total - y);
        let _strip = source.render_strip(y, cur_h).unwrap_or_else(|e| {
            eprintln!("render_strip failed for {mode}: {e:?}");
            std::process::exit(1);
        });
        y += cur_h;
    }
    let wall_time_ms = start.elapsed().as_secs_f64() * 1000.0;

    BenchRecord {
        fixture: String::new(), // caller fills
        page,
        mode,
        dpi,
        strips,
        wall_time_ms,
        peak_rss_bytes: peak_rss_bytes(),
        width: source.width(),
        height: source.height(),
    }
}
