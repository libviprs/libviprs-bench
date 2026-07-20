//! Real-content PDF fixture guards (#22, sub-issues #30 / #31).
//!
//! `fixtures/cc_licenses_mapping.pdf` is the first committed real-content
//! benchmark workload: a small, single-page, CC0 vector infographic fetched
//! from Wikimedia Commons (see `fixtures/PROVENANCE.md`). These tests pin its
//! integrity and prove it drives the pdfium rasterization + streaming-pyramid
//! path the scalability `streaming-pdf` series uses:
//!
//! 1. the committed bytes still hash to the recorded SHA-256, and the human
//!    provenance note records that same digest, source, and license;
//! 2. the fixture rasterizes through pdfium to the expected pixel dimensions
//!    and a non-blank raster;
//! 3. the rasterized-PDF workload runs end-to-end at a tiny size and produces
//!    a valid DeepZoom pyramid.
//!
//! The pdfium-dependent tests (2, 3) are gated behind the `pdfium` feature so
//! a default (no-pdfium) build still compiles and passes; the checksum and
//! provenance guards (1) run unconditionally.

use sha2::{Digest, Sha256};

/// The committed fixture, embedded so the checksum guard needs no filesystem
/// and so a missing fixture is a compile error, not a silent skip.
const FIXTURE: &[u8] = include_bytes!("../fixtures/cc_licenses_mapping.pdf");

/// SHA-256 of the committed fixture. Recorded here and in
/// `fixtures/PROVENANCE.md`; the two must never drift (see
/// [`provenance_note_pins_source_license_and_checksum`]). Mirrors how
/// `tests/pdfium_provenance.rs` pins the PDFium binary digests.
const FIXTURE_SHA256: &str = "6012f1c07704f27014737da1585dd7780e215ae6e6df27a3804d4aacfa80db0d";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Fixture path on disk — pdfium reads from a path, not from bytes.
#[cfg(feature = "pdfium")]
fn fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("cc_licenses_mapping.pdf")
}

#[test]
fn committed_fixture_matches_recorded_checksum() {
    // Guards both that the fixture is committed (else `include_bytes!` fails to
    // compile) and that its bytes still hash to the recorded digest.
    assert!(!FIXTURE.is_empty(), "fixture must be committed and non-empty");
    let digest = hex(&Sha256::digest(FIXTURE));
    assert_eq!(
        digest, FIXTURE_SHA256,
        "committed fixture SHA-256 drifted from the recorded checksum; if the \
         fixture was intentionally replaced, update FIXTURE_SHA256 and \
         fixtures/PROVENANCE.md together"
    );
}

#[test]
fn provenance_note_pins_source_license_and_checksum() {
    // The human-readable note must record the enforced digest, the freely-
    // licensed source, and the license — mirroring how `tests/pdfium_provenance`
    // asserts the pinned digests appear in the Dockerfile.
    const NOTE: &str = include_str!("../fixtures/PROVENANCE.md");
    assert!(
        NOTE.contains(FIXTURE_SHA256),
        "PROVENANCE.md must record the same SHA-256 the test enforces"
    );
    assert!(
        NOTE.contains("commons.wikimedia.org") || NOTE.contains("upload.wikimedia.org"),
        "PROVENANCE.md must record the source URL"
    );
    assert!(
        NOTE.contains("CC0"),
        "PROVENANCE.md must record the (CC0) license"
    );
}

/// (b) The fixture rasterizes through the pdfium streaming path to the
/// expected pixel dimensions and a non-blank raster.
#[cfg(feature = "pdfium")]
#[test]
fn fixture_rasterizes_to_expected_dimensions_and_nonblank() {
    use libviprs::{PdfiumStripSource, StripSource};

    // The streaming source derives dimensions arithmetically from the page's
    // point size (1190.52 × 841.861 pt) at the requested DPI — `floor(pts ×
    // dpi/72)` — so these are stable across pdfium builds.
    let src = PdfiumStripSource::new_streaming(fixture_path(), 1, 72).expect(
        "pdfium must open the committed fixture (set PDFIUM_PATH if libpdfium \
         is not on the system library path)",
    );
    assert_eq!(
        (src.width(), src.height()),
        (1190, 841),
        "72-DPI raster dimensions"
    );

    // Doubling DPI doubles the raster — the mechanism the scalability sweep
    // uses to drive the fixture to progressively larger sizes.
    let src2 =
        PdfiumStripSource::new_streaming(fixture_path(), 1, 144).expect("open fixture at 144 DPI");
    assert_eq!(
        (src2.width(), src2.height()),
        (2381, 1683),
        "144-DPI raster dimensions"
    );

    // Non-blank: the colored infographic must render pixel variation, not a
    // uniform (blank) fill.
    let strip = src
        .render_strip(0, src.height())
        .expect("render the full-page strip");
    let data = strip.data();
    assert!(!data.is_empty(), "raster must have pixels");
    let first = data[0];
    assert!(
        data.iter().any(|&b| b != first),
        "rasterized fixture must be non-blank (found a uniform buffer)"
    );
}

/// (c) The rasterized-PDF workload runs end-to-end at a tiny size and produces
/// a valid DeepZoom pyramid — one cell of the `streaming-pdf` scalability
/// series.
#[cfg(feature = "pdfium")]
#[test]
fn pdf_streaming_workload_produces_valid_pyramid() {
    // Tiny DPI keeps the raster small (~397 × 280) and the test fast.
    let metrics = libviprs_bench::bench_streaming_pdf(
        &fixture_path(),
        1,
        24,
        256,
        1,
        4_000_000,
        "pdf_smoke",
    )
    .expect("PDF streaming workload must run end-to-end");

    assert_eq!(
        metrics.engine, "streaming-pdf",
        "the real-content workload must report its own engine series"
    );
    assert!(
        metrics.width > 0 && metrics.height > 0,
        "raster dimensions must be recorded, got {}x{}",
        metrics.width,
        metrics.height
    );
    assert!(
        metrics.tiles_produced > 0,
        "workload must emit a non-empty pyramid, got {} tiles",
        metrics.tiles_produced
    );
    assert!(
        !metrics.per_level_tiles.is_empty(),
        "per-level grid must be populated"
    );
    assert_eq!(
        metrics.per_level_tiles.iter().sum::<u64>(),
        metrics.tiles_produced,
        "per-level tiles must sum to the total"
    );
    assert!(
        metrics.strips > 0,
        "the streaming path must render at least one strip"
    );
}
