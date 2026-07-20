//! Version-matrix runner: build + benchmark the harness against a series of
//! libviprs releases in one invocation (issues #19, #26, #27).
//!
//! The release-history axis of `benchmark_history.json` used to be built by
//! hand — check out an old tag, `cargo run --bin report`, repeat — which made
//! it easy to drift the pinned profile or the host environment between
//! versions. This module drives it deterministically:
//!
//!   * [`CoreWorktree::checkout`] materialises the sibling `../libviprs` at a
//!     given tag/SHA in a throwaway git worktree, removed on `Drop`.
//!   * [`build_harness`] rebuilds *this* harness against that worktree with
//!     the identical pinned `[profile.release]`, redirecting the `libviprs`
//!     path dependency through a Cargo `paths` override (and pointing
//!     `build.rs` at the worktree via `BENCH_CORE_DIR` so the built binary
//!     self-reports the measured version).
//!   * [`run_matrix`] ties it together: per version it checks out, builds,
//!     runs the identical isolated suite, and appends one tagged,
//!     environment-fingerprinted [`BenchmarkSnapshot`] via
//!     [`append_version_snapshot`], reusing the existing provenance +
//!     append-to-history path. A build failure at an old tag is a skip+warn,
//!     never an abort of the whole matrix.
//!
//! Version identity is keyed by `version@short_sha` ([`version_key`]) and
//! ordered by (semver, timestamp) ([`ordered_version_keys`]), so two builds of
//! the same version don't collapse into one column and `0.10.0` sorts after
//! `0.9.0` rather than lexically before `0.3.1` (issue #19).

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::harness::{self, Engine};
use crate::{BenchmarkSnapshot, RunMetrics};

/// The harness binary rebuilt per tag and re-invoked (as `--single` children)
/// to measure each cell. It is the same `report` bin the everyday benchmark
/// uses.
const HARNESS_BIN: &str = "report";

/// Anything that can go wrong resolving, building, or recording one version.
///
/// Every variant is recoverable at the matrix level: [`run_matrix`] turns it
/// into a skip+warn for that version and carries on, so one un-buildable old
/// tag never sinks the whole release-history run.
#[derive(Debug)]
pub enum MatrixError {
    /// A `git` invocation failed (bad ref, worktree add/remove, rev-parse).
    Git(String),
    /// The per-tag `cargo build` failed or produced no harness binary.
    Build(String),
    /// The history file could not be loaded (e.g. corrupt) — refused rather
    /// than clobbered, mirroring [`crate::load_history`].
    History(String),
}

impl fmt::Display for MatrixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MatrixError::Git(m) => write!(f, "git error: {m}"),
            MatrixError::Build(m) => write!(f, "build error: {m}"),
            MatrixError::History(m) => write!(f, "history error: {m}"),
        }
    }
}

impl std::error::Error for MatrixError {}

/// Absolute path to the measured core crate — the `libviprs = { path =
/// "../libviprs" }` dependency, resolved relative to this crate's manifest.
/// This is the git repository the per-tag worktrees are cut from.
pub fn core_repo_dir() -> PathBuf {
    let joined = Path::new(env!("CARGO_MANIFEST_DIR")).join("../libviprs");
    std::fs::canonicalize(&joined).unwrap_or(joined)
}

/// A throwaway git worktree of the core repo checked out at one ref.
///
/// Created by [`CoreWorktree::checkout`] and torn down on `Drop` (or via
/// [`CoreWorktree::remove`]) so a matrix run leaves no worktrees behind, even
/// on panic.
#[derive(Debug)]
pub struct CoreWorktree {
    /// The worktree directory (a full checkout of the core crate at `refname`).
    pub path: PathBuf,
    /// The ref requested (`"HEAD"`, `"v0.3.1"`, a SHA, …).
    pub refname: String,
    /// The concrete short SHA the ref resolved to.
    pub short_sha: String,
    /// The `[package] version` read out of the checked-out core manifest, or
    /// `"unknown"` if it could not be parsed.
    pub version: String,
    /// The source core repo, retained so `Drop` can `git worktree remove`.
    repo: PathBuf,
    removed: bool,
}

