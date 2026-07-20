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
//! Version identity (keying by `version@short_sha`, ordering by (semver,
//! timestamp)) is pure domain logic and lives in [`crate::version_id`]; this
//! module is the process-orchestration half.
//!
//! Measurement note: unlike the everyday axis (measured inside the pinned
//! Docker image), the release-history axis is driven on the **host** toolchain —
//! it needs the core repo's real git worktree topology, which the container does
//! not carry. Its snapshots are therefore a self-contained series, only
//! comparable within themselves on one host; the environment fingerprint each
//! snapshot records (and `cross_version`'s `env≠` guard) keeps them from being
//! silently compared against Docker-measured numbers.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::RunMetrics;
use crate::harness::{self, Engine};
use crate::version_id::version_key;

/// The harness binary rebuilt per tag and re-invoked (as `--single` children)
/// to measure each cell. It is the same `report` bin the everyday benchmark
/// uses.
const HARNESS_BIN: &str = "report";

/// Anything that can go wrong resolving, building, or recording one version.
///
/// [`Git`](MatrixError::Git) and [`Build`](MatrixError::Build) are *per-version*
/// failures: [`run_matrix`] turns them into a skip+warn for that version and
/// carries on, so one un-buildable old tag never sinks the whole run.
///
/// [`History`](MatrixError::History) is different — it signals the shared
/// history file itself is unusable (corrupt/unreadable), a whole-run
/// precondition. The runner pre-flights the load once up front and aborts
/// before doing any expensive work rather than rebuilding + benchmarking every
/// ref only to fail identically at each append. A `History` still surfacing
/// mid-sweep (e.g. a same-directory temp write that fails) degrades to a
/// per-version skip so already-recorded snapshots survive.
#[derive(Debug, Clone)]
pub enum MatrixError {
    /// A `git` invocation failed (bad ref, worktree add/remove, rev-parse).
    Git(String),
    /// The per-tag `cargo build` failed, produced no harness binary, or built a
    /// binary whose self-reported core did not match the requested ref.
    Build(String),
    /// The history file could not be loaded or persisted — refused rather than
    /// clobbered, mirroring [`crate::load_history`] / [`crate::save_history`].
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
    /// Private: `Drop` calls `remove_dir_all(&self.path)`, so letting a holder
    /// reassign it would orphan the real worktree and point cleanup at the wrong
    /// directory. Read it through [`CoreWorktree::path`].
    path: PathBuf,
    /// The ref requested (`"HEAD"`, `"v0.3.1"`, a SHA, …).
    refname: String,
    /// The concrete short SHA the ref resolved to.
    short_sha: String,
    /// The `[package] version` read out of the checked-out core manifest, or
    /// `"unknown"` if it could not be parsed.
    version: String,
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

