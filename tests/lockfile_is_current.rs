//! #156: the committed Cargo.lock is the one the manifest resolves to.
//!
//! `cargo check` and `cargo test` rewrite a stale lock in the working tree and
//! carry on, which is why a stale one never failed a gate. The capture image
//! builds with `cargo fetch --locked`, which refuses, so a stale lock showed up
//! hours later as a sweep that could not start. `--locked` here is the same
//! refusal. It needs the network the way the build does, to read the manifests
//! of the packages the lock names.

use std::process::Command;

#[test]
fn cargo_lock_needs_no_update_against_the_sibling_core() {
    let out = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--locked",
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
