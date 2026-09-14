//! Capture the *measured* core crate's identity at build time.
//!
//! The bench history keys every snapshot on the version of the
//! `libviprs` engine it measured, not on this harness's own version.
//! Cargo only exposes `CARGO_PKG_VERSION` for the crate being compiled,
//! which here is `libviprs-bench` (a different version from core), so I
//! read the sibling path dependency's manifest directly and stamp two
//! compile-time env vars the library reads back:
//!
//!   * `LIBVIPRS_CORE_VERSION` — the `[package] version` from
//!     `../libviprs/Cargo.toml`.
//!   * `LIBVIPRS_CORE_GIT_SHA` — the short git SHA of that checkout, or
//!     `unknown` when git is unavailable (git-less tarball, no repo).
//!
//! Both always get emitted (with `unknown` fallbacks) so the library can
//! read them unconditionally without risking a compile error.

use std::path::{Path, PathBuf};
use std::process::Command;

// SHA-256 in `std` alone, shared verbatim with the library. A build script
// cannot use its own crate, and this needs the same digest the archive uses, so
// the file is pulled in here rather than duplicated. `src/sha256.rs` carries no
// inner doc comments precisely so that this `include!` compiles.
include!("src/sha256.rs");

/// Path to the measured core crate, relative to this crate's manifest.
/// It is a Cargo path dependency (`libviprs = { path = "../libviprs" }`),
/// so if this crate compiles at all the directory is present.
const CORE_DIR: &str = "../libviprs";

fn main() {
    // The version-matrix runner rebuilds this harness against a *worktree* of
    // the core crate checked out at some tag, redirecting the linked library
    // with a Cargo `paths` override. `paths` overrides don't reach this build
    // script, so it honours `BENCH_CORE_DIR`: when set, the version/SHA stamps
    // are read from that worktree instead of the sibling `../libviprs`, keeping
    // the built binary's self-reported version in step with the library it
    // actually linked (issues #19, #26). Unset (the everyday build) → `../libviprs`.
    let core_dir = std::env::var("BENCH_CORE_DIR").unwrap_or_else(|_| CORE_DIR.to_string());
    let core_dir = Path::new(&core_dir);
    let manifest = core_dir.join("Cargo.toml");

    let version = read_package_version(&manifest).unwrap_or_else(|| "unknown".to_string());
    let sha = git_short_sha(core_dir).unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=LIBVIPRS_CORE_VERSION={version}");
    println!("cargo:rustc-env=LIBVIPRS_CORE_GIT_SHA={sha}");

    // Stamp the toolchain + the pinned bench build knobs into the binary so
    // every snapshot's provenance records the environment it was measured
    // in (issue #159). RUSTFLAGS is echoed so the LTO/codegen-units pin from
    // run-bench.sh is visible in the recorded fingerprint.
    let rustc_version = rustc_version().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=BENCH_RUSTC_VERSION={rustc_version}");
    let build_flags = std::env::var("RUSTFLAGS").unwrap_or_default();
    println!("cargo:rustc-env=BENCH_BUILD_FLAGS={build_flags}");

    // Re-run when the core manifest changes so the stamped version keeps
    // pace with a core version bump without a manual clean.
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed=build.rs");
    // Re-run when the version-matrix runner repoints the measured core at a
    // per-tag worktree, so each tag's build re-stamps its own version/SHA.
    println!("cargo:rerun-if-env-changed=BENCH_CORE_DIR");

    stamp_storage_provenance(core_dir);
}

// ---------------------------------------------------------------------------
// The storage suite's provenance stamps (libviprs-bench #66).
//
// Everything below exists because `docs/pmtiles-benchmarks.md` publishes 480
// lines of numbers and records no platform at all. A benchmark document has to
// be able to say what produced it, and half of "what produced it" is only
// knowable while the crate is being compiled: the toolchain, the flags, the
// resolved dependency graph and the state of the two source trees. So it is
// captured here and read back at runtime from `option_env!` and one generated
// file, rather than guessed later from a tree that may not even be mounted.
// ---------------------------------------------------------------------------

