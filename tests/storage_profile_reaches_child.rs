//! #112: the profile a sweep runs under has to reach the child that walks the
//! coordinates, and a document whose label and coordinates disagree has to be
//! refused rather than archived.
//!
//! Each test names the wrong implementation it goes red against.

use libviprs_bench::storage::cells::{Backend, Cell, Profile, Source};
use libviprs_bench::storage::{coordinate_sets, scenario_command};
use std::path::Path;

fn env_of(cmd: &std::process::Command, key: &str) -> Option<String> {
    cmd.get_envs()
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
}

fn command(profile: Profile) -> std::process::Command {
    let cell = Cell::new(2048, 2048, 256, Source::Gradient, 93);
    scenario_command(
        Path::new("/bin/true"),
        Backend::Directory,
        cell,
        "open",
        1,
        None,
        profile,
    )
}

// Goes red against: the parent that builds the child command from the backend,
// cell, scenario and reps alone, which is what shipped. The child then reads
// nothing and defaults to ci.
#[test]
fn the_full_profile_is_passed_to_the_child() {
    assert_eq!(
        env_of(&command(Profile::Full), "LIBVIPRS_BENCH_PROFILE").as_deref(),
        Some("full")
    );
}

// Goes red against: an implementation that passes the variable only when it is
// not ci, or that hardcodes one label.
#[test]
fn every_profile_label_survives_the_trip() {
    for p in [Profile::Ci, Profile::Full, Profile::Xl] {
        let got = env_of(&command(p), "LIBVIPRS_BENCH_PROFILE");
        assert_eq!(got.as_deref(), Some(p.label()));
        assert_eq!(Profile::parse(&got.unwrap()), Some(p));
    }
}

// The walk a child builds is what the label promises, on a cell big enough to
// tell the profiles apart (1373 tiles: ci walks 512, full walks them all).
#[test]
fn the_profile_changes_how_many_coordinates_a_cell_walks() {
    let plan = Cell::new(8192, 8192, 256, Source::Gradient, 1373)
        .plan()
        .unwrap();
    let ci = coordinate_sets(&plan, Profile::Ci, 7);
    let full = coordinate_sets(&plan, Profile::Full, 7);
    assert_eq!(ci.plan_order.len(), 512);
    assert_eq!(full.plan_order.len(), 1373);
}
