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
    for v in [la.one, la.five, la.fifteen] {
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
        one: 3.5,
        five: 2.0,
        fifteen: 1.25,
    });
    prov.thermal_throttle_count = Some(7);

    let json = serde_json::to_string(&prov).unwrap();
    let back: Provenance = serde_json::from_str(&json).unwrap();

    assert_eq!(
        back.load_average,
        Some(LoadAverage {
            one: 3.5,
            five: 2.0,
            fifteen: 1.25
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

    let prov: Provenance = serde_json::from_str(legacy).expect("legacy provenance must still parse");
    assert_eq!(prov.load_average, None, "absent load average defaults to None");
    assert_eq!(
        prov.thermal_throttle_count, None,
        "absent thermal indicator defaults to None"
    );
    // The pre-existing axes still populate.
    assert_eq!(prov.libvips_version, "8.18.4");
    assert_eq!(prov.host.ncpu, 10);
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
        one: 0.1,
        five: 0.1,
        fifteen: 0.1,
    });
    idle.thermal_throttle_count = Some(0);
    loaded.load_average = Some(LoadAverage {
        one: 40.0,
        five: 39.0,
        fifteen: 38.0,
    });
    loaded.thermal_throttle_count = Some(999);
    assert_eq!(
        idle.fingerprint(),
        loaded.fingerprint(),
        "an idle and a loaded run on the same box share one environment fingerprint"
    );
}
