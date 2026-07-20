//! Version-matrix runner tests (issues #19, #26, #27).
//!
//! The release-history axis of the benchmark used to be built by hand: check
//! out an old libviprs tag, `cargo run --bin report`, repeat, and hope every
//! run used the same pinned profile and landed in `benchmark_history.json`.
//! The version-matrix runner does that in one invocation — per tag it checks
//! `../libviprs` out into a throwaway git worktree, rebuilds the harness
//! against it, runs the identical suite, and appends one tagged,
//! environment-fingerprinted [`BenchmarkSnapshot`].
//!
//! These are behavioural tests in the style of `tests/history_migration.rs`:
//! they drive the real public API and keep fast by exercising the git
//! worktree + append machinery directly (the heavy per-tag `cargo` rebuild is
//! stubbed via [`HarnessBuild::Reuse`], and the full end-to-end orchestration
//! is behind `#[ignore]`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use libviprs_bench::version_id::{ordered_version_keys, version_key};
use libviprs_bench::version_matrix::{
    self, CoreWorktree, HarnessBuild, append_version_snapshot, build_harness, core_repo_dir,
};
use libviprs_bench::{BenchmarkSnapshot, CURRENT_SCHEMA_VERSION, RunMetrics, load_history};

/// A unique scratch path under the OS temp dir (no external crate), mirroring
/// the helper in `lib.rs`'s history tests.
fn scratch_path(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("libviprs_vmatrix_{tag}_{nanos}.json"))
}

/// Short git SHA of a ref in a repo, via the same `git` the driver shells out
/// to — the oracle the resolved worktree SHA is checked against.
fn git_short_sha(repo: &Path, refname: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--short", refname])
        .output()
        .expect("git rev-parse");
    assert!(out.status.success(), "git rev-parse {refname} failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A fabricated single-cell result, so the append-path tests never pay for a
/// real pyramid run.
fn synthetic_run(engine: &str, wall_ms: u64) -> RunMetrics {
    RunMetrics {
        label: format!("64x64_c0_{engine}"),
        width: 64,
        height: 64,
        engine: engine.to_string(),
        measurement_path: String::new(),
        wall_time: Duration::from_millis(wall_ms),
        tracked_memory_bytes: 1024 * 1024,
        peak_rss_bytes: 8 * 1024 * 1024,
        stats: None,
        per_level_tiles: vec![1],
        tiles_produced: 1,
        levels_processed: 1,
        tiles_skipped: 0,
        strips: 0,
        batches: 0,
        inflight_strips: 0,
        concurrency: 0,
        memory_budget_bytes: 0,
        equivalence_psnr_db: None,
    }
}

// ---------------------------------------------------------------------------
// Sub-issue #26: per-tag worktree checkout + rebuild driver
// ---------------------------------------------------------------------------

#[test]
fn checkout_resolves_head_sha_into_a_temp_worktree_and_cleans_up() {
    let repo = core_repo_dir();
    // Premise guard: the sibling core really is a git checkout.
    assert!(
        repo.join(".git").exists()
            || Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["rev-parse", "--git-dir"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false),
        "core repo {} must be a git checkout for the version matrix",
        repo.display()
    );
    let expected = git_short_sha(&repo, "HEAD");

    let wt = CoreWorktree::checkout(&repo, "HEAD").expect("checkout of HEAD must succeed");

    // The driver reports the ref it resolved and its concrete SHA.
    assert_eq!(wt.refname(), "HEAD");
    assert_eq!(
        wt.short_sha(),
        expected,
        "resolved worktree SHA must match `git rev-parse --short HEAD`"
    );
    // A real, populated worktree exists on disk with the core manifest.
    assert!(wt.path().exists(), "worktree dir must exist");
    assert!(
        wt.path().join("Cargo.toml").exists(),
        "worktree must contain the core Cargo.toml"
    );
    // The measured core version is read out of the checked-out manifest.
    assert!(!wt.version().is_empty(), "worktree version must resolve");
    assert_eq!(
        wt.version(),
        git_manifest_version(wt.path()),
        "resolved version must be the worktree manifest's [package] version"
    );

    let path = wt.path().to_path_buf();
    drop(wt); // Drop tears the worktree down deterministically.

    assert!(
        !path.exists(),
        "worktree dir {} must be removed on drop",
        path.display()
    );
    // git's worktree registry must no longer list it either (not just the dir).
    let listed = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["worktree", "list"])
        .output()
        .expect("git worktree list");
    let listed = String::from_utf8_lossy(&listed.stdout);
    assert!(
        !listed.contains(path.to_string_lossy().as_ref()),
        "removed worktree must be pruned from `git worktree list`"
    );
}

/// Read the `[package] version` straight out of a checked-out manifest, the
/// oracle for the version the driver resolves.
fn git_manifest_version(worktree: &Path) -> String {
    let text = std::fs::read_to_string(worktree.join("Cargo.toml")).unwrap();
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            if let Some(rest) = line.strip_prefix("version") {
                if let Some(rest) = rest.trim_start().strip_prefix('=') {
                    return rest.trim().trim_matches('"').to_string();
                }
            }
        }
    }
    panic!("no [package] version in worktree manifest");
}

