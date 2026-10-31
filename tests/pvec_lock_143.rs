//! The `pvec` Docker stage has to build from a committed lock (libviprs-bench#143).
//!
//! The first PVec run was archived dirty because the container swapped PVec's private crates for
//! local paths and cargo rewrote `Cargo.lock` on the way, so every cell was stamped modified and
//! the importer refused the document. A clean capture needs two things in the tree itself: a lock
//! that already names the git pins, so `--locked` has nothing to rewrite, and a stage that gets
//! read access to the private repository from a build secret, never from a file or a layer.

use std::fs;

const PIN: &str = "b9f441618153d256777121a6a1518b15e7f42099";

fn read(path: &str) -> String {
    fs::read_to_string(format!("{}/{path}", env!("CARGO_MANIFEST_DIR"))).expect(path)
}

#[test]
fn lock_names_the_pvec_pin_for_every_pvec_crate_the_manifest_declares() {
    let manifest = read("Cargo.toml");
    let lock = read("Cargo.lock");
    let source = format!("git+https://github.com/libviprs/pvec?rev={PIN}#{PIN}");
    for name in ["pvec", "pvec-geometry"] {
        assert!(manifest.contains(&format!("rev = \"{PIN}\"")), "Cargo.toml no longer pins {PIN}");
        let stanza = format!("name = \"{name}\"\nversion = \"0.0.0\"\nsource = \"{source}\"");
        assert!(lock.contains(&stanza), "Cargo.lock has no stanza for {name} at {PIN}");
    }
}

#[test]
fn lock_carries_the_version_of_the_library_it_measures() {
    let Ok(lib) = fs::read_to_string(format!("{}/../libviprs/Cargo.toml", env!("CARGO_MANIFEST_DIR")))
    else {
        return; // no sibling checkout, nothing to compare against
    };
    let version = lib
        .lines()
        .find_map(|l| l.strip_prefix("version = \"").and_then(|v| v.strip_suffix('"')))
        .expect("libviprs version");
    let lock = read("Cargo.lock");
    assert!(
        lock.contains(&format!("name = \"libviprs\"\nversion = \"{version}\"")),
        "Cargo.lock is stale against libviprs {version}, so --locked would rewrite it"
    );
}

#[test]
fn pvec_stage_reads_the_private_repo_through_a_build_secret_only() {
    let dockerfile = read("Dockerfile");
    let stage = dockerfile
        .split("AS pvec")
        .nth(1)
        .and_then(|s| s.split("\nFROM ").next())
        .expect("pvec stage");
    assert!(stage.contains("--mount=type=secret,id=gh_token"), "no build secret in the pvec stage");
    for forbidden in ["ARG GH_TOKEN", "ENV GH_TOKEN", "ARG GITHUB_TOKEN", "ENV GITHUB_TOKEN"] {
        assert!(!stage.contains(forbidden), "{forbidden} would put the token in an image layer");
    }
    assert!(!stage.contains("\nCOPY .netrc") && !stage.contains(".git-credentials"));
}
