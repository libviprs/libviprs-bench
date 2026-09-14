//! The family concept: what each family measures, what it refuses, and what it
//! writes into its snapshot (issue #64).
//!
//! Every test here names, in a comment, the wrong implementation it goes red
//! against.

use libviprs_bench::family::{
    ALL_FAMILIES, DEFAULT_FAMILY, Family, FamilyRefusal, LIBVIPRS_ENGINES, VIPS_FEATURE,
};
use libviprs_bench::harness::Engine;
use libviprs_bench::provenance::Provenance;
use libviprs_bench::{
    BenchmarkSnapshot, CURRENT_SCHEMA_VERSION, LEGACY_SNAPSHOT_FAMILY, RunMetrics,
    create_snapshot, migrate_snapshot, push_snapshot,
};

// ---------------------------------------------------------------------------
// The engine set is a function of the family and of nothing else
// ---------------------------------------------------------------------------

/// RED against the implementation this replaces, which is still sitting in git
/// history: `let mut engines = vec![Monolithic, Streaming, MapReduce]; if
/// vips_available() { engines.push(Libvips) }`. On any machine with a `vips`
/// binary on PATH — the benchmark container, and every developer box that has
/// ever run the comparison — that produces a four-engine `engines` run, and the
/// same run on a machine without libvips produces three. Two runs of the same
/// family would then not be comparable, which is the entire reason to key on a
/// family rather than on the environment.
#[test]
fn the_engines_family_measures_exactly_the_three_libviprs_engines() {
    assert_eq!(
        Family::Engines.engines(),
        vec![Engine::Monolithic, Engine::Streaming, Engine::MapReduce],
        "the `engines` family is monolithic vs streaming vs mapreduce, in pipeline order"
    );
    assert!(
        !Family::Engines.engines().contains(&Engine::Libvips),
        "the `engines` family must never carry a libvips row, whatever is installed"
    );
    assert!(
        !Family::Engines.measures_libvips(),
        "`measures_libvips` is the single gate every libvips code path hangs off"
    );
}

/// RED against an `engines` arm written under `#[cfg(feature = "libvips")]`, or
/// against any `cfg` on the engine set at all. This test compiles into BOTH
/// feature cells of CI, so a family whose membership changes with the feature
/// fails in one of them: the default `check` cell or the `libvips` one.
///
/// This is the executable half of "no `engines` code path is reachable from a
/// `cfg(feature = "libvips")` item". The other half is the dependency graph,
/// which `the_engines_family_never_links_libvips` below covers.
#[test]
fn the_engines_family_is_the_same_set_with_and_without_the_libvips_feature() {
    assert_eq!(
        Family::Engines.engines(),
        LIBVIPRS_ENGINES.to_vec(),
        "the engine set the `engines` family measures must not depend on cargo features; \
         this assertion runs in both the default and the libvips CI cells and must give \
         the same answer in each"
    );
    assert_eq!(
        Family::Storage.engines(),
        LIBVIPRS_ENGINES.to_vec(),
        "so must the `storage` family's — it varies the sink, not the engine"
    );
}

/// RED against a manifest that lists `libvips-rs` as a plain (non-optional)
/// dependency, which would put the FFI bindings — and with them the pkg-config
/// probe for the libvips headers — into `cargo build` with no features. That is
/// the difference between "the default build needs no libvips" and "the default
/// build needs libvips installed but does not call it", and only the manifest
/// decides which one is true.
#[test]
fn the_engines_family_never_links_libvips() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read Cargo.toml");

    let deps = section(&manifest, "[dependencies]");
    let vips_line = deps
        .iter()
        .find(|line| line.starts_with("libvips-rs"))
        .expect("libvips-rs is still a dependency of this crate, behind its feature");
    assert!(
        vips_line.contains("optional = true"),
        "libvips-rs must be `optional = true` or the default build links the FFI: {vips_line}"
    );

    let features = section(&manifest, "[features]");
    let default_line = features
        .iter()
        .find(|line| line.starts_with("default"))
        .expect("[features] declares a `default` list");
    assert!(
        !default_line.contains("libvips") && !default_line.contains("dep:"),
        "the default feature set must stay empty of libvips: {default_line}"
    );
    let vips_feature = features
        .iter()
        .find(|line| line.starts_with(VIPS_FEATURE))
        .expect("[features] declares the libvips feature");
    assert!(
        vips_feature.contains("dep:libvips-rs"),
        "the `libvips` feature is what pulls the optional dep in: {vips_feature}"
    );
}

