//! The PMTiles writer sweep's contract (libviprs-bench #132).
//!
//! The epic that built the bounded-memory writer (libviprs#1135) listed two
//! benchmarks as a deliverable and closed without either: the regression
//! against the previous revision, and the comparison against go-pmtiles. These
//! checks are the contract those two have to meet, and they were written before
//! the sweep so that the first thing the PR shows is them failing.
//!
//! Three groups.
//!
//! * The shape of the sweep: the matrix the issue names (two layouts, with and
//!   without ordered emission, three dedupe budgets), the cells, the scenarios.
//! * A real run of the binary at the `ci` profile, put through the same
//!   aggregator that archives a capture. A document nothing can admit is not
//!   evidence, and the fixture is the producer's own output rather than a
//!   hand-written approximation of it.
//! * The archive. A sweep that ran and left no sealed document in
//!   `archive/storage/` is the exact state this issue was opened over, so the
//!   evidence has to be in the tree.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use libviprs_bench::storage::archive;
use libviprs_bench::storage::cells::Profile;
use libviprs_bench::storage::writer_sweep::{self, GO_PMTILES, SCENARIOS, Variant};
use serde_json::Value;

// ---------------------------------------------------------------------------
// The shape of the sweep
// ---------------------------------------------------------------------------

/// The matrix the issue asks for, and only when the measured core can express
/// it. On a revision that predates the bounded writer there is one writer to
/// measure, so asking for twelve there would be asking for a lie.
#[test]
fn the_matrix_is_two_layouts_by_two_emissions_by_three_budgets() {
    let labels: Vec<String> = writer_sweep::matrix().iter().map(Variant::label).collect();
    if cfg!(core_writer_knobs) {
        let expected: BTreeSet<String> = ["tile-id", "arrival", "tile-id+ordered", "arrival+ordered"]
            .iter()
            .flat_map(|mode| {
                ["256KiB", "1MiB", "8MiB"]
                    .iter()
                    .map(move |budget| format!("{mode}@{budget}"))
            })
            .collect();
        assert_eq!(
            labels.iter().cloned().collect::<BTreeSet<_>>(),
            expected,
            "the matrix is {labels:?}"
        );
        assert_eq!(labels.len(), 12, "no variant is listed twice: {labels:?}");
    } else {
        assert_eq!(
            labels,
            ["tile-id@unbounded"],
            "a core with no dedupe budget has one writer, and it says its table is unbounded"
        );
    }
}

#[test]
fn a_variant_label_round_trips_and_names_one_variant() {
    let all = writer_sweep::matrix();
    assert!(!all.is_empty(), "there is nothing to round-trip");
    let mut seen = BTreeSet::new();
    for variant in all {
        let label = variant.label();
        assert!(seen.insert(label.clone()), "{label} names two variants");
        assert_eq!(
            Variant::parse(&label),
            Some(variant),
            "{label} does not parse back to the variant that wrote it"
        );
    }
    assert_eq!(Variant::parse("not-a-writer"), None);
}

/// The default writer is a variant of its own. It is what every caller who
/// asked for nothing gets, so it is the row the regression is read off.
#[test]
fn the_default_writer_is_in_the_matrix() {
    let labels: Vec<String> = writer_sweep::matrix().iter().map(Variant::label).collect();
    let default = if cfg!(core_writer_knobs) {
        "tile-id@8MiB"
    } else {
        "tile-id@unbounded"
    };
    assert!(labels.iter().any(|l| l == default), "{labels:?} has no {default}");
}

#[test]
fn the_profiles_walk_the_cells_they_say_they_do() {
    assert_eq!(
        writer_sweep::sweep_cells(Profile::Ci),
        ["2048x2048@256+gradient", "synthetic@z8"]
    );
    // The three corpus cells the storage family already publishes, so the
    // writer is compared on the same bytes, then the three synthetic cells the
    // bounded-memory claim needs: a real pyramid has too few distinct payloads
    // for a table that grows with them to show. z11 is the one that matters most.
    // The writer's sort buffer holds a million records, so at z9 and z10 it is
    // still filling and heap grows with tiles whatever the dedupe table does;
    // only at four million tiles is the buffer full, and flat means flat.
    assert_eq!(
        writer_sweep::sweep_cells(Profile::Full),
        [
            "8192x8192@256+gradient",
            "8192x8192@64+gradient",
            "8192x8192@64+noise",
            "synthetic@z9",
            "synthetic@z10",
            "synthetic@z11",
        ]
    );
}

#[test]
fn the_sweep_publishes_the_four_scenarios_the_issue_names() {
    assert_eq!(
        SCENARIOS,
        &["write_ingest", "write_finalize", "write_output", "build_from_tiles"][..]
    );
}

