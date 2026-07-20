//! Source-level guard for the rasterized-PDF scalability series (#31).
//!
//! Cheap CI check in the style of `tests/vips_ffi.rs` and
//! `tests/pdfium_provenance.rs`: it fails the moment the scalability binary
//! stops wiring the real-content PDF series, without needing pdfium linked or
//! a multi-minute bench run. The series itself is exercised end-to-end by
//! `tests/pdf_fixture.rs::pdf_streaming_workload_produces_valid_pyramid`.

const SCALABILITY: &str = include_str!("../src/scalability.rs");

/// The PDF workload must be emitted as its OWN chart series, kept apart from
/// the four synthetic-gradient engine series, so the comparison covers real
/// content (#31).
#[test]
fn scalability_emits_a_separate_rasterized_pdf_series() {
    assert!(
        SCALABILITY.contains("streaming-pdf"),
        "scalability must emit a distinct 'streaming-pdf' series alongside the \
         synthetic gradient (see #31)"
    );
}

/// The series must rasterize the committed fixture through the pdfium
/// streaming path (`PdfiumStripSource` via `bench_streaming_pdf`), never a
/// re-materialized full page.
#[test]
fn pdf_series_is_sourced_from_the_committed_fixture_via_pdfium() {
    assert!(
        SCALABILITY.contains("bench_streaming_pdf"),
        "the PDF series must run through libviprs_bench::bench_streaming_pdf \
         (PdfiumStripSource streaming path)"
    );
    assert!(
        SCALABILITY.contains("cc_licenses_mapping.pdf"),
        "the PDF series must rasterize the committed fixture (see #30)"
    );
}

/// The real-content series must stay behind the existing feature gate so a
/// default build is byte-for-byte unaffected (#31).
#[test]
fn pdf_series_is_behind_the_pdfium_feature_gate() {
    assert!(
        SCALABILITY.contains("feature = \"pdfium\""),
        "the rasterized-PDF workload must be gated behind the pdfium feature"
    );
}

/// Structural guard (issue #22 review): the earlier tests match substrings that
/// also occur in doc/NOTE comments, so deleting the actual wiring while leaving
/// the explanatory prose would keep them green. This one couples to the *call
/// site* (`run_pdf_streaming`) AND the push of its result into `all_points` —
/// neither of which appears in a comment — so the series must really be
/// measured and emitted, not merely described.
#[test]
fn pdf_series_is_actually_wired_into_the_emitted_points() {
    assert!(
        SCALABILITY.contains("run_pdf_streaming("),
        "the PDF series must be produced by a call to run_pdf_streaming"
    );
    assert!(
        SCALABILITY.contains("all_points.push(p)"),
        "the run_pdf_streaming result must be pushed into all_points so it \
         reaches the charts and JSON"
    );
}