#[test]
fn build_harness_reuse_fast_path_yields_a_runnable_exe() {
    // The heavy per-tag rebuild is stubbed: HarnessBuild::Reuse hands the
    // driver an already-built harness binary (here this test crate's own
    // `report` bin) instead of paying a full release rebuild.
    let repo = core_repo_dir();
    let wt = CoreWorktree::checkout(&repo, "HEAD").expect("checkout HEAD");
    let target = std::env::temp_dir().join(format!("vmatrix_target_{}", std::process::id()));

    let prebuilt = PathBuf::from(env!("CARGO_BIN_EXE_report"));
    let built = build_harness(
        &wt,
        &target,
        &HarnessBuild::ReuseUnchecked(prebuilt.clone()),
    )
    .expect("reuse build must succeed");

    assert_eq!(
        built.exe, prebuilt,
        "reuse mode must hand back the given exe"
    );
    assert!(built.exe.exists(), "the reused harness binary must exist");
}

#[test]
fn run_matrix_skips_build_failures_and_continues() {
    // A reuse path that does not exist forces build_harness into its failure
    // branch; run_matrix must record a typed Build skip and carry on rather than
    // panic, and must not create the history file (nothing was appended). This
    // exercises the skip-continues path without paying a real release rebuild.
    let history = scratch_path("skip_build");
    let _ = std::fs::remove_file(&history);

    let repo = core_repo_dir();
    let cfg = version_matrix::MatrixConfig {
        sizes: vec![(64, 64)],
        concurrency: vec![0],
        iters: 1,
        warmup: 0,
        build: HarnessBuild::ReuseUnchecked(PathBuf::from("/nonexistent/harness/binary")),
        ..Default::default()
    };

    let outcomes = version_matrix::run_matrix(&repo, &["HEAD".to_string()], &cfg, &history);
    assert_eq!(outcomes.len(), 1);
    match &outcomes[0] {
        version_matrix::VersionOutcome::Skipped { refname, error } => {
            assert_eq!(refname, "HEAD");
            assert!(
                matches!(error, version_matrix::MatrixError::Build(_)),
                "a missing reuse binary must be a Build skip, got {error:?}"
            );
        }
        other => panic!("expected a Build skip, got {other:?}"),
    }
    assert!(
        !history.exists(),
        "a fully-skipped sweep must not create the history file"
    );
    let _ = std::fs::remove_file(&history);
}

#[test]
fn build_failure_at_an_old_tag_is_reported_not_panicked() {
    // A ref that cannot be checked out (nonexistent) must surface a MatrixError
    // the runner can turn into skip+warn, never a panic that aborts the whole
    // matrix.
    let repo = core_repo_dir();
    let err = CoreWorktree::checkout(&repo, "definitely-not-a-real-ref-zzz");
    assert!(
        err.is_err(),
        "checking out a nonexistent ref must be an Err, got Ok"
    );
    // And it renders a message (Display) rather than being opaque.
    let msg = format!("{}", err.unwrap_err());
    assert!(!msg.is_empty(), "MatrixError must render a message");
}