/// The lines of one Cargo.toml table, trimmed, comments and blanks dropped.
fn section(manifest: &str, header: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == header;
            continue;
        }
        if inside && !line.is_empty() && !line.starts_with('#') {
            out.push(line.to_string());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// RED against a silent empty run: `Family::parse("vips")` returning
/// `Some(Vips)` regardless of the build, which is what the old code did in
/// spirit — it asked `vips_available()` and, finding nothing, wrote a
/// comparison-shaped report with no comparison in it and exited 0.
#[cfg(not(feature = "libvips"))]
#[test]
fn the_vips_family_is_refused_without_its_feature() {
    let refusal = Family::resolve("vips").expect_err(
        "a default build has no libvips in it and must refuse the vips family, not run it empty",
    );
    assert!(
        matches!(refusal, FamilyRefusal::FeatureOff { .. }),
        "the refusal must say the feature is off, not that the name is unknown: {refusal:?}"
    );
    let message = refusal.to_string();
    assert!(
        message.contains(VIPS_FEATURE),
        "the refusal must name the feature that would fix it: {message}"
    );
    assert!(
        message.contains("--features libvips"),
        "and it must name the flag, not just the feature: {message}"
    );
    assert_ne!(
        refusal.exit_code(),
        0,
        "a refused run that exits 0 is the silent empty run this type exists to stop"
    );
}

/// The other side of the same fact: with the feature on, the family resolves.
/// RED against a refusal that is unconditional (which would make the comparison
/// unreachable in every build).
#[cfg(feature = "libvips")]
#[test]
fn the_vips_family_resolves_when_its_feature_is_on() {
    assert_eq!(Family::resolve("vips"), Ok(Family::Vips));
    assert!(Family::Vips.measures_libvips());
    assert!(Family::Vips.engines().contains(&Engine::Libvips));
}

/// RED against a `parse` that falls back to the default family on an unknown
/// name, which would silently measure `engines` when someone asked for a family
/// they misspelled and then file the result under the wrong label.
#[test]
fn an_unknown_family_is_refused_naming_the_known_ones() {
    let refusal = Family::resolve("libvips").expect_err("`libvips` is not a family name");
    assert!(matches!(refusal, FamilyRefusal::Unknown { .. }));
    let message = refusal.to_string();
    for family in ALL_FAMILIES {
        assert!(
            message.contains(family.as_str()),
            "the refusal must list every known family so the reader can pick one: {message}"
        );
    }
    assert_ne!(refusal.exit_code(), 0);
}

/// RED against a `storage` family that resolves to an engines-shaped run: that
/// would publish a `storage` snapshot whose numbers are the engines family's,
/// under a label nothing else would question.
///
/// The reason moved when K1.2's scenarios landed and the refusal did not. It
/// used to be "no scenarios yet"; it is now "a different binary measures
/// this", because `storage` has its own runner that builds with no cargo
/// features. The engine runners still refuse it, still loudly, and the message
/// still names the fix.
#[test]
fn the_storage_family_exists_and_is_refused_by_the_engine_runners() {
    assert_eq!(Family::parse("storage"), Some(Family::Storage));
    assert!(
        Family::Storage.is_implemented(),
        "the scenarios landed in K1.2"
    );
    let refusal = Family::resolve("storage").expect_err("report does not measure storage");
    assert!(matches!(refusal, FamilyRefusal::OtherRunner { .. }));
    assert!(
        refusal.to_string().contains("storage` binary"),
        "the refusal names the binary that does measure it: {refusal}"
    );
    assert_ne!(refusal.exit_code(), 0);

    // The control: the other two families are measured by these binaries, so
    // the refusal is about this family and not about `resolve` refusing
    // everything.
    assert_eq!(Family::resolve("engines"), Ok(Family::Engines));
    assert!(Family::Engines.measured_by_engine_runners());
    assert!(!Family::Storage.measured_by_engine_runners());
    assert_eq!(Family::Storage.runner_bin(), "storage");
}

/// RED against a default that is anything but the libviprs-only family — the
/// whole point of the lane is that running the harness with no arguments and no
/// features measures libviprs, not a comparison.
#[test]
fn the_default_family_is_libviprs_only_and_needs_no_features() {
    assert_eq!(DEFAULT_FAMILY, Family::Engines);
    assert_eq!(DEFAULT_FAMILY.required_feature(), None);
    assert!(DEFAULT_FAMILY.is_compiled_in());
    assert!(DEFAULT_FAMILY.is_implemented());
    assert_eq!(Family::resolve(DEFAULT_FAMILY.as_str()), Ok(Family::Engines));
}

// ---------------------------------------------------------------------------
// Snapshots name their family
// ---------------------------------------------------------------------------

fn sample_runs() -> Vec<RunMetrics> {
    vec![RunMetrics {
        label: "256x256_c0_mono".to_string(),
        width: 256,
        height: 256,
        engine: "monolithic".to_string(),
        measurement_path: String::new(),
        wall_time: std::time::Duration::from_millis(12),
        tracked_memory_bytes: 1024,
        peak_rss_bytes: 4096,
        stats: None,
        per_level_tiles: vec![1, 1],
        equivalence_psnr_db: None,
        tiles_produced: 2,
        levels_processed: 2,
        tiles_skipped: 0,
        strips: 0,
        batches: 0,
        inflight_strips: 0,
        concurrency: 0,
        memory_budget_bytes: 0,
    }]
}

fn snapshot_of(family: Family) -> BenchmarkSnapshot {
    create_snapshot(family, Provenance::capture(), sample_runs(), 256, 1_000_000)
}

/// RED against an unlabelled snapshot — a `BenchmarkSnapshot` with no `family`
/// field, or one left empty. That is what makes two families' history mergeable
/// by accident: `engines` measures three engines and `vips` measures four on the
/// identical sizes, so a history holding both draws a trend where libvips blinks
/// in and out between runs and the verdict divides by a row that is only
/// sometimes there.
#[test]
fn every_family_names_itself_in_its_snapshot() {
    for family in ALL_FAMILIES {
        let snapshot = snapshot_of(family);
        assert_eq!(
            snapshot.family,
            family.as_str(),
            "a {family} snapshot must say so"
        );
        assert!(
            !snapshot.family.trim().is_empty(),
            "an empty label is the same hole as no label at all"
        );
        assert_eq!(snapshot.schema_version, CURRENT_SCHEMA_VERSION);

        // And it must survive the round trip, since the label only earns its
        // keep when a file read back a month later still carries it.
        let json = serde_json::to_string(&snapshot).expect("serialize");
        let back: BenchmarkSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.family, family.as_str());
    }
}