/// Stamp everything the storage document's `provenance` block needs.
fn stamp_storage_provenance(core_dir: &Path) {
    let cargo_version = cargo_version().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=BENCH_CARGO_VERSION={cargo_version}");

    // `RUSTFLAGS` is the human-set one; `CARGO_ENCODED_RUSTFLAGS` is what cargo
    // actually hands rustc, including flags from `.cargo/config.toml` and the
    // `[build]` section that `RUSTFLAGS` alone never shows. Recording only the
    // first is how a run picks up `-C target-cpu=native` from a config file and
    // reports an empty flag set. The encoded form separates flags with 0x1f,
    // which is turned into a space so the value survives an env var.
    let encoded = std::env::var("CARGO_ENCODED_RUSTFLAGS")
        .ok()
        .map(|v| v.split('\u{1f}').collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    println!("cargo:rustc-env=BENCH_RUSTFLAGS_ENCODED={encoded}");
    println!("cargo:rerun-if-env-changed=RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");

    // Both trees, each with its own note saying why a field is missing when it
    // is. The notes are the whole point: a null commit has at least three
    // ordinary causes and they are fixed from the environment, so an operator
    // needs to be told which one they are in.
    let harness = git_tree_state(Path::new("."));
    let library = git_tree_state(core_dir);
    for (label, stamp) in [("HARNESS", &harness), ("LIBRARY", &library)] {
        println!(
            "cargo:rustc-env=BENCH_{label}_COMMIT={}",
            stamp.commit.clone().unwrap_or_default()
        );
        println!(
            "cargo:rustc-env=BENCH_{label}_DIRTY={}",
            match stamp.dirty {
                Some(true) => "true",
                Some(false) => "false",
                None => "",
            }
        );
        println!("cargo:rustc-env=BENCH_{label}_GIT_NOTE={}", stamp.note);
        if stamp.commit.is_none() || stamp.dirty.is_none() {
            // A `cargo:warning=` is the loud half of the contract. The document
            // will refuse to archive either way, but a refusal an hour after a
            // forty-minute sweep is worse than a warning before it starts.
            println!(
                "cargo:warning=provenance: {} tree at {} -- {}",
                label.to_lowercase(),
                stamp.dir.display(),
                stamp.note
            );
        }
        for path in &stamp.rerun {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    stamp_lockfile();
}

/// The state of one source tree, and why it is not better than it is.
struct TreeStamp {
    dir: PathBuf,
    commit: Option<String>,
    dirty: Option<bool>,
    note: String,
    rerun: Vec<PathBuf>,
}

/// The cargo driving this build, for the toolchain half of the fingerprint.
fn cargo_version() -> Option<String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(cargo).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let v = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if v.is_empty() { None } else { Some(v) }
}

/// Read one tree's commit and dirty flag, handling the three ways a previous
/// campaign produced `commit: null` by accident.
///
/// None of the three is the harness's fault and all three are fixed from the
/// environment, which is exactly why each one gets named rather than swallowed:
///
/// 1. **A linked git worktree bind-mounted into a container.** Its `.git` is a
///    *file* holding `gitdir: <path>` that points outside the mount, so git
///    answers "not a git repository" and every stamp comes back empty. This
///    code is running in one right now. The fix is to mount the real gitdir at
///    the same absolute path; the detection is to read the pointer and check
///    whether its target exists, which distinguishes it from a tarball with no
///    repository at all.
/// 2. **`safe.directory`.** A tree owned by another uid, which is what a NAS
///    share read as root looks like, makes every git call exit 128 with
///    "dubious ownership". Passing `-c safe.directory=<abs path>` for the one
///    directory being asked about clears it without touching global config and
///    without the blanket `*` that would also silence a genuinely foreign repo.
/// 3. **A share that forces mode 0777.** Every file then reads as a mode
///    change, so `git status --porcelain` lists all 814 of them and `dirty`
///    comes back true on a tree nobody edited. `-c core.fileMode=false` is the
///    fix. The status is run *both* ways so the difference can be reported: a
///    tree that is clean with fileMode off and dirty with it on is in this case
///    and nothing else, and saying so is better than quietly masking it.
fn git_tree_state(dir: &Path) -> TreeStamp {
    let abs = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut notes: Vec<String> = Vec::new();
    let mut rerun: Vec<PathBuf> = Vec::new();

    // Trap 1, detected before git is asked, so the message can name the cause.
    let dot_git = abs.join(".git");
    if dot_git.is_file() {
        match std::fs::read_to_string(&dot_git) {
            Ok(text) => {
                let target = text
                    .lines()
                    .find_map(|l| l.trim().strip_prefix("gitdir:"))
                    .map(|p| PathBuf::from(p.trim()));
                match target {
                    Some(target) if !target.exists() => notes.push(format!(
                        "this is a linked git worktree whose .git file points at {}, which \
                         does not exist here; bind-mount that directory at the same absolute \
                         path and the commit comes back",
                        target.display()
                    )),
                    Some(target) => rerun.push(target.join("HEAD")),
                    None => notes.push(format!(
                        "{} is a file but carries no gitdir: line",
                        dot_git.display()
                    )),
                }
            }
            Err(err) => notes.push(format!("{} is unreadable: {err}", dot_git.display())),
        }
    } else if dot_git.is_dir() {
        rerun.push(dot_git.join("HEAD"));
    } else {
        notes.push(
            "there is no .git here at all, so this is a source tree with no repository".to_string(),
        );
    }

    let commit = match git(&abs, &["rev-parse", "HEAD"]) {
        Ok(sha) => Some(sha),
        Err(err) => {
            notes.push(err);
            None
        }
    };

    // Trap 3: ask twice and report the difference rather than only the answer.
    let dirty = match (git_status_dirty(&abs, false), git_status_dirty(&abs, true)) {
        (Ok(with_modes), Ok(without_modes)) => {
            if with_modes && !without_modes {
                notes.push(
                    "the tree reads as dirty only while git compares file modes, which is \
                     what a share that forces 0777 does to every file; the dirty flag below \
                     ignores mode-only differences"
                        .to_string(),
                );
            }
            Some(without_modes)
        }
        (_, Ok(without_modes)) => Some(without_modes),
        (_, Err(err)) => {
            notes.push(err);
            None
        }
    };

    let note = if notes.is_empty() {
        "clean read".to_string()
    } else {
        notes.join("; ")
    };
    TreeStamp {
        dir: abs,
        commit,
        dirty,
        note,
        rerun,
    }
}

/// Run git in `dir` with the two settings that clear traps 2 and 3, and turn a
/// failure into a sentence rather than a `None`.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .arg("-c")
        .arg(format!("safe.directory={}", dir.display()))
        .arg("-c")
        .arg("core.fileMode=false")
        .arg("-C")
        .arg(dir)
        .args(args);
    let output = match command.output() {
        Ok(output) => output,
        Err(err) => return Err(format!("git could not be run ({err})")),
    };
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Ok(text);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let cause = if stderr.contains("dubious ownership") {
        "git still refuses this tree as dubiously owned even with safe.directory set for it"
    } else if stderr.contains("not a git repository") {
        "git does not see a repository here"
    } else {
        "git failed"
    };
    Err(format!(
        "{cause} (git {} exited {}: {stderr})",
        args.join(" "),
        output.status.code().unwrap_or(-1)
    ))
}

/// Whether `git status --porcelain -uno` lists anything, with file-mode
/// comparison on or off.
fn git_status_dirty(dir: &Path, compare_modes: bool) -> Result<bool, String> {
    let mut command = Command::new("git");
    command
        .arg("-c")
        .arg(format!("safe.directory={}", dir.display()))
        .arg("-c")
        .arg(format!("core.fileMode={compare_modes}"))
        .arg("-C")
        .arg(dir)
        .args(["status", "--porcelain", "-uno"]);
    let output = command
        .output()
        .map_err(|err| format!("git could not be run ({err})"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "git status exited {}: {stderr}",
            output.status.code().unwrap_or(-1)
        ));
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

/// Digest `Cargo.lock` and write the resolved graph out for the library to read
/// back.
///
/// `SUITE-PLAN.md` §5.4 asks for this "via `build.rs` and `cargo metadata`". It
/// is the lockfile that gets parsed instead, and the reason is narrow: a build
/// script that shells out to `cargo metadata` runs cargo underneath a cargo
/// that may be holding the package-cache lock, which is a documented way to
/// wedge a build until someone kills it. `cargo metadata` resolves *from*
/// `Cargo.lock`, so reading the lockfile is the same facts by a route that
/// cannot deadlock: name, version, `source` and `checksum` are Cargo's own
/// spellings of what npm calls `resolved` and `integrity`.
///
/// The graph goes to a file in `OUT_DIR` rather than an env var because it is
/// tens of kilobytes of JSON and an env var is the wrong shape for that.
fn stamp_lockfile() {
    let lock_path = Path::new("Cargo.lock");
    println!("cargo:rerun-if-changed=Cargo.lock");
    let out_dir = std::env::var("OUT_DIR").expect("cargo always sets OUT_DIR for a build script");
    let generated = Path::new(&out_dir).join("lock-dependencies.json");

    let Ok(text) = std::fs::read_to_string(lock_path) else {
        println!("cargo:rustc-env=BENCH_LOCKFILE_HASH=");
        println!(
            "cargo:warning=provenance: Cargo.lock is unreadable, so the archived document \
             will carry no dependency graph and will be refused"
        );
        std::fs::write(&generated, "{}\n").expect("OUT_DIR is writable");
        return;
    };

    println!(
        "cargo:rustc-env=BENCH_LOCKFILE_HASH=sha256:{}",
        sha256_hex(text.as_bytes())
    );
    std::fs::write(&generated, lock_dependencies_json(&text)).expect("OUT_DIR is writable");
}

/// `Cargo.lock` into `{name: {version, source, checksum}}`, sorted.
///
/// A name that appears at more than one version is keyed `name@version` for
/// every one of its entries, so a graph with two majors of `sha2` in it records
/// both rather than silently keeping whichever came last. A name that appears
/// once keeps its bare name, which is the shape the plan's table asks for and
/// the shape `provenance.library` is checked against.
fn lock_dependencies_json(lock: &str) -> String {
    #[derive(Default)]
    struct Pkg {
        name: String,
        version: String,
        source: Option<String>,
        checksum: Option<String>,
    }

    fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
        let rest = line.strip_prefix(key)?.trim_start();
        let rest = rest.strip_prefix('=')?.trim();
        Some(rest.trim_matches('"'))
    }

    let mut packages: Vec<Pkg> = Vec::new();
    let mut current: Option<Pkg> = None;
    for line in lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            if let Some(pkg) = current.take() {
                packages.push(pkg);
            }
            current = Some(Pkg::default());
            continue;
        }
        if line.starts_with('[') {
            if let Some(pkg) = current.take() {
                packages.push(pkg);
            }
            continue;
        }
        let Some(pkg) = current.as_mut() else {
            continue;
        };
        if let Some(v) = field(line, "name") {
            pkg.name = v.to_string();
        } else if let Some(v) = field(line, "version") {
            pkg.version = v.to_string();
        } else if let Some(v) = field(line, "source") {
            pkg.source = Some(v.to_string());
        } else if let Some(v) = field(line, "checksum") {
            pkg.checksum = Some(v.to_string());
        }
    }
    if let Some(pkg) = current.take() {
        packages.push(pkg);
    }

    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for pkg in &packages {
        *counts.entry(pkg.name.as_str()).or_default() += 1;
    }

    let mut rows: Vec<(String, String)> = packages
        .iter()
        .filter(|pkg| !pkg.name.is_empty())
        .map(|pkg| {
            let key = if counts.get(pkg.name.as_str()).copied().unwrap_or(0) > 1 {
                format!("{}@{}", pkg.name, pkg.version)
            } else {
                pkg.name.clone()
            };
            let body = format!(
                "{{\"version\":{},\"source\":{},\"checksum\":{}}}",
                json_str(&pkg.version),
                pkg.source
                    .as_deref()
                    .map(json_str)
                    .unwrap_or_else(|| "null".to_string()),
                pkg.checksum
                    .as_deref()
                    .map(json_str)
                    .unwrap_or_else(|| "null".to_string()),
            );
            (key, body)
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows.dedup_by(|a, b| a.0 == b.0);

    let mut out = String::from("{");
    for (i, (key, body)) in rows.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_str(key));
        out.push(':');
        out.push_str(body);
    }
    out.push_str("}\n");
    out
}

/// The same escaping rule the canonicaliser uses, in the few characters a crate
/// name, version, source URL or checksum can actually contain.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Extract the first `version = "..."` from the `[package]` section of a
/// Cargo manifest. I keep this to a small hand scan rather than pulling
/// in a TOML parser as a build dependency: the `[package] version` line
/// is stable and appears before any other table.
///
/// This intentionally duplicates the library's `version_matrix::read_package_field`:
/// a build script cannot depend on the crate it builds, so the two copies can't
/// be merged. Keep them in step.
fn read_package_version(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            if let Some(rest) = line.strip_prefix("version") {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('=') {
                    return Some(rest.trim().trim_matches('"').to_string());
                }
            }
        }
    }
    None
}

/// The compiling rustc's version string (`rustc 1.89.0 (… )`), via the
/// `RUSTC` cargo exposes to build scripts. `None` if it cannot be run.
fn rustc_version() -> Option<String> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let output = Command::new(rustc).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let v = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if v.is_empty() { None } else { Some(v) }
}

/// Short git SHA of the core checkout, or `None` if git can't resolve it
/// (no repository, git not installed, detached tarball).
fn git_short_sha(core_dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(core_dir)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}
