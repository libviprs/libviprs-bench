//! Fidelity metrics for comparing a lossy tile to its lossless source (#125).
//!
//! # Why not PSNR or SSIM
//!
//! The panel behind #102 found two cells that break both:
//!
//! * on a sparse CAD tile PSNR is **non-monotonic in quality**: q85 scores
//!   67.44 dB and q95 scores 65.33;
//! * on coloured linework PSNR is **flat across the quality range**: 25.91 dB
//!   at q75 and 26.05 at q100, while the file grows 83%.
//!
//! A size-versus-PSNR curve reads both as "no difference", so this module does
//! not offer one. It offers the three things the decision rule needs: whether the
//! pixels are exactly the same, whether the *ink* is in the same place, and where
//! a known edge actually landed.
//!
//! # The gradient mask is not optional
//!
//! A naive threshold metric reported 25 px of "edge displacement" on a dense tile
//! and 166 px on a photograph, both artefacts of flipped pixels in flat regions
//! whose luma already sat within a level of the threshold. Every threshold metric
//! here is therefore evaluated only inside [`gradient_mask`], which is pixels
//! within a radius of a real intensity gradient on the *reference*.

/// Rec. 601 luma of an interleaved RGB8 buffer, rounded to the nearest level.
pub fn luma(rgb: &[u8]) -> Vec<u8> {
    let _ = rgb;
    unimplemented!()
}

/// Fraction of pixels whose every channel is identical. A length mismatch is
/// `0.0`, never a panic and never a partial score. Every lossless cell must
/// score exactly `1.0` or the run is invalid.
pub fn exact_match_rate(reference: &[u8], candidate: &[u8], channels: usize) -> f64 {
    let _ = (reference, candidate, channels);
    unimplemented!()
}

/// Pixels within `radius` (Chebyshev) of a real intensity gradient in `luma`: a
/// pixel is on a gradient when it differs from a 4-neighbour by at least
/// `min_step` levels.
pub fn gradient_mask(luma: &[u8], w: usize, h: usize, min_step: u8, radius: usize) -> Vec<bool> {
    let _ = (luma, w, h, min_step, radius);
    unimplemented!()
}

/// Intersection over union of the ink (`luma < threshold`) of two tiles,
/// counted only where `mask` is set. `None` when neither tile has ink in the
/// mask, because an empty union has no IoU and `1.0` would be a claim.
pub fn ink_iou(reference: &[u8], candidate: &[u8], mask: &[bool], threshold: u8) -> Option<f64> {
    let _ = (reference, candidate, mask, threshold);
    unimplemented!()
}

/// Which way a synthetic edge runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeAxis {
    /// A vertical line: its position is an x coordinate.
    X,
    /// A horizontal line: its position is a y coordinate.
    Y,
}

/// The subpixel position of the single edge in a synthetic control tile, as the
/// gradient-weighted centroid of the luma step, averaged over every row (or
/// column). Pixel `i` covers `[i, i+1)`, so an area-sampled step at `t` lands at
/// `t`. `None` when the tile has no edge to find.
pub fn edge_position(luma: &[u8], w: usize, h: usize, axis: EdgeAxis) -> Option<f64> {
    let _ = (luma, w, h, axis);
    unimplemented!()
}

/// How far the edge landed from known truth, in pixels. `None` when no edge is
/// found, which is a failure to report and not a displacement of zero.
pub fn edge_displacement(
    candidate: &[u8],
    w: usize,
    h: usize,
    axis: EdgeAxis,
    truth: f64,
) -> Option<f64> {
    edge_position(candidate, w, h, axis).map(|p| (p - truth).abs())
}
