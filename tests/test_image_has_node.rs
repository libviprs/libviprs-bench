//! The Rust test image needs node (#135).
//!
//! `tests/engines_family_end_to_end.rs` shells out to `node` to prove the charts
//! are drawn, and fails on purpose when it is missing. The stock
//! `rust:*-bookworm` image has no node, so `cargo test` was red on MARS on main.
//! These read only committed files: there has to be a test image that carries
//! both toolchains, a script that runs the suite in it, and a README that says
//! the red in the bare Rust image is not a regression.

const TEST_DOCKERFILE: &str = include_str!("../tools/test-image/Dockerfile");
const TEST_SCRIPT: &str = include_str!("../tools/test-image/run-tests.sh");
const README: &str = include_str!("../README.md");

#[test]
fn test_image_is_a_rust_image() {
    assert!(
        TEST_DOCKERFILE.contains("FROM rust:1.99.1-bookworm@sha256:"),
        "the test image starts from the same digest-pinned Rust image as the bench Dockerfile"
    );
}

#[test]
fn test_image_carries_a_pinned_node_new_enough_for_package_json() {
    assert!(
        TEST_DOCKERFILE.contains("COPY --from=node:22-bookworm-slim@sha256:"),
        "node comes from a digest-pinned image, not a floating apt package (bookworm's is 18)"
    );
    assert!(
        TEST_DOCKERFILE.contains("/usr/local/bin/node"),
        "node lands on PATH in the image"
    );
}

#[test]
fn script_runs_cargo_test_inside_that_image() {
    assert!(TEST_SCRIPT.contains("docker build"), "builds the test image");
    assert!(TEST_SCRIPT.contains("docker run --rm"), "throw-away container");
    assert!(TEST_SCRIPT.contains("cargo test"), "runs the suite");
    assert!(
        TEST_SCRIPT.contains("CARGO_TARGET_DIR=/tmp/target"),
        "target dir stays off the bind mount"
    );
}

#[test]
fn readme_explains_the_node_red_and_names_the_image() {
    assert!(
        README.contains("tools/test-image/run-tests.sh"),
        "README points at the script that runs the suite with node present"
    );
    assert!(
        README.contains("engines_family_end_to_end") && README.contains("not a regression"),
        "README says the bare Rust image's red is not a regression"
    );
}