        // Arm cleanup *before* the fallible resolves below. Past this point the
        // worktree exists on disk and in git's registry, so construct the RAII
        // guard first with a placeholder identity and fill it in afterwards: if
        // `rev-parse` or the manifest read fails, `wt` drops on the `?` and its
        // `Drop` removes + prunes the worktree instead of leaking it.
        let mut wt = CoreWorktree {
            path,
            refname: refname.to_string(),
            short_sha: String::new(),
            version: String::new(),
            repo,
            removed: false,
        };
        wt.short_sha = git_stdout(&wt.path, &["rev-parse", "--short", "HEAD"])?;
        wt.version = read_package_version(&wt.path.join("Cargo.toml"));
        Ok(wt)
    }

    /// The worktree directory — a full checkout of the core crate at its ref.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The ref requested (`"HEAD"`, `"v0.3.1"`, a SHA, …).
    pub fn refname(&self) -> &str {
        &self.refname
    }

    /// The concrete short SHA the ref resolved to.
    pub fn short_sha(&self) -> &str {
        &self.short_sha
    }

    /// The `[package] version` read out of the checked-out core manifest, or
    /// `"unknown"` if it could not be parsed.
    pub fn version(&self) -> &str {
        &self.version
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
        // Success is defined by the end state, not by which step achieved it: if
        // the directory is gone the worktree is cleaned up even when the initial
        // `git worktree remove` returned non-zero (the backstop finished the job
        // and prune dropped the registry entry). Only a directory we could not
        // delete is a genuine error worth surfacing.
        if !self.path.exists() {
            return Ok(());
        }
        match status {
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
    /// After the build, the binary's `build.rs`-stamped self-report is asserted
    /// to match the requested ref, so a snapshot's identity comes from the
    /// artifact rather than a side channel (see [`build_harness`]).
    Rebuild,
    /// Skip the per-ref rebuild and reuse an already-built harness binary.
    ///
    /// **Unchecked identity.** The supplied binary is *not* rebuilt against the
    /// checked-out ref, so its measurements reflect whatever core it was
    /// originally linked against while the appended snapshot is still tagged
    /// with the requested ref's version/SHA. That is only sound when the two
    /// coincide (e.g. reusing a `report` built against `HEAD` to measure
    /// `HEAD`). The everyday runner always uses
    /// [`Rebuild`](HarnessBuild::Rebuild); this exists for tests and a fast
    /// local smoke where the heavy release rebuild is not what is under test. In
    /// debug builds [`build_harness`] `debug_assert!`s the reused binary's
    /// self-report matches the ref, to catch accidental misuse.
    ReuseUnchecked(PathBuf),
}

/// The package name of the measured core crate. A Cargo `paths` override only
/// applies to a crate whose name matches a graph dependency, so the worktree's
/// manifest is checked against this before a build is spent on it.
const CORE_CRATE_NAME: &str = "libviprs";

/// A harness binary ready to drive the isolated suite for one version.
///
/// Under a shared `target_dir` (the default) the `exe` path is a fixed location
/// that the *next* [`build_harness`] overwrites, so a handle must be consumed
/// before the next per-tag build. The strictly-serial [`run_matrix`] loop
/// upholds this: it builds, runs, and appends one version fully before the next.
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
    if let HarnessBuild::ReuseUnchecked(exe) = mode {
        if !exe.exists() {
            return Err(MatrixError::Build(format!(
                "reuse harness {} does not exist",
                exe.display()
            )));
        }
        // Best-effort guard against accidental misuse (see `ReuseUnchecked`): in
        // debug builds, fail loudly if the reused binary was linked against a
        // different core than the ref it is being made to represent.
        #[cfg(debug_assertions)]
        if let Ok((reported_version, _)) = harness_core_identity(exe) {
            debug_assert_eq!(
                reported_version,
                worktree.version(),
                "reused harness self-reports core {reported_version} but ref {} is core {}",
                worktree.refname(),
                worktree.version(),
            );
        }
        return Ok(BuiltHarness { exe: exe.clone() });
    }

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let worktree_abs =
        std::fs::canonicalize(worktree.path()).unwrap_or_else(|_| worktree.path().to_path_buf());

    // Cheap pre-flight: a Cargo `paths` override is *silently ignored* when the
    // overriding crate's package name does not match a dependency in the graph —
    // which would leave us measuring the current sibling core under an old tag's
    // identity. Confirm the worktree really is the `libviprs` crate before we
    // spend a full release build on it.
    let worktree_name = read_package_name(&worktree_abs.join("Cargo.toml"));
    if worktree_name.as_deref() != Some(CORE_CRATE_NAME) {
        return Err(MatrixError::Build(format!(
            "worktree at {} is package {:?}, expected `{CORE_CRATE_NAME}` — a `paths` override \
             would be silently ignored",
            worktree.refname(),
            worktree_name.as_deref().unwrap_or("<unreadable>"),
        )));
    }

    let exe_path = target_dir.join("release").join(HARNESS_BIN);

    // Never let a stale binary from a previous version masquerade as this
    // build's output: drop it first and require the build to recreate it.
    let _ = std::fs::remove_file(&exe_path);

    // The path is embedded in a TOML basic string; escape it so a temp/canonical
    // path containing a `"` or `\` (or a Windows path) can't produce malformed
    // TOML that cargo would mis-parse.
    let paths_override = format!(
        "paths=[\"{}\"]",
        toml_escape(&worktree_abs.display().to_string())
    );
    let mut cmd = Command::new("cargo");
    cmd.current_dir(manifest_dir)
        .args(["build", "--release", "--bin", HARNESS_BIN])
        .arg("--target-dir")
        .arg(target_dir)
        .arg("--config")
        .arg(&paths_override)
        .env("BENCH_CORE_DIR", &worktree_abs);
    // Forward the bench crate's `libvips` feature to the per-tag build when this
    // driver was compiled with it. The feature selects how the libvips baseline
    // cell is measured — in-process FFI (`bench_libvips_inprocess`) with it, the
    // `vips` CLI fallback without — so matching the driver keeps that cell
    // measured the same way as the everyday `report`. It is a bench-crate
    // concern, independent of the core tag; gating on the driver's own build
    // also guarantees the system libvips it needs is present (issue #19).
    #[cfg(feature = "libvips")]
    cmd.args(["--features", "libvips"]);
    let status = cmd
        .status()
        .map_err(|e| MatrixError::Build(format!("failed to spawn cargo build: {e}")))?;
    if !status.success() {
        return Err(MatrixError::Build(format!(
            "cargo build against {} ({}) failed",
            worktree.refname(),
            worktree.short_sha()
        )));
    }
    if !exe_path.exists() {
        return Err(MatrixError::Build(format!(
            "harness binary {} missing after build",
            exe_path.display()
        )));
    }

    // Make the artifact the single source of truth for identity. The freshly
    // built binary self-reports the core its `build.rs` stamped from
    // `BENCH_CORE_DIR`; assert it matches the ref we resolved *before* any
    // measurement is recorded under that ref. A mismatch means the version
    // plumbing broke (a stale re-used stamp, an unhonoured `BENCH_CORE_DIR`), so
    // refuse to persist numbers under a false identity rather than doing it
    // silently.
    let (reported_version, reported_sha) = harness_core_identity(&exe_path)?;
    if reported_version != worktree.version() {
        return Err(MatrixError::Build(format!(
            "built harness self-reports core {reported_version} but ref {} resolved to {}",
            worktree.refname(),
            worktree.version(),
        )));
    }
    // The SHA is a second, independent check; skip it only when either side is
    // the `unknown` git-less fallback.
    if reported_sha != "unknown"
        && worktree.short_sha() != "unknown"
        && reported_sha != worktree.short_sha()
    {
        return Err(MatrixError::Build(format!(
            "built harness self-reports sha {reported_sha} but ref {} resolved to {}",
            worktree.refname(),
            worktree.short_sha(),
        )));
    }

    Ok(BuiltHarness { exe: exe_path })
}

