//! Output-equivalence PSNR/SSIM mid-pyramid tile spot-check (#23, sub-issue #32).
//!
//! The cross-engine equivalence gate used to validate only per-level PNG tile
//! GEOMETRY — the level count and each level's tile grid (see
//! `harness::check_output_equivalence`). An engine that emitted the right
//! *number* of correctly-sized tiles filled with the *wrong pixels* therefore
//! sailed through the gate, and its (bogus) timings were reported as if it had
//! done the same work as libvips.
//!
//! This adds a pixel-level spot-check: decode a few mid-pyramid tiles from the
//! libvips `dzsave` reference and the corresponding libviprs engine tiles and
//! assert their PSNR clears a documented near-lossless threshold
//! ([`MIN_TILE_PSNR_DB`]).
//!
//! Coverage:
//!   * pure unit tests of the self-contained [`psnr`] / [`ssim`] helpers on
//!     known small buffers — no libvips needed;
//!   * a fixture -> libvips-differential POSITIVE test: the committed fixture
//!     image, tiled by both engines, clears the threshold on every compared
//!     mid tile;
//!   * a NEGATIVE test: corrupting one candidate mid tile drops the spot-check
//!     PSNR below the threshold and trips the gate.
//!
//! The libvips-invoking tests are guarded behind [`vips_available`], matching
//! the pattern used by `tests/vips_ffi.rs`.

use std::path::{Path, PathBuf};

use libviprs::{EngineKind, Layout, PixelFormat, PyramidPlanner, Raster};
use libviprs_bench::harness::{
    MIN_TILE_PSNR_DB, PSNR_CLAMP_DB, global_ssim, psnr, spot_check_tile_psnr,
};
use libviprs_bench::{vips_available, write_libviprs_pyramid, write_libvips_pyramid};

/// Committed fixture: a 1024x1024 RGB image (two colour ramps + a coarse 32px
/// checker in blue). At tile size 256 its pyramid has a downsampled 2x2 mid
/// level, which is exactly what the spot-check compares.
const FIXTURE: &[u8] = include_bytes!("fixtures/equivalence_src.png");
const TILE: u32 = 256;

// ---------------------------------------------------------------------------
// Pure unit tests of the helpers (no libvips required)
// ---------------------------------------------------------------------------

#[test]
fn psnr_identical_buffers_clamp_to_the_ceiling() {
    // Identical inputs have zero error, so PSNR is mathematically infinite;
    // the helper clamps it to a finite ceiling so the score stays
    // serializable and averageable. The ceiling sits above the gate threshold
    // by construction (PSNR_CLAMP_DB = 100 dB, MIN_TILE_PSNR_DB = 40 dB), so an
    // identical tile always passes.
    let a = [10u8, 20, 30, 40, 50, 60];
    assert_eq!(psnr(&a, &a), PSNR_CLAMP_DB);
}

#[test]
fn psnr_one_lsb_difference_is_the_expected_finite_value() {
    // Four samples, exactly one off by a single LSB → MSE = 1/4.
    let a = [10u8, 20, 30, 40];
    let b = [10u8, 20, 30, 41];
    let got = psnr(&a, &b);
    // PSNR = 10·log10(255² / MSE) = 10·log10(255² · 4).
    let expected = 10.0 * (255.0f64 * 255.0 * 4.0).log10();
    assert!(got.is_finite());
    assert!((got - expected).abs() < 1e-6, "psnr {got} != {expected}");
    // A single-LSB difference is still comfortably near-lossless.
    assert!(
        got > MIN_TILE_PSNR_DB,
        "a one-LSB difference ({got} dB) should clear the threshold"
    );
}

#[test]
fn psnr_size_mismatch_is_a_definitive_failure() {
    // Differing lengths are a definitive mismatch, not a partial compare.
    assert_eq!(psnr(&[1u8, 2, 3], &[1u8, 2]), 0.0);
}

#[test]
fn ssim_identical_is_one_and_inversion_drops_it() {
    let a: Vec<u8> = (0u32..256).map(|v| v as u8).collect();
    assert!((global_ssim(&a, &a) - 1.0).abs() < 1e-9);
    let inverted: Vec<u8> = a.iter().map(|b| 255 - b).collect();
    assert!(
        global_ssim(&a, &inverted) < 1.0,
        "an inverted buffer must score below a perfect SSIM"
    );
}

// ---------------------------------------------------------------------------
// Fixture -> libvips-differential tests (guarded behind vips availability)
// ---------------------------------------------------------------------------

/// The committed fixture decoded into an in-memory RGB8 raster.
fn fixture_raster() -> Raster {
    let img = image::load_from_memory(FIXTURE)
        .expect("fixture PNG decodes")
        .to_rgb8();
    let (w, h) = img.dimensions();
    Raster::new(w, h, PixelFormat::Rgb8, img.into_raw()).unwrap()
}

/// A pair of on-disk pyramids (libvips reference + libviprs candidate) built
/// from the fixture, with a `Drop` that cleans the temp tree up.
struct Pyramids {
    root: PathBuf,
    reference_files: PathBuf,
    candidate_files: PathBuf,
}

impl Drop for Pyramids {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn build_pyramids(tag: &str) -> Pyramids {
    let raster = fixture_raster();
    let (w, h) = (raster.width(), raster.height());
    let plan = PyramidPlanner::new(w, h, TILE, 0, Layout::DeepZoom)
        .unwrap()
        .plan();

