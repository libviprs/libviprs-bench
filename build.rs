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

use std::path::Path;
use std::process::Command;

/// Path to the measured core crate, relative to this crate's manifest.
/// It is a Cargo path dependency (`libviprs = { path = "../libviprs" }`),
/// so if this crate compiles at all the directory is present.
const CORE_DIR: &str = "../libviprs";

fn main() {
    let core_dir = Path::new(CORE_DIR);
    let manifest = core_dir.join("Cargo.toml");

    let version = read_package_version(&manifest).unwrap_or_else(|| "unknown".to_string());
    let sha = git_short_sha(core_dir).unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=LIBVIPRS_CORE_VERSION={version}");
    println!("cargo:rustc-env=LIBVIPRS_CORE_GIT_SHA={sha}");

    // Re-run when the core manifest changes so the stamped version keeps
    // pace with a core version bump without a manual clean.
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed=build.rs");
}

/// Extract the first `version = "..."` from the `[package]` section of a
/// Cargo manifest. I keep this to a small hand scan rather than pulling
/// in a TOML parser as a build dependency: the `[package] version` line
/// is stable and appears before any other table.
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