/// Ask a built harness binary which core it was compiled against, via the
/// hidden `--print-core` subcommand (see
/// [`harness::maybe_run_print_core_subcommand`]). The output is one line,
/// `version\tshort_sha` — the `build.rs` stamp. This is what lets
/// [`build_harness`] treat the measured artifact, not a side channel, as the
/// source of truth for a snapshot's identity.
fn harness_core_identity(exe: &Path) -> Result<(String, String), MatrixError> {
    let out = Command::new(exe)
        .arg("--print-core")
        .output()
        .map_err(|e| {
            MatrixError::Build(format!(
                "failed to spawn {} --print-core: {e}",
                exe.display()
            ))
        })?;
    if !out.status.success() {
        return Err(MatrixError::Build(format!(
            "{} --print-core exited non-zero",
            exe.display()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.trim();
    let (version, sha) = line.split_once('\t').ok_or_else(|| {
        MatrixError::Build(format!(
            "unparseable --print-core output from {}: {line:?}",
            exe.display()
        ))
    })?;
    Ok((version.trim().to_string(), sha.trim().to_string()))
}

/// Escape a string for embedding in a TOML basic (double-quoted) string:
/// backslash and double-quote are the only characters that would break out of,
/// or be mis-parsed inside, `"…"`.
fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
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
    // `save_history` is atomic (temp file + rename) and fallible: a write fault
    // is surfaced as a per-version `History` skip so the sweep continues and the
    // prior file stays intact, never a panic that aborts the whole run.
    crate::save_history(history_path, &history).map_err(MatrixError::History)?;
    Ok(history.len())
}

/// Configuration for a [`run_matrix`] sweep. [`Default`] consumes the shared
/// suite constants ([`crate::DEFAULT_SIZES`], [`crate::DEFAULT_CONCURRENCY`],
/// [`crate::BENCH_TILE_SIZE`], [`crate::BENCH_STREAMING_BUDGET`], and
/// [`harness::DEFAULT_ITERS`] / [`harness::DEFAULT_WARMUP`]) — the exact same
/// definitions the everyday `report` benchmark uses — so the release-history
/// axis measures the identical suite as a compile-time fact, not by hand-copied
/// literals.
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
            sizes: crate::DEFAULT_SIZES.to_vec(),
            concurrency: crate::DEFAULT_CONCURRENCY.to_vec(),
            tile_size: crate::BENCH_TILE_SIZE,
            memory_budget_bytes: crate::BENCH_STREAMING_BUDGET,
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
    /// The version was skipped: `error` is the typed [`MatrixError`] (a `Git`
    /// checkout failure, a `Build` failure, or a per-version `History` write
    /// fault), so a consumer can tell the reason apart programmatically. Render
    /// it with `error.to_string()` at the print site.
    Skipped { refname: String, error: MatrixError },
}

/// RAII cleanup for a matrix-owned temporary target directory.
///
/// A synthesised `--release` build tree can be hundreds of MB; removing it in
/// `Drop` makes cleanup panic-safe and symmetric with [`CoreWorktree`], instead
/// of leaking the tree when the sweep panics partway through. A caller-supplied
/// `target_dir` is not owned and is left in place.
struct OwnedTargetDir {
    path: PathBuf,
    owned: bool,
}

impl Drop for OwnedTargetDir {
    fn drop(&mut self) {
        if self.owned {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
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
    // across versions (only the core crate + harness relink per tag; the shared
    // stamp is re-verified against each ref after the build). A dir we
    // synthesised is cleaned up (RAII, panic-safe); a caller-supplied one is
    // left be.
    let target = match &cfg.target_dir {
        Some(dir) => OwnedTargetDir {
            path: dir.clone(),
            owned: false,
        },
        None => OwnedTargetDir {
            path: std::env::temp_dir()
                .join("libviprs-bench-vmatrix-target")
                .join(std::process::id().to_string()),
            owned: true,
        },
    };

    let total = refs.len();
    let mut outcomes = Vec::with_capacity(total);
    for (idx, refname) in refs.iter().enumerate() {
        // Per-tag heartbeat + wall-clock on stderr (stdout stays parseable): each
        // step is a full LTO release build plus the isolated suite — many minutes.
        eprintln!(
            "version-matrix: [{}/{total}] building {refname} ...",
            idx + 1
        );
        let started = Instant::now();
        match run_one_version(repo, refname, cfg, &target.path, history_path) {
            Ok((version, short_sha, entries)) => {
                eprintln!(
                    "version-matrix: [{}/{total}] appended {} ({refname}) in {:.1}s — \
                     {entries} snapshot(s) on record",
                    idx + 1,
                    version_key(&version, &short_sha),
                    started.elapsed().as_secs_f64(),
                );
                outcomes.push(VersionOutcome::Appended {
                    refname: refname.clone(),
                    version,
                    short_sha,
                    entries,
                });
            }
            Err(e) => {
                eprintln!(
                    "version-matrix: [{}/{total}] WARNING skipping {refname} after {:.1}s: {e}",
                    idx + 1,
                    started.elapsed().as_secs_f64(),
                );
                outcomes.push(VersionOutcome::Skipped {
                    refname: refname.clone(),
                    error: e,
                });
            }
        }
    }
    outcomes
    // `target` drops here, removing the owned build tree.
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
        worktree.version(),
        worktree.short_sha(),
        runs,
        cfg.tile_size,
        cfg.memory_budget_bytes,
    )?;
    Ok((
        worktree.version().to_string(),
        worktree.short_sha().to_string(),
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

/// Read a bare string field (`name`, `version`, …) from the `[package]` table
/// of a Cargo manifest with a small hand scan (no TOML dependency), mirroring
/// `build.rs`. `None` if the manifest is unreadable or the field is absent.
///
/// `build.rs` keeps its own copy of this scan out of necessity — a build script
/// cannot depend on the crate it builds — but the two library call sites
/// (version, name) share this one implementation.
fn read_package_field(manifest: &Path, field: &str) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            if let Some(rest) = line.strip_prefix(field) {
                // Guard against key-prefix collisions (`name` vs `namespace`):
                // the key must be followed by optional whitespace then `=`.
                if let Some(rest) = rest.trim_start().strip_prefix('=') {
                    return Some(rest.trim().trim_matches('"').to_string());
                }
            }
        }
    }
    None
}

/// The `[package] version` of a Cargo manifest, or `"unknown"` if absent.
fn read_package_version(manifest: &Path) -> String {
    read_package_field(manifest, "version").unwrap_or_else(|| "unknown".to_string())
}

/// The `[package] name` of a Cargo manifest, or `None` if absent/unreadable.
fn read_package_name(manifest: &Path) -> Option<String> {
    read_package_field(manifest, "name")
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

    // Pure version identity/ordering (`version_key`, `ordered_version_keys`,
    // `semver_sort_key`) is tested in `crate::version_id`; this module owns the
    // orchestration-adjacent helpers.

    #[test]
    fn sanitize_ref_is_filesystem_safe() {
        assert_eq!(sanitize_ref("v0.3.1"), "v0.3.1");
        assert_eq!(sanitize_ref("feature/foo"), "feature_foo");
        assert_eq!(sanitize_ref(""), "ref");
    }

    #[test]
    fn toml_escape_neutralises_quote_and_backslash() {
        assert_eq!(toml_escape("/tmp/plain"), "/tmp/plain");
        assert_eq!(toml_escape(r"C:\tmp\wt"), r"C:\\tmp\\wt");
        // A crafted path cannot break out of the `paths=["…"]` basic string to
        // close the array or inject a trailing key: the `"` is escaped.
        assert_eq!(toml_escape(r#"/tmp/a"]evil=["#), r#"/tmp/a\"]evil=["#);
    }

    #[test]
    fn read_package_field_reads_name_and_version_guarding_prefix_collisions() {
        let dir = std::env::temp_dir().join(format!("vmatrix_manifest_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = dir.join("Cargo.toml");
        std::fs::write(
            &manifest,
            "[package]\nnamespace = \"nope\"\nname = \"libviprs\"\nversion = \"0.4.0\"\n\n[dependencies]\nname = \"wrong-table\"\n",
        )
        .unwrap();

        assert_eq!(read_package_name(&manifest).as_deref(), Some("libviprs"));
        assert_eq!(read_package_version(&manifest), "0.4.0");
        // `name` must not be matched by the `namespace` line that precedes it,
        // and a key that is only a prefix (`nam`) matches nothing.
        assert_eq!(read_package_field(&manifest, "nam"), None);
        // Absent field / unreadable manifest degrade cleanly.
        assert_eq!(read_package_field(&manifest, "edition"), None);
        assert_eq!(read_package_version(&dir.join("missing.toml")), "unknown");
        assert_eq!(read_package_name(&dir.join("missing.toml")), None);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