// ---------------------------------------------------------------------------
// A real run, through the aggregator
// ---------------------------------------------------------------------------

fn producer_document() -> &'static Value {
    static DOCUMENT: OnceLock<Value> = OnceLock::new();
    DOCUMENT.get_or_init(|| {
        let exe = env!("CARGO_BIN_EXE_pmtiles-writer");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("writer-{}-{nonce}.json", std::process::id()));
        let run = Command::new(exe)
            // This proves the sweep's shape, not its timings, so it does not
            // wait for a quiet machine: on a shared runner that wait is the
            // whole test.
            .env("WRITER_QUIET_WAIT_S", "0")
            .args(["--profile", "ci", "--out"])
            .arg(&out)
            .output()
            .expect("the pmtiles-writer binary runs");
        assert!(
            run.status.success(),
            "the sweep exited {:?}\n--- stderr ---\n{}",
            run.status.code(),
            String::from_utf8_lossy(&run.stderr)
        );
        let text = std::fs::read_to_string(&out)
            .unwrap_or_else(|e| panic!("the sweep wrote {}: {e}", out.display()));
        serde_json::from_str(&text).expect("the document is JSON")
    })
}

fn rows(doc: &Value) -> &Vec<Value> {
    doc["cells"].as_array().expect("the document has cells")
}

fn field<'a>(row: &'a Value, name: &str) -> &'a str {
    row[name].as_str().unwrap_or("")
}

