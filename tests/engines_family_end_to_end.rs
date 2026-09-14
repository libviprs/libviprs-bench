//! A build with no cargo features must produce a complete `engines` run, with
//! charts, and it must refuse the `vips` family rather than run it empty
//! (issue #64).
//!
//! These drive the real `report` binary as a child process, so what they prove
//! is the shipped artifact's behaviour and not a library call that happens to
//! sit next to it. `CARGO_BIN_EXE_report` is the binary cargo just built for
//! this test run, in whatever feature cell CI is in.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// The report binary this test run built.
const REPORT: &str = env!("CARGO_BIN_EXE_report");

/// The six grouped-bar comparison charts `tools/charts/render.mjs` draws.
const CHARTS: &[&str] = &[
    "chart_wall_time.svg",
    "chart_peak_memory.svg",
    "chart_tracked_memory.svg",
    "chart_throughput.svg",
    "chart_efficiency.svg",
    "chart_resource_cost.svg",
];

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("libviprs-bench-k11")
        .join(format!("{}_{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("the run must have written {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

/// A fast but complete run: the smallest size that still yields a real
/// multi-level pyramid, one concurrency level, one timed iteration. Everything
/// the shape of the output depends on is exercised; only the sample count is
/// small, and this lane measures nothing.
fn run_family(family: &str, dir: &Path) -> std::process::Output {
    Command::new(REPORT)
        .args([
            "--family",
            family,
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
        .expect("run the report binary")
}

/// RED today: before this lane the `report` binary had no family at all, built
/// its engine list from `vips_available()`, wrote every family into one
/// `report/` directory, and stamped its snapshot with no family. On a machine
/// with libvips installed the "default" run silently became a four-engine
/// comparison; on one without, the same command measured something else. There
/// was no argument that could ask for the libviprs-only run and no snapshot
/// field that recorded which run you got.
///
/// Also RED against a run that writes its JSON but no charts: the chart
/// renderer is the only producer of the SVGs, and a family whose JSON it cannot
/// draw is not a complete run.
#[test]
fn a_default_build_runs_the_engines_family_end_to_end() {
    let dir = scratch("engines");
    let out = run_family("engines", &dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "the engines family must run to completion with no features:\n--- stdout ---\n{stdout}\n\
         --- stderr ---\n{stderr}"
    );

    // 1. Every artifact the run promises.
    for name in [
        "benchmark_results.json",
        "benchmark_history.json",
        "comparison_table.txt",
        "verdict_table.txt",
    ] {
        assert!(
            dir.join(name).is_file(),
            "the run must write {name} into its family directory"
        );
    }

    // 2. All three libviprs engines, and no libvips row — whatever is installed
    //    on the machine running this test.
    let results = read_json(&dir.join("benchmark_results.json"));
    let rows = results.as_array().expect("benchmark_results.json is an array");
    let engines: Vec<&str> = rows
        .iter()
        .filter_map(|r| r["engine"].as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(
        engines,
        vec!["mapreduce", "monolithic", "streaming"],
        "an `engines` run is exactly the three libviprs engines"
    );
    for row in rows {
        assert!(
            row["tiles_produced"].as_u64().unwrap_or(0) > 0,
            "every row must have produced tiles, or the run is complete in name only: {row}"
        );
    }

    // 3. The snapshot names its family, so this history can never be appended
    //    to another family's.
    let history = read_json(&dir.join("benchmark_history.json"));
    let snapshots = history.as_array().expect("benchmark_history.json is an array");
    assert_eq!(snapshots.len(), 1, "one run, one snapshot");
    assert_eq!(
        snapshots[0]["family"].as_str(),
        Some("engines"),
        "the snapshot must name the family it came out of"
    );
    let snapshot_engines: std::collections::BTreeSet<&str> = snapshots[0]["runs"]
        .as_array()
        .expect("the snapshot carries its runs")
        .iter()
        .filter_map(|r| r["engine"].as_str())
        .collect();
    assert!(
        !snapshot_engines.contains("libvips"),
        "no libvips row may reach an engines snapshot"
    );

    // 4. Charts on disk. render.mjs is the only thing that draws them, so this
    //    runs it exactly as run-bench.sh does.
    render_charts(&dir);
    for name in CHARTS {
        let svg = dir.join(name);
        assert!(svg.is_file(), "{name} must be rendered for an engines run");
        let text = std::fs::read_to_string(&svg).expect("read the chart");
        assert!(text.starts_with("<svg"), "{name} must be an SVG document");
        for engine in ["Monolithic", "Streaming", "MapReduce"] {
            assert!(
                text.contains(engine),
                "{name} must carry the {engine} series"
            );
        }
        assert!(
            !text.contains(">libvips<"),
            "{name} must carry no libvips legend entry for an engines run"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// RED against a silent empty run: the old binary had no way to ask for the
/// comparison, so there was nothing to refuse — it simply measured whatever
/// libvips it could find, or none, and exited 0 either way. A default build
/// must now say no, name the feature, and die non-zero.
#[cfg(not(feature = "libvips"))]
#[test]
fn the_vips_family_is_refused_by_the_report_binary_without_its_feature() {
    let dir = scratch("vips-refused");
    let out = run_family("vips", &dir);
    assert!(
        !out.status.success(),
        "asking a libvips-free build for the vips family must fail"
    );
    assert_eq!(out.status.code(), Some(2), "and with a deliberate exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("libvips") && stderr.contains("--features libvips"),
        "the refusal must name the feature and the flag: {stderr}"
    );
    assert!(
        !dir.join("benchmark_results.json").exists(),
        "a refused run must write nothing at all"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// RED against a `storage` family that quietly measures the engines family's
/// cells before K1.2 has written its own.
#[test]
fn the_storage_family_is_refused_by_the_report_binary_until_its_scenarios_land() {
    let dir = scratch("storage-refused");
    let out = run_family("storage", &dir);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("K1.2"), "{stderr}");
    assert!(!dir.join("benchmark_results.json").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Render the SVGs from whatever JSON is in `dir`, the same way `run-bench.sh`
/// does. Node is a hard requirement of this suite rather than a conditional
/// skip: a chart assertion that quietly does not run is the same colour as one
/// that passed, and the whole claim under test is that the default family
/// produces charts.
fn render_charts(dir: &Path) {
    let renderer = Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/charts/render.mjs");
    let out = Command::new("node")
        .arg(&renderer)
        .args(["--report-dir", dir.to_str().unwrap()])
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "this suite needs `node` on PATH to prove the charts are drawn (GitHub's \
                 ubuntu runners and the bench container both carry it): {e}"
            )
        });
    assert!(
        out.status.success(),
        "render.mjs failed on an engines run:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
