//! Host load-average + thermal-throttle provenance guard (#25).
//!
//! A wall-time number measured while the host was under load — or while the CPU
//! was thermally throttled — is slower for reasons that have nothing to do with
//! the code under test. [`Provenance`] therefore samples the host load average
//! (and, where cheaply available, a thermal-throttle indicator) at capture time,
//! so a loaded or throttled measurement run is flagged in the snapshot rather
//! than silently polluting a cross-version delta.
//!
//! Both axes are additive with a serde default, so a `benchmark_history.json`
//! written before #25 (no such fields) still deserializes.

use libviprs_bench::provenance::{LoadAverage, Provenance};

/// `capture()` records the host load average on the platforms the benchmarks
/// run on (the Linux container and the macOS host): three finite, non-negative
/// numbers read from `/proc/loadavg` (Linux) or `getloadavg` (macOS).
#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn capture_records_a_host_load_average() {
    let prov = Provenance::capture();
    let la = prov
        .load_average
        .expect("load average must be recorded on linux/macos");
    for v in [la.one_min, la.five_min, la.fifteen_min] {
        assert!(
            v.is_finite() && v >= 0.0,
            "each load component is finite and non-negative, got {v}"
        );
    }
}

/// A snapshot's load-average + thermal fields serde round-trip unchanged.
#[test]
fn load_and_thermal_round_trip() {
    let mut prov = Provenance::capture();
    prov.load_average = Some(LoadAverage {
        one_min: 3.5,
        five_min: 2.0,
        fifteen_min: 1.25,
    });
    prov.thermal_throttle_count = Some(7);

    let json = serde_json::to_string(&prov).unwrap();
    let back: Provenance = serde_json::from_str(&json).unwrap();

    assert_eq!(
        back.load_average,
        Some(LoadAverage {
            one_min: 3.5,
            five_min: 2.0,
            fifteen_min: 1.25
        })
    );
    assert_eq!(back.thermal_throttle_count, Some(7));
}

/// Legacy provenance written before these axes existed still deserializes: the
/// new fields serde-default to `None`, so an old history file keeps loading
/// (the same forward-compatibility the pinned-oracle axis already enjoys).
#[test]
fn legacy_provenance_without_load_or_thermal_deserializes() {
    let legacy = r#"{
        "libvips_version": "8.18.4",
        "pinned_libvips_version": "8.18.4",
        "rustc_version": "rustc 1.89.0",
        "build_profile": "release",
        "build_flags": "-C target-cpu=native",
        "host": {
            "cpu_model": "Apple M1 Max",
            "ncpu": 10,
            "arch": "aarch64",
            "os": "macos",
            "in_container": false
        }
    }"#;

    let prov: Provenance =
        serde_json::from_str(legacy).expect("legacy provenance must still parse");
    assert_eq!(
        prov.load_average, None,
        "absent load average defaults to None"
    );
    assert_eq!(
        prov.thermal_throttle_count, None,
        "absent thermal indicator defaults to None"
    );
    // The pre-existing axes still populate.
    assert_eq!(prov.libvips_version, "8.18.4");
    assert_eq!(prov.host.ncpu, 10);
}

/// The centralised measurement-condition warnings (#25 review): one source of
/// wording for the `report` and `scalability` binaries so their stderr guards
/// can never drift again. A clean provenance warns about nothing; a contended,
/// thermally-throttled, mispinned one emits exactly the three lines, in order.
#[test]
fn measurement_condition_warnings_are_centralised() {
    // Clean: idle (no load average), no thermal counter, oracle indeterminate
    // (both versions "unknown") — nothing to warn about.
    let clean = Provenance::default();
    assert!(
        clean.measurement_condition_warnings().is_empty(),
        "a clean run warns about nothing"
    );

    // Contended + throttled + mispinned — all three guards trip, in order.
    let mut dirty = Provenance::default();
    dirty.host.ncpu = 4;
    dirty.load_average = Some(LoadAverage {
        one_min: 8.0,
        five_min: 6.0,
        fifteen_min: 5.0,
    });
    dirty.thermal_throttle_count = Some(3);
    dirty.libvips_version = "8.16.0".to_string();
    dirty.pinned_libvips_version = "8.18.4".to_string();

    let warnings = dirty.measurement_condition_warnings();
    assert_eq!(
        warnings.len(),
        3,
        "contention + thermal + oracle mismatch, got: {warnings:?}"
    );
    assert!(
        warnings[0].contains("host load") && warnings[0].contains(">= 4 CPUs"),
        "first line is the contention guard: {}",
        warnings[0]
    );
    assert!(
        warnings[1].contains("thermal-throttle"),
        "second line is the thermal guard: {}",
        warnings[1]
    );
    assert!(
        warnings[2].contains("8.16") && warnings[2].contains("8.18") && warnings[2].contains("#33"),
        "third line is the mismatched-oracle guard: {}",
        warnings[2]
    );
}

/// Load average and thermal state are per-run measurement *conditions*, not part
/// of the environment identity: two runs on the same box under different loads
/// must still group together, so the fingerprint must not vary with them —
/// mirroring how the pinned-oracle axis is deliberately kept out of the
/// fingerprint.
#[test]
fn load_and_thermal_are_not_in_the_fingerprint() {
    let mut idle = Provenance::capture();
    let mut loaded = idle.clone();
    idle.load_average = Some(LoadAverage {
        one_min: 0.1,
        five_min: 0.1,
        fifteen_min: 0.1,
    });
    idle.thermal_throttle_count = Some(0);
    loaded.load_average = Some(LoadAverage {
        one_min: 40.0,
        five_min: 39.0,
        fifteen_min: 38.0,
    });
    loaded.thermal_throttle_count = Some(999);
    assert_eq!(
        idle.fingerprint(),
        loaded.fingerprint(),
        "an idle and a loaded run on the same box share one environment fingerprint"
    );
}