impl CoreWorktree {
    /// Check `repo` out at `refname` into a fresh temp worktree and resolve its
    /// short SHA + measured version.
    ///
    /// Deterministic and self-cleaning: the worktree lands under a unique
    /// per-process temp path and is removed when the returned value drops. A
    /// bad ref (or any `git worktree add` failure — e.g. a checkout that can't
    /// be materialised) is a [`MatrixError::Git`], not a panic.
    pub fn checkout(repo: &Path, refname: &str) -> Result<CoreWorktree, MatrixError> {
        let repo = repo.to_path_buf();
        let base = std::env::temp_dir().join("libviprs-bench-vmatrix");
        std::fs::create_dir_all(&base)
            .map_err(|e| MatrixError::Git(format!("mkdir {}: {e}", base.display())))?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = base.join(format!(
            "{}-{}-{}",
            sanitize_ref(refname),
            std::process::id(),
            nanos
        ));

        // `--detach`: we only want the tree at that ref, never a branch. Let
        // git create the leaf directory so it owns the worktree registration.
        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["worktree", "add", "--detach"])
            .arg(&path)
            .arg(refname)
            .output()
            .map_err(|e| MatrixError::Git(format!("failed to spawn git worktree add: {e}")))?;
        if !out.status.success() {
            return Err(MatrixError::Git(format!(
                "git worktree add {refname} in {} failed: {}",
                repo.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }

        let short_sha = git_stdout(&path, &["rev-parse", "--short", "HEAD"])?;
        let version = read_package_version(&path.join("Cargo.toml"));

        Ok(CoreWorktree {
            path,
            refname: refname.to_string(),
            short_sha,
            version,
            repo,
            removed: false,
        })
    }

    /// Remove the worktree and prune it from the repo's registry. Idempotent.
    pub fn remove(&mut self) -> Result<(), MatrixError> {
        if self.removed {
            return Ok(());
        }
        self.removed = true;
        let status = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .output();
        // Backstop: if `git worktree remove` could not (e.g. the dir was moved),
        // delete the directory ourselves so nothing is left on disk...
        if self.path.exists() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
        // ...then prune the now-dangling registry entry either way.
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(["worktree", "prune"])
            .output();
        match status {
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => Err(MatrixError::Git(format!(
                "git worktree remove failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ))),
            Err(e) => Err(MatrixError::Git(format!(
                "failed to spawn git worktree remove: {e}"
            ))),
        }
    }
}

impl Drop for CoreWorktree {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

/// How [`build_harness`] obtains the per-tag harness binary.
#[derive(Debug, Clone)]
pub enum HarnessBuild {
    /// Rebuild the harness from source against the worktree — the real path,
    /// pinning `[profile.release]` and redirecting the `libviprs` dependency.
    Rebuild,
    /// Skip the rebuild and reuse an already-built harness binary. Used by
    /// tests and a fast local smoke, where the heavy release rebuild is not
    /// what is under test.
    Reuse(PathBuf),
}

/// A harness binary ready to drive the isolated suite for one version.
#[derive(Debug, Clone)]
pub struct BuiltHarness {
    /// The `report` binary re-invoked (as `--single` children) to measure each
    /// cell.
    pub exe: PathBuf,
}

/// Build (or reuse) the harness against `worktree`, into `target_dir`.
///
/// The real path runs `cargo build --release --bin report` with two
/// redirections so the numbers describe the worktree's core, not the current
/// checkout:
///
///   * `--config paths=[<worktree>]` overrides the `libviprs` path dependency
///     with the worktree copy (a `paths` override does no version resolution,
///     so an arbitrary old tag slots in regardless of its version), and
///   * `BENCH_CORE_DIR=<worktree>` points `build.rs` at the same tree so the
///     built binary self-reports the measured version/SHA.
///
/// The pinned `[profile.release]` (lto/codegen-units) is inherited from this
/// crate's `Cargo.toml`, and `RUSTFLAGS` from the ambient environment
/// (`run-bench.sh` exports the measurement pin), so the profile is identical to
/// the everyday benchmark. A non-zero `cargo` exit — the common "an old tag no
/// longer builds against today's deps" case — is a [`MatrixError::Build`].
pub fn build_harness(
    worktree: &CoreWorktree,
    target_dir: &Path,
    mode: &HarnessBuild,
) -> Result<BuiltHarness, MatrixError> {
    if let HarnessBuild::Reuse(exe) = mode {
        if !exe.exists() {
            return Err(MatrixError::Build(format!(
                "reuse harness {} does not exist",
                exe.display()
            )));
        }
        return Ok(BuiltHarness { exe: exe.clone() });
    }

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let worktree_abs = std::fs::canonicalize(&worktree.path).unwrap_or(worktree.path.clone());
    let exe_path = target_dir.join("release").join(HARNESS_BIN);

    // Never let a stale binary from a previous version masquerade as this
    // build's output: drop it first and require the build to recreate it.
    let _ = std::fs::remove_file(&exe_path);

    let paths_override = format!("paths=[\"{}\"]", worktree_abs.display());
    let status = Command::new("cargo")
        .current_dir(manifest_dir)
        .args(["build", "--release", "--bin", HARNESS_BIN])
        .arg("--target-dir")
        .arg(target_dir)
        .arg("--config")
        .arg(&paths_override)
        .env("BENCH_CORE_DIR", &worktree_abs)
        .status()
        .map_err(|e| MatrixError::Build(format!("failed to spawn cargo build: {e}")))?;
    if !status.success() {
        return Err(MatrixError::Build(format!(
            "cargo build against {} ({}) failed",
            worktree.refname, worktree.short_sha
        )));
    }
    if !exe_path.exists() {
        return Err(MatrixError::Build(format!(
            "harness binary {} missing after build",
            exe_path.display()
        )));
    }
    Ok(BuiltHarness { exe: exe_path })
}

/// Append one tagged, environment-fingerprinted snapshot to the history file.
///
/// Reuses the guarded [`crate::load_history`] / [`crate::save_history`] path:
/// a corrupt existing history is surfaced as [`MatrixError::History`] and left
/// untouched rather than silently overwritten. `version` / `git_sha` are the
/// *measured* core's identity (resolved from the tag's worktree), and the
/// environment fingerprint is captured live. Returns the new history length.
pub fn append_version_snapshot(
    history_path: &Path,
    version: &str,
    git_sha: &str,
    runs: Vec<RunMetrics>,
    tile_size: u32,
    memory_budget_bytes: u64,
) -> Result<usize, MatrixError> {
    let mut history = crate::load_history(history_path).map_err(MatrixError::History)?;
    let snapshot =
        crate::create_snapshot_for(version, git_sha, runs, tile_size, memory_budget_bytes);
    history.push(snapshot);
    crate::save_history(history_path, &history);
    Ok(history.len())
}

/// Version identity key: `version@short_sha`, so two builds of the same
/// version at different commits stay distinct (issue #19).
///
/// Falls back to the bare version when the SHA is empty or `"unknown"` (legacy
/// history predating the SHA field), so those snapshots still group sanely.
pub fn version_key(version: &str, git_sha: &str) -> String {
    if git_sha.is_empty() || git_sha == "unknown" {
        version.to_string()
    } else {
        format!("{version}@{git_sha}")
    }
}

/// The version keys of `history`, deduplicated and ordered by (semver,
/// timestamp) — the ordering `cross_version` presents releases in.
///
/// Semver ordering (not lexicographic) means `0.9.0` precedes `0.10.0`; ties on
/// version are broken by the RFC 3339 timestamp (chronological), and any
/// unparseable version sorts last but deterministically.
pub fn ordered_version_keys(history: &[BenchmarkSnapshot]) -> Vec<String> {
    let mut items: Vec<(String, (u64, u64, u64), String)> = history
        .iter()
        .map(|s| {
            (
                version_key(&s.version, &s.git_sha),
                semver_sort_key(&s.version),
                s.timestamp.clone(),
            )
        })
        .collect();
    // (semver, timestamp, key) — the trailing key makes the order total and
    // stable across equal (semver, timestamp) pairs.
    items.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.cmp(&b.2)).then(a.0.cmp(&b.0)));

    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter_map(|(key, _, _)| seen.insert(key.clone()).then_some(key))
        .collect()
}

/// Parse a `MAJOR.MINOR.PATCH` version (tolerating a leading `v` and a
/// `-pre`/`+build` suffix) into a numerically sortable tuple. Anything that
/// isn't three integer components sorts last via an all-`MAX` key.
fn semver_sort_key(version: &str) -> (u64, u64, u64) {
    let core = version
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()
        .unwrap_or("");
    let mut it = core.split('.');
    let next = |it: &mut std::str::Split<'_, char>| it.next().and_then(|s| s.parse::<u64>().ok());
    match (next(&mut it), next(&mut it), next(&mut it)) {
        (Some(a), Some(b), Some(c)) => (a, b, c),
        _ => (u64::MAX, u64::MAX, u64::MAX),
    }
}

/// Configuration for a [`run_matrix`] sweep. [`Default`] mirrors the everyday
/// `report` benchmark (same sizes, concurrency levels, tile size, budget, and
/// iteration counts) so the release-history axis measures the identical suite.
#[derive(Debug, Clone)]
pub struct MatrixConfig {
    pub sizes: Vec<(u32, u32)>,
    pub concurrency: Vec<usize>,
    pub tile_size: u32,
    pub memory_budget_bytes: u64,
    pub iters: u32,
    pub warmup: u32,
    /// How each per-tag harness is obtained (rebuild vs reuse).
    pub build: HarnessBuild,
    /// Where per-tag builds land; `None` uses a shared temp dir removed at the
    /// end of the run.
    pub target_dir: Option<PathBuf>,
}

impl Default for MatrixConfig {
    fn default() -> Self {
        MatrixConfig {
            sizes: vec![(512, 512), (1024, 1024), (2048, 2048), (4096, 4096)],
            concurrency: vec![0, 4],
            tile_size: 256,
            memory_budget_bytes: 1_000_000,
            iters: harness::DEFAULT_ITERS,
            warmup: harness::DEFAULT_WARMUP,
            build: HarnessBuild::Rebuild,
            target_dir: None,
        }
    }
}

/// What became of one version in a matrix sweep.
#[derive(Debug, Clone)]
pub enum VersionOutcome {
    /// The version built, ran, and appended a snapshot; `entries` is the
    /// resulting history length.
    Appended {
        refname: String,
        version: String,
        short_sha: String,
        entries: usize,
    },
    /// The version was skipped (checkout/build/append failed); `reason` is the
    /// rendered [`MatrixError`].
    Skipped { refname: String, reason: String },
}

/// Run the identical isolated suite against each ref in `refs` and append one
/// tagged, fingerprinted snapshot per version to `history_path`.
///
/// This is the `--versions <tag,tag,HEAD>` driver: it produces the whole
/// release-history axis in one invocation instead of manual accretion. Per
/// version it checks the core repo out at the ref, (re)builds the harness
/// against it, runs `cfg`'s suite, and appends. A checkout/build/append failure
/// for one version is logged and recorded as [`VersionOutcome::Skipped`]; the
/// sweep continues.
pub fn run_matrix(
    repo: &Path,
    refs: &[String],
    cfg: &MatrixConfig,
    history_path: &Path,
) -> Vec<VersionOutcome> {
    // Per-tag builds share one target dir so dependency compilation is reused
    // across versions (only the core crate + harness relink per tag). A dir we
    // synthesised is cleaned up at the end; a caller-supplied one is left be.
    let (target_dir, owned) = match &cfg.target_dir {
        Some(dir) => (dir.clone(), false),
        None => (
            std::env::temp_dir()
                .join("libviprs-bench-vmatrix-target")
                .join(std::process::id().to_string()),
            true,
        ),
    };

    let mut outcomes = Vec::with_capacity(refs.len());
    for refname in refs {
        match run_one_version(repo, refname, cfg, &target_dir, history_path) {
            Ok((version, short_sha, entries)) => {
                eprintln!(
                    "version-matrix: appended {} ({}) — {entries} snapshot(s) on record",
                    version_key(&version, &short_sha),
                    refname
                );
                outcomes.push(VersionOutcome::Appended {
                    refname: refname.clone(),
                    version,
                    short_sha,
                    entries,
                });
            }
            Err(e) => {
                eprintln!("version-matrix: WARNING skipping {refname}: {e}");
                outcomes.push(VersionOutcome::Skipped {
                    refname: refname.clone(),
                    reason: e.to_string(),
                });
            }
        }
    }

    if owned && matches!(cfg.build, HarnessBuild::Rebuild) {
        let _ = std::fs::remove_dir_all(&target_dir);
    }
    outcomes
}

/// Check out, build, run, and append for a single version. The worktree is a
/// local — dropped (and thus removed) as this returns, success or failure.
fn run_one_version(
    repo: &Path,
    refname: &str,
    cfg: &MatrixConfig,
    target_dir: &Path,
    history_path: &Path,
) -> Result<(String, String, usize), MatrixError> {
    let worktree = CoreWorktree::checkout(repo, refname)?;
    let built = build_harness(&worktree, target_dir, &cfg.build)?;

    let engines = engine_set();
    let runs = harness::run_isolated_suite(
        &built.exe,
        &cfg.sizes,
        &cfg.concurrency,
        &engines,
        cfg.tile_size,
        cfg.memory_budget_bytes,
        cfg.iters,
        cfg.warmup,
    );

    let entries = append_version_snapshot(
        history_path,
        &worktree.version,
        &worktree.short_sha,
        runs,
        cfg.tile_size,
        cfg.memory_budget_bytes,
    )?;
    Ok((
        worktree.version.clone(),
        worktree.short_sha.clone(),
        entries,
    ))
}

/// The engine set the matrix measures: the three libviprs engines always, plus
/// libvips when the system binary is present — matching the `report` bin.
fn engine_set() -> Vec<Engine> {
    let mut engines = vec![Engine::Monolithic, Engine::Streaming, Engine::MapReduce];
    if crate::vips_available() {
        engines.push(Engine::Libvips);
    }
    engines
}

/// Map an arbitrary ref to a filesystem-safe worktree basename component.
fn sanitize_ref(refname: &str) -> String {
    let cleaned: String = refname
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "ref".to_string()
    } else {
        cleaned
    }
}

/// Read the `[package] version` from a Cargo manifest with a small hand scan
/// (no TOML dependency), mirroring `build.rs`. `"unknown"` if absent.
fn read_package_version(manifest: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(manifest) else {
        return "unknown".to_string();
    };
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
    "unknown".to_string()
}

/// Run a `git -C repo <args>` command, returning trimmed stdout or a
/// [`MatrixError::Git`].
fn git_stdout(repo: &Path, args: &[&str]) -> Result<String, MatrixError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| MatrixError::Git(format!("failed to spawn git {args:?}: {e}")))?;
    if !out.status.success() {
        return Err(MatrixError::Git(format!(
            "git {args:?} in {} failed: {}",
            repo.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_key_orders_numerically_not_lexically() {
        assert!(semver_sort_key("0.9.0") < semver_sort_key("0.10.0"));
        assert!(semver_sort_key("0.3.1") < semver_sort_key("0.9.0"));
        // Leading v and pre-release suffix are tolerated.
        assert_eq!(semver_sort_key("v0.3.1"), (0, 3, 1));
        assert_eq!(semver_sort_key("0.4.0-rc.1"), (0, 4, 0));
        // Unparseable sorts last.
        assert_eq!(semver_sort_key("nightly"), (u64::MAX, u64::MAX, u64::MAX));
        assert!(semver_sort_key("9.9.9") < semver_sort_key("nightly"));
    }

    #[test]
    fn version_key_uses_sha_when_known() {
        assert_eq!(version_key("0.3.1", "abc1234"), "0.3.1@abc1234");
        assert_eq!(version_key("0.3.1", ""), "0.3.1");
        assert_eq!(version_key("0.3.1", "unknown"), "0.3.1");
    }

    #[test]
    fn sanitize_ref_is_filesystem_safe() {
        assert_eq!(sanitize_ref("v0.3.1"), "v0.3.1");
        assert_eq!(sanitize_ref("feature/foo"), "feature_foo");
        assert_eq!(sanitize_ref(""), "ref");
    }
}
