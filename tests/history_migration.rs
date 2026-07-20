//! Regression test for the benchmark-history schema migration (issue #154).
//!
//! Before the schema-migration rework, deserializing the committed
//! `report/benchmark_history.json` ERRORED: the file was written under the
//! pre-#153 schema (field `peak_memory_bytes`, no RSS column, no git SHA,
//! space-separated run labels), while `RunMetrics` required
//! `tracked_memory_bytes` and `peak_rss_bytes`. A single un-parseable
//! history file broke the entire version-history / cross_version pipeline.
//!
//! `tests/fixtures/benchmark_history_legacy.json` is a verbatim copy of
//! that committed history. This test loads it through the real
//! `load_history` path and asserts the migration produces sane values.

use std::path::Path;

use libviprs_bench::{BenchmarkSnapshot, CURRENT_SCHEMA_VERSION, load_history};

const LEGACY: &str = include_str!("fixtures/benchmark_history_legacy.json");

#[test]
fn legacy_history_json_lacks_the_current_field_names() {
    // Guard the premise: the committed history really is written in the old
    // schema (this is what used to make the strict struct fail to parse).
    assert!(
        LEGACY.contains("peak_memory_bytes"),
        "fixture should use the legacy `peak_memory_bytes` field name"
    );
    assert!(
        !LEGACY.contains("tracked_memory_bytes"),
        "fixture predates the `tracked_memory_bytes` rename"
    );
    assert!(
        LEGACY.contains("1024x1024 c0 monolithic"),
        "fixture should carry legacy space-separated run labels"
    );
}

#[test]
fn legacy_history_deserializes_verbatim() {
    // Raw serde: with the alias/default annotations this now parses where
    // it previously errored with "missing field tracked_memory_bytes".
    let parsed: Result<Vec<BenchmarkSnapshot>, _> = serde_json::from_str(LEGACY);
    assert!(
        parsed.is_ok(),
        "legacy history must deserialize verbatim, got {:?}",
        parsed.err()
    );
    let snaps = parsed.unwrap();
    assert_eq!(snaps.len(), 2, "fixture has two snapshots");
}

#[test]
fn load_history_migrates_legacy_file() {
    // Write the verbatim fixture to a scratch path and load it through the
    // production `load_history` (which applies `migrate_snapshot`).
    let dir = std::env::temp_dir().join(format!("libviprs_hist_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("benchmark_history.json");
    std::fs::write(&path, LEGACY).unwrap();

    let history = load_history(&path).expect("legacy history must load, not error");
    assert_eq!(history.len(), 2);

    let snap = &history[0];
    // Schema version bumped to current on load.
    assert_eq!(snap.schema_version, CURRENT_SCHEMA_VERSION);
    // Missing git SHA / provenance default cleanly.
    assert_eq!(snap.git_sha, "");
    assert_eq!(snap.provenance.libvips_version, "unknown");

    let run = &snap.runs[0];
    // Labels normalized from "1024x1024 c0 monolithic" to underscore form so
    // the `starts_with("{w}x{h}_c{c}")` filters in the history pipeline work.
    assert!(
        !run.label.contains(' '),
        "label should be normalized, got {:?}",
        run.label
    );
    assert_eq!(run.label, "1024x1024_c0_monolithic");
    // Old `peak_memory_bytes` maps onto `tracked_memory_bytes` via the alias.
    assert_eq!(run.tracked_memory_bytes, 4_194_304);
    // No RSS column in the legacy file → "unknown" (0), not a parse error.
    assert_eq!(run.peak_rss_bytes, 0);
    // No stats in legacy history.
    assert!(run.stats.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn history_chart_grouping_fields_survive_migration() {
    // The history-trend pipeline (tools/charts/render.mjs `configsOf` /
    // `historyPointsFor`) buckets runs by the STRUCTURED width/height/
    // concurrency fields, not by the run label. The concrete regression this
    // guards: those numeric fields must survive the legacy-history migration
    // so the JS grouping key still resolves (the label is no longer read by
    // any chart code).
    let dir = std::env::temp_dir().join(format!("libviprs_hist2_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("benchmark_history.json");
    std::fs::write(&path, LEGACY).unwrap();
    let history = load_history(&path).unwrap();

    // The 1024x1024 c0 config render.mjs groups on must be present via the
    // numeric fields (width == 1024 && height == 1024 && concurrency == 0).
    let matched = history
        .iter()
        .flat_map(|s| &s.runs)
        .filter(|r| r.width == 1024 && r.height == 1024 && r.concurrency == 0)
        .count();
    assert!(
        matched > 0,
        "migrated runs must expose the numeric width/height/concurrency the JS pipeline groups by"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = Path::new(&path);
}
