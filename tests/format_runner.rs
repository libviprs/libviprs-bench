//! #102: the `format` family runner. Cells are keyed on (content class, pyramid level, format);
//! the corpus classes that are not committed are named as blocked rather than faked.

use libviprs_bench::format::runner::{
    CellRow, ClassStatus, Codec, content_classes, run_synthetic_control, validate,
};

const LEVELS: u32 = 4;

fn rows() -> Vec<CellRow> {
    run_synthetic_control(LEVELS).expect("the synthetic control runs")
}

#[test]
fn one_row_per_level_and_format() {
    let rows = rows();
    assert_eq!(rows.len(), (LEVELS as usize) * Codec::ALL.len());
    for level in 0..LEVELS {
        for codec in Codec::ALL {
            let n = rows
                .iter()
                .filter(|r| {
                    r.class == "synthetic_control" && r.level == level && r.format == codec.name()
                })
                .count();
            assert_eq!(n, 1, "level {level} {}", codec.name());
        }
    }
}

#[test]
fn every_lossless_cell_is_exact_or_the_run_is_invalid() {
    let rows = rows();
    for r in rows.iter().filter(|r| {
        Codec::ALL
            .iter()
            .any(|c| c.name() == r.format && c.is_lossless())
    }) {
        assert_eq!(r.exact_match_rate, 1.0, "{} level {}", r.format, r.level);
    }
    let mut bad = rows.clone();
    let i = bad.iter().position(|r| r.format == "png").unwrap();
    bad[i].exact_match_rate = 0.999;
    assert!(
        validate(&bad).is_err(),
        "a lossless cell below 100% invalidates the run"
    );
    assert!(validate(&rows).is_ok());
}

#[test]
fn jpeg_is_not_exact_and_its_edge_is_measured_against_truth() {
    let rows = rows();
    let j = rows
        .iter()
        .find(|r| r.format == "jpeg85" && r.level == 0)
        .unwrap();
    assert!(j.exact_match_rate < 1.0);
    let d = j
        .edge_displacement
        .expect("the control has an edge to find");
    assert!(d.is_finite() && d < 2.0, "displacement {d}");
    let p = rows
        .iter()
        .find(|r| r.format == "png" && r.level == 0)
        .unwrap();
    assert!(
        p.edge_displacement.unwrap() < 0.05,
        "a lossless cell lands on the truth"
    );
}

#[test]
fn dedupe_is_counted_before_and_after() {
    for r in rows() {
        assert!(r.bytes_after_dedupe <= r.bytes_before_dedupe);
        assert!(r.distinct_payloads >= 1 && r.distinct_payloads <= r.tiles);
        assert!(r.bytes_after_dedupe > 0);
    }
    let r = rows()
        .into_iter()
        .find(|r| r.format == "png" && r.level == 0)
        .unwrap();
    assert!(
        r.distinct_payloads < r.tiles,
        "the blank tiles of the control share one payload"
    );
}

#[test]
fn timing_is_reported_at_p50_and_p99() {
    for r in rows() {
        assert!(r.encode_ms_p50 >= 0.0 && r.encode_ms_p99 >= r.encode_ms_p50);
        assert!(r.cpu_secs > 0.0);
    }
}

#[test]
fn the_uncommitted_classes_are_named_blocked_not_faked() {
    let classes = content_classes();
    let names: Vec<_> = classes.iter().map(|c| c.0).collect();
    assert_eq!(names, ["cad_vector", "scan", "photo", "synthetic_control"]);
    for (name, status) in classes {
        match (name, status) {
            ("synthetic_control", ClassStatus::Measured) => {}
            ("synthetic_control", _) => panic!("the control is committed code"),
            (_, ClassStatus::Blocked(why)) => assert!(!why.is_empty(), "{name} says why"),
            (_, ClassStatus::Measured) => panic!("{name} has no committed corpus"),
        }
    }
    assert!(rows().iter().all(|r| r.class == "synthetic_control"));
}
