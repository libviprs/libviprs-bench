//! #156: the committed Cargo.lock is the one the manifest resolves to.
//!
//! `cargo check` and `cargo test` rewrite a stale lock in the working tree and
//! carry on, which is why a stale one never failed a gate. The capture image
//! builds with `cargo fetch --locked`, which refuses, so a stale lock showed up
//! hours later as a sweep that could not start. `--locked` here is the same
//! refusal, and `--offline` keeps the test from reaching for the network: the
//! build that ran just before has already filled the registry cache.

use std::process::Command;

#[test]
fn cargo_lock_needs_no_update_against_the_sibling_core() {
    let out = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
            "--manifest-path",
            concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
        ])
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "Cargo.lock is behind the manifest and libviprs main; refresh it:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
