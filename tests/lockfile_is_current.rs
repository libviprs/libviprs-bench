//! #156: a stale Cargo.lock fails a gate instead of a capture.
//!
//! `cargo check` and `cargo test` rewrite a stale lock in the working tree and
//! carry on, so nothing that builds can fail on it from the inside (a test that
//! ran `cargo metadata --locked` would pass, because the build before it had
//! already fixed the file). The only place the refusal can come from is the
//! command line: `--locked` on the gate's own `cargo test`, which is what the
//! capture image's `cargo fetch --locked` already does.

use std::fs;

#[test]
fn the_test_image_script_runs_cargo_test_locked() {
    let script = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tools/test-image/run-tests.sh"
    ))
    .expect("read the test image script");
    assert!(
        script.contains("cargo test --locked"),
        "run-tests.sh must run `cargo test --locked`, or a stale Cargo.lock is rewritten and the gate stays green"
    );
}

#[test]
fn the_capture_image_builds_locked() {
    let dockerfile = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Dockerfile"))
        .expect("read the Dockerfile");
    assert!(
        dockerfile.contains("cargo fetch --locked"),
        "the capture image is the other place a stale lock has to be refused"
    );
}
