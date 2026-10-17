//! #112: a document whose profile label and read coordinates disagree is
//! refused at admission rather than archived and charted as a full sweep.

use libviprs_bench::storage::archive::admit;
use libviprs_bench::storage::cells::{Cell, Source};
use serde_json::{Value, json};

fn doc_with(profile: &str, cell: Cell, declared: f64) -> Value {
    json!({
        "profile": profile,
        "cells": [{
            "backend": "directory", "scenario": "requests", "metric": "requests_declared",
            "cell": cell.spec(), "scale": cell.declared_tiles, "outcome": "ok",
            "median": declared,
        }]
    })
}

fn codes(doc: &Value) -> Vec<&'static str> {
    admit(doc).into_iter().map(|r| r.code).collect()
}

fn big() -> Cell {
    Cell::new(8192, 8192, 256, Source::Gradient, 1373)
}

// Goes red against: no gate. A full-labelled document carrying a ci-sized walk
// (512 + 1) is exactly what was archived.
#[test]
fn a_full_document_with_a_ci_sized_walk_is_refused() {
    assert!(codes(&doc_with("full", big(), 513.0)).contains(&"profile-walk-mismatch"));
}

// Goes red against: a gate that refuses every full document, or compares
// against 20000 without capping at the plan length.
#[test]
fn a_full_document_with_the_full_walk_is_admitted_by_this_rule() {
    assert!(!codes(&doc_with("full", big(), 1374.0)).contains(&"profile-walk-mismatch"));
}

// Goes red against: a gate keyed on the label alone, which would refuse every
// honest ci run. The ci walk on the big cell is the one the ci label promises.
#[test]
fn a_ci_document_with_a_ci_walk_is_admitted_by_this_rule() {
    assert!(!codes(&doc_with("ci", big(), 513.0)).contains(&"profile-walk-mismatch"));
}

// Goes red against: a gate that only catches the downgrade. The disagreement
// is the defect, in either direction.
#[test]
fn a_ci_document_with_a_full_walk_is_refused_too() {
    assert!(codes(&doc_with("ci", big(), 1374.0)).contains(&"profile-walk-mismatch"));
}

// Goes red against: a gate that forgets a short plan caps the walk. The 93 tile
// control walks 93 under every profile and declares 94 under every profile.
#[test]
fn a_short_plan_declares_its_own_length_under_any_profile() {
    let control = Cell::new(2048, 2048, 256, Source::Gradient, 93);
    for p in ["ci", "full"] {
        assert!(!codes(&doc_with(p, control, 94.0)).contains(&"profile-walk-mismatch"));
    }
}
