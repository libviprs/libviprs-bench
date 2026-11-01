//! #138: an `engines` run records the equivalence check, so its cell is
//! comparable and the charts get drawn.
//!
//! The family has no libvips, so there is nothing to score the engines against
//! but each other. Monolithic is the reference (it carries no score against
//! itself, like libvips in the four-engine run) and streaming and mapreduce each
//! carry a PSNR against its pyramid. Red today: every row has
//! `equivalence_psnr_db: null`, the contract holds the cell back, and
//! `chart_wall_time.svg` is never written.

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

const REPORT: &str = env!("CARGO_BIN_EXE_report");

fn engines_rows(label: &str) -> Vec<Value> {
    let dir: PathBuf = std::env::temp_dir()
        .join("libviprs-bench-138")
        .join(format!("{}_{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    let out = Command::new(REPORT)
        .args([
            "--family",
            "engines",
            "--report-dir",
            dir.to_str().unwrap(),
            "--sizes",
            "512x384",
            "--concurrency",
            "0",
            "--iters",
            "1",
            "--warmup",
            "0",
        ])
        .output()
        .expect("run the report binary");
    assert!(
        out.status.success(),
        "the engines family must run: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(dir.join("benchmark_results.json")).expect("results");
    let rows: Vec<Value> = serde_json::from_str(&text).expect("results are JSON");
    let _ = std::fs::remove_dir_all(&dir);
    rows
}

fn psnr_of<'a>(rows: &'a [Value], engine: &str) -> &'a Value {
    &rows
        .iter()
        .find(|r| r["engine"] == engine)
        .unwrap_or_else(|| panic!("no {engine} row"))["equivalence_psnr_db"]
}

#[test]
fn streaming_and_mapreduce_record_a_psnr_against_the_monolithic_pyramid() {
    let rows = engines_rows("scored");
    for engine in ["streaming", "mapreduce"] {
        let psnr = psnr_of(&rows, engine).as_f64().unwrap_or_else(|| {
            panic!("{engine} must carry equivalence_psnr_db, got {}", psnr_of(&rows, engine))
        });
        assert!(
            psnr >= libviprs_bench::harness::MIN_TILE_PSNR_DB,
            "{engine} scored {psnr} dB against monolithic"
        );
    }
}

#[test]
fn monolithic_is_the_reference_and_carries_no_score_against_itself() {
    let rows = engines_rows("reference");
    assert!(
        psnr_of(&rows, "monolithic").is_null(),
        "a self-comparison proves nothing and must not be recorded"
    );
}
