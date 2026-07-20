//! Rust↔JS chart field-shape drift guards (#42 / #44).
//!
//! Two concerns:
//!   * #42 — the JS chart migration is FINISHED, so the Rust plotters
//!     `generate_charts` is gone and `plotters` is no longer referenced
//!     anywhere under `src/`. A grep-style walk asserts it, so a stray
//!     re-introduction (or an un-dropped dependency) fails here rather than
//!     silently keeping the heavy dep alive.
//!   * #44 — the committed golden fixtures under `tools/charts/fixtures/` that
//!     `render.mjs` consumes mirror the EXACT serde shape these serializers
//!     emit. Comparing the serialized STRUCTURE (field names + nesting, values
//!     ignored) against the golden means renaming a Rust field without updating
//!     the golden (and `render.mjs`) fails here — the PRODUCER half of the
//!     drift guard whose CONSUMER half lives in `tools/charts/golden.test.mjs`.
//!     (ScalabilityPoint is a `scalability`-binary-private type; its producer
//!     half is asserted by that binary's own unit test.)

use std::path::{Path, PathBuf};
use std::time::Duration;

use libviprs_bench::{BenchmarkSnapshot, RunMetrics, RunStats, create_snapshot};
use serde_json::Value;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Recursively reduce a JSON value to its SHAPE: an object keeps its (sorted)
/// keys mapped to the shape of each value; an array collapses to the shape of
/// its first element (`Null` when empty); every scalar becomes `Null`. Two
/// values with an equal shape carry identical field names at every nesting
/// level, regardless of the concrete numbers/strings.
fn shape(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), shape(&map[k])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(vec![items.first().map(shape).unwrap_or(Value::Null)]),
        _ => Value::Null,
    }
}

fn read_golden(name: &str) -> Value {
    let path = crate_root().join("tools/charts/fixtures").join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read golden {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse golden {}: {e}", path.display()))
}

/// A fully-populated `RunMetrics` — every `Option` is `Some`, every `Vec`
/// non-empty — so the serialized shape exercises the nested `Duration` +
/// `RunStats` objects the golden must mirror.
fn sample_run_metrics() -> RunMetrics {
    RunMetrics {
        label: "2048x2048_c0_monolithic".to_string(),
        width: 2048,
        height: 2048,
        engine: "monolithic".to_string(),
        measurement_path: String::new(),
        wall_time: Duration::new(0, 85_000_000),
        tracked_memory_bytes: 16_777_216,
        peak_rss_bytes: 209_715_200,
        stats: Some(RunStats {
            n: 7,
            wall_ms_median: 85.0,
            wall_ms_min: 82.0,
            wall_ms_iqr: 3.5,
            wall_ms_ci95: 1.8,
            rss_mb_median: 200.0,
            rss_mb_min: 198.0,
            rss_mb_iqr: 2.0,
            rss_mb_ci95: 1.1,
        }),
        per_level_tiles: vec![64, 16, 4, 1],
        equivalence_psnr_db: Some(48.5),
        tiles_produced: 85,
        levels_processed: 4,
        tiles_skipped: 0,
        strips: 0,
        batches: 0,
        inflight_strips: 0,
        concurrency: 0,
        memory_budget_bytes: 0,
    }
}

/// Depth-first walk of `dir`, invoking `f(path, contents)` for every `*.rs`.
fn walk_rs(dir: &Path, f: &mut impl FnMut(&Path, &str)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rs(&path, f);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                f(&path, &text);
            }
        }
    }
}

#[test]
fn plotters_is_fully_removed_from_src() {
    // The JS migration is finished (#42): no `src/*.rs` may reference plotters.
    let src = crate_root().join("src");
    let mut offenders = Vec::new();
    walk_rs(&src, &mut |path, text| {
        if text.contains("plotters") {
            offenders.push(path.to_path_buf());
        }
    });
    assert!(
        offenders.is_empty(),
        "plotters must be fully removed from src/ once the JS chart migration is finished; \
         found references in {offenders:?}"
    );
}

#[test]
fn golden_run_metrics_matches_the_serializer_shape() {
    let serialized = serde_json::to_value(sample_run_metrics()).unwrap();
    let golden = read_golden("golden_results.json");
    let first = golden
        .as_array()
        .and_then(|a| a.first())
        .expect("golden_results.json is a non-empty array");
    assert_eq!(
        shape(&serialized),
        shape(first),
        "golden_results.json[0] must mirror the RunMetrics serde shape (field names + nested \
         Duration/RunStats). If this fails a RunMetrics field changed — update \
         tools/charts/fixtures/*.json AND tools/charts/render.mjs to match."
    );
}

#[test]
fn golden_snapshot_matches_the_serializer_shape() {
    // `create_snapshot` stamps a live-captured Provenance, so this also pins the
    // Provenance + HostInfo + nested-runs shape the history golden must carry.
    let snapshot: BenchmarkSnapshot = create_snapshot(vec![sample_run_metrics()], 256, 4_000_000);
    let serialized = serde_json::to_value(&snapshot).unwrap();
    let golden = read_golden("golden_history.json");
    let first = golden
        .as_array()
        .and_then(|a| a.first())
        .expect("golden_history.json is a non-empty array");
    assert_eq!(
        shape(&serialized),
        shape(first),
        "golden_history.json[0] must mirror the BenchmarkSnapshot serde shape (incl. Provenance \
         and the nested runs). If this fails a BenchmarkSnapshot/RunMetrics/Provenance field \
         changed — update the goldens AND tools/charts/render.mjs to match."
    );
}