// ---------------------------------------------------------------------------
// Sub-issue #27: --versions runner + append + version ordering / keying
// ---------------------------------------------------------------------------

#[test]
fn append_records_distinct_tagged_fingerprinted_snapshots() {
    let path = scratch_path("append");
    let _ = std::fs::remove_file(&path);

    // Two versions appended through the runner's append path, out of semver
    // order, with distinct SHAs.
    let n1 = append_version_snapshot(
        &path,
        "0.3.1",
        "aaaaaaa",
        vec![synthetic_run("monolithic", 10)],
        256,
        1_000_000,
    )
    .expect("first append");
    assert_eq!(n1, 1, "first append yields one history entry");

    let n2 = append_version_snapshot(
        &path,
        "0.2.0",
        "bbbbbbb",
        vec![synthetic_run("streaming", 20)],
        256,
        1_000_000,
    )
    .expect("second append");
    assert_eq!(n2, 2, "second append accretes, not overwrites");

    // load_history reads both back through the production path.
    let history = load_history(&path).expect("history must load");
    assert_eq!(history.len(), 2, "two distinct snapshots on record");

    // Each snapshot is tagged with its own measured version + SHA and carries a
    // captured environment fingerprint.
    assert_eq!(history[0].version, "0.3.1");
    assert_eq!(history[0].git_sha, "aaaaaaa");
    assert_eq!(history[1].version, "0.2.0");
    assert_eq!(history[1].git_sha, "bbbbbbb");
    for snap in &history {
        assert_eq!(snap.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(
            !snap.provenance.fingerprint().is_empty(),
            "every appended snapshot must be environment-fingerprinted"
        );
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn version_key_disambiguates_same_version_by_short_sha() {
    // Two builds of the same version at different commits must not collapse
    // into one column (issue #19): key them by version@short_sha.
    assert_eq!(version_key("0.3.1", "aaaaaaa"), "0.3.1@aaaaaaa");
    assert_ne!(
        version_key("0.3.1", "aaaaaaa"),
        version_key("0.3.1", "bbbbbbb"),
        "same version, different SHA must produce distinct keys"
    );
    // A missing / unknown SHA falls back to the bare version (legacy history).
    assert_eq!(version_key("0.3.1", ""), "0.3.1");
    assert_eq!(version_key("0.3.1", "unknown"), "0.3.1");
}

#[test]
fn ordered_version_keys_sorts_by_semver_and_timestamp_not_lexically() {
    // The discriminating case: lexicographically "0.10.0" < "0.3.1" < "0.9.0",
    // but by semver 0.3.1 < 0.9.0 < 0.10.0. Same-version snapshots are keyed by
    // SHA and ordered among themselves by timestamp.
    let history = vec![
        snap_with("0.10.0", "d4d4d4d", "2026-04-01T00:00:00Z"),
        snap_with("0.3.1", "c3c3c3c", "2026-02-01T00:00:00Z"),
        snap_with("0.3.1", "a1a1a1a", "2026-01-01T00:00:00Z"),
        snap_with("0.9.0", "b2b2b2b", "2026-03-01T00:00:00Z"),
    ];

    let keys = ordered_version_keys(&history);
    assert_eq!(
        keys,
        vec![
            "0.3.1@a1a1a1a", // older 0.3.1 first (timestamp)
            "0.3.1@c3c3c3c",
            "0.9.0@b2b2b2b",
            "0.10.0@d4d4d4d", // 0.10.0 last despite sorting first as a string
        ],
        "versions must order by (semver, timestamp), keyed by version@sha"
    );
}

/// Build a snapshot with a chosen version / SHA / timestamp for the ordering
/// test (bypassing `create_snapshot` so the timestamp is deterministic).
fn snap_with(version: &str, git_sha: &str, timestamp: &str) -> BenchmarkSnapshot {
    let mut snap = libviprs_bench::create_snapshot_for(
        version,
        git_sha,
        vec![synthetic_run("monolithic", 1)],
        256,
        1_000_000,
    );
    snap.timestamp = timestamp.to_string();
    snap
}

// ---------------------------------------------------------------------------
// End-to-end orchestration (heavier: real git worktrees + real suite runs).
// Ignored by default; run with `cargo test -- --ignored`.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "spawns real per-cell suite runs; run explicitly with --ignored"]
fn run_matrix_reuse_appends_one_snapshot_per_version() {
    let history = scratch_path("run_matrix");
    let _ = std::fs::remove_file(&history);

    let repo = core_repo_dir();
    // Tiny + single-shot so the ignored run is quick; reuse the prebuilt bin
    // instead of a per-tag rebuild.
    let cfg = version_matrix::MatrixConfig {
        sizes: vec![(64, 64)],
        concurrency: vec![0],
        iters: 1,
        warmup: 0,
        build: HarnessBuild::ReuseUnchecked(PathBuf::from(env!("CARGO_BIN_EXE_report"))),
        ..Default::default()
    };

    let outcomes = version_matrix::run_matrix(&repo, &["HEAD".to_string()], &cfg, &history);
    assert_eq!(outcomes.len(), 1);

    let hist = load_history(&history).expect("history loads");
    assert_eq!(
        hist.len(),
        1,
        "one appended snapshot for the single version"
    );
    assert!(!hist[0].version.is_empty(), "snapshot is version-tagged");
    assert_eq!(hist[0].git_sha, git_short_sha(&repo, "HEAD"));
    assert!(!hist[0].runs.is_empty(), "the suite produced runs");

    let _ = std::fs::remove_file(&history);
}

#[test]
#[ignore = "performs a real per-tag release rebuild; run explicitly with --ignored"]
fn run_matrix_rebuild_tags_snapshot_with_the_built_artifacts_identity() {
    // The *real* orchestration end-to-end: check out a ref, rebuild the harness
    // against it (paths override + BENCH_CORE_DIR), verify the built binary's
    // self-reported core matches the ref, run the suite, and append. Unlike the
    // Reuse tests (which assert a trivially-true HEAD==HEAD tautology while
    // build_harness's real arm never runs), this drives build_harness's Rebuild
    // path and asserts the appended identity is the built *artifact's* — the
    // check that catches a silently-ignored paths override or a broken stamp.
    let history = scratch_path("run_matrix_rebuild");
    let _ = std::fs::remove_file(&history);

    let repo = core_repo_dir();
    let expected_sha = git_short_sha(&repo, "HEAD");
    let expected_version = {
        let wt = CoreWorktree::checkout(&repo, "HEAD").expect("checkout HEAD");
        wt.version().to_string()
    };

    // Rebuild (not Reuse) against HEAD, tiny + single-shot to bound the run.
    let cfg = version_matrix::MatrixConfig {
        sizes: vec![(64, 64)],
        concurrency: vec![0],
        iters: 1,
        warmup: 0,
        build: HarnessBuild::Rebuild,
        ..Default::default()
    };

    let outcomes = version_matrix::run_matrix(&repo, &["HEAD".to_string()], &cfg, &history);
    assert_eq!(outcomes.len(), 1);
    match &outcomes[0] {
        version_matrix::VersionOutcome::Appended {
            version, short_sha, ..
        } => {
            assert_eq!(
                version, &expected_version,
                "snapshot tagged with built core version"
            );
            assert_eq!(
                short_sha, &expected_sha,
                "snapshot tagged with built core SHA"
            );
        }
        other => panic!("Rebuild against HEAD should append, got {other:?}"),
    }

    let hist = load_history(&history).expect("history loads");
    assert_eq!(hist.len(), 1);
    assert_eq!(hist[0].version, expected_version);
    assert_eq!(hist[0].git_sha, expected_sha);

    let _ = std::fs::remove_file(&history);
}