/// RED against a `push`-and-hope history append. Two families in one file is
/// the accident the label exists to catch, and a label nothing checks is
/// decoration.
#[test]
fn a_history_refuses_a_snapshot_from_another_family() {
    let mut history = vec![snapshot_of(Family::Engines)];
    let err = push_snapshot(&mut history, snapshot_of(Family::Vips))
        .expect_err("a vips snapshot must not join an engines history");
    assert!(err.contains("engines") && err.contains("vips"), "{err}");
    assert_eq!(history.len(), 1, "the refused append must change nothing");

    push_snapshot(&mut history, snapshot_of(Family::Engines))
        .expect("its own family appends fine");
    assert_eq!(history.len(), 2);
}

/// RED against a migration that leaves pre-family history unlabelled. Every
/// snapshot written before issue #64 came out of the one benchmark this crate
/// had, the libvips comparison, so that is what those snapshots are — and an
/// unlabelled one would otherwise be waved into any family's history by the
/// guard above.
#[test]
fn pre_family_history_migrates_to_the_libvips_comparison() {
    let mut legacy = snapshot_of(Family::Vips);
    legacy.family = String::new();
    legacy.schema_version = 2;
    migrate_snapshot(&mut legacy);
    assert_eq!(legacy.family, LEGACY_SNAPSHOT_FAMILY);
    assert_eq!(legacy.family, Family::Vips.as_str());
    assert_eq!(legacy.schema_version, CURRENT_SCHEMA_VERSION);

    let mut history: Vec<BenchmarkSnapshot> = Vec::new();
    push_snapshot(&mut history, legacy).expect("a migrated snapshot is appendable");
    push_snapshot(&mut history, snapshot_of(Family::Engines))
        .expect_err("and it is a vips one, so an engines snapshot must not join it");
}

/// RED against a family whose artifacts share one directory with another's:
/// the JS chart renderer writes `chart_wall_time.svg` from whatever
/// `benchmark_results.json` it finds, so two families in one directory means
/// the second run silently overwrites the first one's charts.
#[test]
fn each_family_writes_into_its_own_report_directory() {
    let root = std::path::Path::new("/tmp/report-root");
    let dirs: Vec<_> = ALL_FAMILIES
        .iter()
        .map(|f| f.report_dir(root))
        .collect();
    for (i, a) in dirs.iter().enumerate() {
        assert!(a.starts_with(root), "{a:?} must live under report/");
        assert_eq!(a.file_name().unwrap(), ALL_FAMILIES[i].as_str());
        for b in dirs.iter().skip(i + 1) {
            assert_ne!(a, b, "two families must not share a report directory");
        }
    }
}