#[test]
fn the_document_is_a_storage_document_the_aggregator_admits() {
    let doc = producer_document();
    assert_eq!(doc["family"], "libviprs-storage");
    let refusals = archive::admit(doc);
    assert!(
        refusals.is_empty(),
        "the aggregator refuses the writer sweep's own document:\n{}",
        refusals
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Every variant has a row for every scenario on the real cell, and the
/// synthetic cell has the one scenario that does not need an engine.
#[test]
fn every_variant_answers_every_scenario() {
    let doc = producer_document();
    let mut seen: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    for row in rows(doc) {
        *seen
            .entry((
                field(row, "backend").to_string(),
                field(row, "cell").to_string(),
                field(row, "scenario").to_string(),
            ))
            .or_default() += 1;
    }
    for variant in writer_sweep::matrix() {
        let label = variant.label();
        for scenario in SCENARIOS {
            assert!(
                seen.contains_key(&(
                    label.clone(),
                    "2048x2048@256+gradient".to_string(),
                    scenario.to_string()
                )),
                "{label} has no {scenario} row on the real cell"
            );
        }
        assert!(
            seen.contains_key(&(
                label.clone(),
                "synthetic@z8".to_string(),
                "build_from_tiles".to_string()
            )),
            "{label} has no build_from_tiles row on the synthetic cell"
        );
    }
}

/// The issue's three quantities, each as a series per repetition rather than a
/// single number: ingest, finalize and peak live heap.
#[test]
fn ingest_finalize_and_peak_live_heap_are_published_per_repetition() {
    let doc = producer_document();
    let reps = doc["measurement"]["reps"]["write"].as_u64().unwrap() as usize;
    let want = [
        ("write_ingest", "wall"),
        ("write_ingest", "heap_peak"),
        ("write_finalize", "wall"),
        ("write_finalize", "heap_peak"),
        ("build_from_tiles", "wall"),
        ("build_from_tiles", "heap_peak"),
    ];
    for variant in writer_sweep::matrix() {
        for (scenario, metric) in want {
            let row = rows(doc)
                .iter()
                .find(|r| {
                    field(r, "backend") == variant.label()
                        && field(r, "scenario") == scenario
                        && field(r, "metric") == metric
                        && field(r, "cell") == "2048x2048@256+gradient"
                })
                .unwrap_or_else(|| panic!("{} has no {scenario}.{metric} row", variant.label()));
            assert_eq!(field(row, "outcome"), "ok", "{row}");
            assert_eq!(
                row["samples"].as_array().map(Vec::len),
                Some(reps),
                "{} {scenario}.{metric} is not one sample per repetition",
                variant.label()
            );
        }
    }
}

/// Peak RSS has to be on the same basis for every contender, because it is the
/// number go-pmtiles is compared on.
#[test]
fn peak_rss_is_published_for_the_writer_and_for_go_pmtiles() {
    let doc = producer_document();
    for backend in writer_sweep::matrix()
        .iter()
        .map(Variant::label)
        .chain([GO_PMTILES.to_string()])
    {
        let row = rows(doc)
            .iter()
            .find(|r| {
                field(r, "backend") == backend
                    && field(r, "scenario") == "build_from_tiles"
                    && field(r, "metric") == "peak_rss"
                    && field(r, "cell") == "2048x2048@256+gradient"
            })
            .unwrap_or_else(|| panic!("{backend} has no build_from_tiles.peak_rss row"));
        // go-pmtiles is not installed in the cheap check job. A row that says
        // so and why is the honest answer there; what is not allowed is a
        // missing row, or an ok row with no samples.
        match field(row, "outcome") {
            "ok" => assert!(row["median"].as_f64().is_some_and(|m| m > 0.0), "{row}"),
            "skipped" => assert!(
                !field(row, "reason").is_empty(),
                "{backend} is skipped with no reason"
            ),
            other => panic!("{backend} peak_rss is {other}: {row}"),
        }
    }
}

#[test]
fn an_ok_row_is_attested_against_the_tile_set_it_was_built_from() {
    let doc = producer_document();
    let mut ok = 0;
    for row in rows(doc) {
        if field(row, "outcome") == "ok" {
            ok += 1;
            assert_eq!(
                row["attested"],
                Value::Bool(true),
                "{} {} is ok and not attested",
                field(row, "backend"),
                field(row, "key")
            );
        }
    }
    assert!(ok > 0, "no row is ok, so the sweep measured nothing");
}

// ---------------------------------------------------------------------------
// The archive
// ---------------------------------------------------------------------------

fn archived_writer_documents() -> Vec<(PathBuf, Value)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("archive/storage");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("archive/storage exists") {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.ends_with(".json") || name == "index.json" {
            continue;
        }
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        if rows(&doc)
            .iter()
            .any(|r| field(r, "scenario") == "build_from_tiles")
        {
            out.push((path, doc));
        }
    }
    out
}

fn commit(doc: &Value) -> &str {
    doc["provenance"]["library"]["commit"].as_str().unwrap_or("")
}

/// The previous-revision half of the deliverable. No release carries the
/// pre-epic writer (v0.4.0 has no PMTiles at all and v0.5.0 already has the
/// whole epic), so the baseline is a pinned revision, and the document says so
/// by carrying its commit.
#[test]
fn the_archive_holds_a_run_of_the_writer_before_the_epic() {
    let docs = archived_writer_documents();
    let baseline: Vec<_> = docs
        .iter()
        .filter(|(_, d)| commit(d).starts_with("150774a5"))
        .collect();
    assert!(
        !baseline.is_empty(),
        "no archived writer run was measured at 150774a5, the last revision before the bounded \
         writer; found {:?}",
        docs.iter().map(|(_, d)| commit(d)).collect::<Vec<_>>()
    );
}

#[test]
fn the_archive_holds_a_run_of_the_current_writer_across_the_whole_matrix() {
    let docs = archived_writer_documents();
    let current: Vec<_> = docs
        .iter()
        .filter(|(_, d)| !commit(d).starts_with("150774a5"))
        .collect();
    assert!(!current.is_empty(), "no archived writer run of a current revision");
    let want: BTreeSet<String> = ["tile-id", "arrival", "tile-id+ordered", "arrival+ordered"]
        .iter()
        .flat_map(|m| ["256KiB", "1MiB", "8MiB"].iter().map(move |b| format!("{m}@{b}")))
        .collect();
    let covered = current.iter().any(|(_, d)| {
        let have: BTreeSet<String> = rows(d)
            .iter()
            .filter(|r| field(r, "scenario") == "write_ingest" && field(r, "outcome") == "ok")
            .map(|r| field(r, "backend").to_string())
            .collect();
        want.is_subset(&have)
    });
    assert!(covered, "no archived current run has an ok write_ingest row for every variant");
}

#[test]
fn the_archive_holds_a_go_pmtiles_build_of_the_same_tile_set() {
    let docs = archived_writer_documents();
    let measured = docs.iter().any(|(_, d)| {
        rows(d).iter().any(|r| {
            field(r, "backend") == GO_PMTILES
                && field(r, "scenario") == "build_from_tiles"
                && field(r, "metric") == "wall"
                && field(r, "outcome") == "ok"
        })
    });
    assert!(measured, "no archived run has an ok go-pmtiles wall row");
}

/// What the issue says out loud: native x86_64, inside a throw-away container.
#[test]
fn every_archived_writer_run_is_native_x86_64_at_a_publishable_profile() {
    let docs = archived_writer_documents();
    assert!(!docs.is_empty(), "no archived writer run to check");
    for (path, doc) in docs {
        let p = &doc["provenance"];
        assert_eq!(p["arch"], "x86_64", "{}", path.display());
        assert_eq!(p["emulated"], Value::Bool(false), "{}", path.display());
        assert_eq!(p["inContainer"], Value::Bool(true), "{}", path.display());
        assert!(
            matches!(doc["profile"].as_str(), Some("full" | "xl")),
            "{} is profile {}",
            path.display(),
            doc["profile"]
        );
    }
}
