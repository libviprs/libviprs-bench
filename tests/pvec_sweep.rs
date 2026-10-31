//! The PVec row of the storage family (libviprs/libviprs#1331): one scan generated into PMTiles,
//! PVec, GDAL (COG) and libvips (zip of PNG tiles), measured in fresh processes, attested from
//! what each wrote, and filed in the storage envelope. Needs `--features pvec`.
//!
//! The contenders that need an external tool (GDAL, libvips) are skipped with their reason when the
//! tool is not on the machine, so this runs anywhere; the container stage has both.

#![cfg(feature = "pvec")]

use libviprs_bench::storage::pvec_sweep::{self, Contender, ScanCell};

fn scratch(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir()
        .join("libviprs-bench-pvec-sweep")
        .join(format!("{}_{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_four_contenders_are_named_and_round_trip() {
    let labels: Vec<_> = Contender::ALL.iter().map(|c| c.label()).collect();
    assert_eq!(labels, ["pmtiles", "pvec", "gdal-cog", "libvips-zip"]);
    for c in Contender::ALL {
        assert_eq!(Contender::parse(c.label()), Some(c));
    }
    assert_eq!(Contender::parse("go-pmtiles"), None);
}

#[test]
fn the_cells_are_the_a0_fixture_and_a_small_one_for_ci() {
    let a0 = ScanCell::a0();
    assert_eq!((a0.width, a0.height), (18_720, 13_240));
    assert_eq!(a0.tiles(), 1_308, "512 px tiles, grid cells only (no Google padding)");
    assert_eq!(ScanCell::parse(&a0.spec()), Some(a0));
    assert_eq!(ScanCell::parse(&ScanCell::ci().spec()), Some(ScanCell::ci()));
    assert!(ScanCell::ci().tiles() > 1);
}

#[test]
fn ours_write_the_same_png_tiles_so_neither_has_a_codec_advantage() {
    // The encoding claim of tests/encoding_claim.rs, extended to the PVec row: the PNG tiles in the
    // PVec artifact are byte for byte the PNG tiles in the PMTiles archive.
    let cell = ScanCell::ci();
    let (pm_dir, pv_dir) = (scratch("claim-pm"), scratch("claim-pv"));
    pvec_sweep::one_rep(Contender::Pmtiles, cell, &pm_dir, &[]).unwrap();
    pvec_sweep::one_rep(Contender::Pvec, cell, &pv_dir, &[]).unwrap();
    let (pm_tiles, pv_tiles) = pvec_sweep::png_tiles_of(&pm_dir, &pv_dir, cell).unwrap();
    assert!(!pv_tiles.is_empty());
    for (key, bytes) in &pv_tiles {
        assert_eq!(&bytes[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a], "{key:?}");
        assert_eq!(pm_tiles.get(key), Some(bytes), "{key:?} differs between the sinks");
    }
}

#[test]
fn an_attested_rep_reads_back_from_what_was_written_and_a_damaged_one_is_refused() {
    let cell = ScanCell::ci();
    for c in [Contender::Pmtiles, Contender::Pvec] {
        let dir = scratch(&format!("attest-{}", c.label()));
        let out = pvec_sweep::one_rep(c, cell, &dir, &[]).unwrap();
        pvec_sweep::attest(c, cell, &dir, &out).unwrap();
        assert_eq!(out.tiles, Some(cell.tiles()));
        // Damage the artifact: truncate it. The attestation reads the file, not the report.
        let file = dir.join(pvec_sweep::artifact_name(c));
        let bytes = std::fs::read(&file).unwrap();
        std::fs::write(&file, &bytes[..bytes.len() / 2]).unwrap();
        assert!(pvec_sweep::attest(c, cell, &dir, &out).is_err(), "{c:?} accepted a truncated artifact");
    }
}

#[test]
fn a_contender_whose_tool_is_missing_is_skipped_with_the_reason_not_dropped() {
    let r = pvec_sweep::Rival::locate_with(Contender::GdalCog, Some("/nonexistent/gdal_translate"));
    let why = r.unwrap_err();
    assert!(why.contains("gdal_translate") && why.contains("BENCH_GDAL_TRANSLATE"), "{why}");
    assert!(pvec_sweep::Rival::locate_with(Contender::Pvec, None).is_err_and(|e| e.contains("not external")));
}

#[test]
fn a_ci_sweep_has_a_row_set_for_every_contender_with_host_and_arch_on_the_document() {
    // The binary, because a sweep re-executes itself for every repetition.
    let out = scratch("sweep").join("pvec-results.json");
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_pvec-sink"))
        .args(["--profile", "ci", "--out"])
        .arg(&out)
        .env("STORAGE_ALLOW_DIRTY", "1")
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let cells = json["cells"].as_array().unwrap();
    for c in Contender::ALL {
        let rows: Vec<_> = cells.iter().filter(|r| r["backend"] == c.label()).collect();
        assert!(!rows.is_empty(), "{} has no rows", c.label());
        for r in rows {
            assert_eq!(r["scenario"], "generate_scan");
            let outcome = r["outcome"].as_str().unwrap();
            if matches!(c, Contender::Pmtiles | Contender::Pvec) {
                assert_eq!(outcome, "ok", "{r}");
                assert_eq!(r["attested"], true);
            } else {
                assert!(outcome == "ok" || outcome == "skipped", "{r}");
                if outcome == "skipped" {
                    assert!(r["reason"].as_str().is_some_and(|s| !s.is_empty()));
                }
            }
        }
    }
    let metrics: std::collections::BTreeSet<_> =
        cells.iter().map(|r| r["metric"].as_str().unwrap().to_string()).collect();
    for m in ["wall", "throughput", "peak_rss", "output_bytes"] {
        assert!(metrics.contains(m), "{m} is not published: {metrics:?}");
    }
    assert!(json["provenance"]["arch"].as_str().is_some_and(|a| !a.is_empty()));
    assert!(json["provenance"]["os"].as_str().is_some_and(|a| !a.is_empty()));
    // The arm64 run is recorded as not done, never fudged.
    let notes = json["provenance"]["invocation"]["resolved"]["notRun"].to_string();
    assert!(notes.contains("arm64"), "{notes}");
}