    let root = std::env::temp_dir().join(format!("libviprs_equiv_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    // libviprs (monolithic) pyramid. Budget is irrelevant to the monolithic
    // engine but required by the shared signature.
    let candidate_files = write_libviprs_pyramid(
        &raster,
        &plan,
        EngineKind::Monolithic,
        0,
        1_000_000,
        &root.join("lv"),
    )
    .expect("libviprs monolithic pyramid builds");

    // libvips dzsave from the same pixels: write the decoded raster to a PNG
    // so both engines start from an identical source.
    let png = root.join("src.png");
    let file = std::fs::File::create(&png).unwrap();
    let enc = image::codecs::png::PngEncoder::new(std::io::BufWriter::new(file));
    image::ImageEncoder::write_image(enc, raster.data(), w, h, image::ColorType::Rgb8.into())
        .unwrap();
    let reference_files = write_libvips_pyramid(&png, &root.join("vips"), TILE)
        .expect("vips dzsave should produce a pyramid");

    Pyramids {
        root,
        reference_files,
        candidate_files,
    }
}

/// Overwrite one tile with an inverted (255 - v) copy of itself — a drastic,
/// same-geometry corruption that leaves the tile grid intact but destroys the
/// pixels, exactly the failure the geometry-only gate could not see.
fn corrupt_tile(files: &Path, level: u32, col: u32, row: u32) {
    let path = files.join(format!("{level}/{col}_{row}.png"));
    let img = image::open(&path).expect("tile decodes").to_rgb8();
    let (w, h) = img.dimensions();
    let inverted: Vec<u8> = img.into_raw().iter().map(|b| 255 - b).collect();
    let file = std::fs::File::create(&path).unwrap();
    let enc = image::codecs::png::PngEncoder::new(std::io::BufWriter::new(file));
    image::ImageEncoder::write_image(enc, &inverted, w, h, image::ColorType::Rgb8.into()).unwrap();
}

/// The spot-check builds each libviprs engine's OWN candidate pyramid through
/// [`write_libviprs_pyramid`] under the report's flat 1 MB budget floor. The
/// 1024² fixture's worst-case tile-aligned strip (`1024 × 2·256 × 3 = 1.5 MB`)
/// exceeds that floor, so before #38's sizing was applied inside
/// `write_libviprs_pyramid` the streaming / mapreduce candidate build returned
/// `BudgetExceeded` and every spot-check at >= 1024 px silently degraded to "no
/// score". This guards that both budget-driven engines now build a real
/// candidate under the raw floor. Needs no libvips (only the candidate side).
#[test]
fn budget_driven_candidate_pyramids_build_under_the_report_floor() {
    let raster = fixture_raster();
    let (w, h) = (raster.width(), raster.height());
    assert!(
        w >= 1024,
        "fixture must be large enough to exceed the 1 MB floor"
    );
    let plan = PyramidPlanner::new(w, h, TILE, 0, Layout::DeepZoom)
        .unwrap()
        .plan();
    let root = std::env::temp_dir().join(format!("libviprs_equiv_budget_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    for (engine, tag) in [
        (EngineKind::Streaming, "stream"),
        (EngineKind::MapReduce, "mr"),
    ] {
        let base = write_libviprs_pyramid(&raster, &plan, engine, 0, 1_000_000, &root.join(tag))
            .unwrap_or_else(|e| panic!("{tag} candidate must build under the 1 MB floor, got {e}"));
        let level_dirs = std::fs::read_dir(&base)
            .expect("candidate pyramid root exists")
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .count();
        assert!(level_dirs > 0, "{tag} candidate must emit pyramid levels");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn fixture_tiles_are_output_equivalent_by_psnr() {
    if !vips_available() {
        eprintln!("skipping: vips CLI unavailable");
        return;
    }
    let pair = build_pyramids("pos");
    let check = spot_check_tile_psnr(&pair.reference_files, &pair.candidate_files)
        .expect("the fixture pyramid has a comparable mid-pyramid multi-tile level");

    assert!(
        check.tiles_compared >= 1,
        "the spot-check must compare at least one mid tile"
    );
    assert!(
        check.passes(),
        "libviprs tiles must be near-lossless vs libvips: {check:?}"
    );
    assert!(
        check.min_psnr_db >= MIN_TILE_PSNR_DB,
        "min PSNR {} dB < threshold {} dB",
        check.min_psnr_db,
        MIN_TILE_PSNR_DB
    );
}

#[test]
fn corrupted_tile_trips_the_gate() {
    if !vips_available() {
        eprintln!("skipping: vips CLI unavailable");
        return;
    }
    let pair = build_pyramids("neg");

    // The clean pyramids pass and tell us which candidate level is compared.
    let clean = spot_check_tile_psnr(&pair.reference_files, &pair.candidate_files)
        .expect("mid level comparable");
    assert!(clean.passes(), "clean baseline must pass: {clean:?}");

    // Corrupt one candidate tile at the compared level.
    corrupt_tile(&pair.candidate_files, clean.candidate_level, 0, 0);

    let dirty = spot_check_tile_psnr(&pair.reference_files, &pair.candidate_files)
        .expect("mid level still comparable");
    assert!(
        !dirty.passes(),
        "a corrupted mid tile must trip the gate: {dirty:?}"
    );
    assert!(
        dirty.min_psnr_db < MIN_TILE_PSNR_DB,
        "corrupted min PSNR {} dB should be below the threshold {} dB",
        dirty.min_psnr_db,
        MIN_TILE_PSNR_DB
    );
}
