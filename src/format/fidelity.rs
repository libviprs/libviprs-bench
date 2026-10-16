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
    let (pixels, _) = rgb.as_chunks::<3>();
    pixels
        .iter()
        .map(|p| ((299 * p[0] as u32 + 587 * p[1] as u32 + 114 * p[2] as u32 + 500) / 1000) as u8)
        .collect()
}

/// Fraction of pixels whose every channel is identical. A length mismatch is
/// `0.0`, never a panic and never a partial score. Every lossless cell must
/// score exactly `1.0` or the run is invalid.
pub fn exact_match_rate(reference: &[u8], candidate: &[u8], channels: usize) -> f64 {
    if channels == 0 || reference.len() != candidate.len() || reference.is_empty() {
        return 0.0;
    }
    let pixels = reference.len() / channels;
    if pixels == 0 {
        return 0.0;
    }
    let same = reference
        .chunks_exact(channels)
        .zip(candidate.chunks_exact(channels))
        .filter(|(a, b)| a == b)
        .count();
    same as f64 / pixels as f64
}

/// Pixels within `radius` (Chebyshev) of a real intensity gradient in `luma`: a
/// pixel is on a gradient when it differs from a 4-neighbour by at least
/// `min_step` levels.
pub fn gradient_mask(luma: &[u8], w: usize, h: usize, min_step: u8, radius: usize) -> Vec<bool> {
    let mut on = vec![false; w * h];
    let diff = |a: u8, b: u8| a.abs_diff(b);
    for y in 0..h {
        for x in 0..w {
            let here = luma[y * w + x];
            let step = (x > 0 && diff(here, luma[y * w + x - 1]) >= min_step)
                || (x + 1 < w && diff(here, luma[y * w + x + 1]) >= min_step)
                || (y > 0 && diff(here, luma[(y - 1) * w + x]) >= min_step)
                || (y + 1 < h && diff(here, luma[(y + 1) * w + x]) >= min_step);
            on[y * w + x] = step;
        }
    }
    let mut out = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            if !on[y * w + x] {
                continue;
            }
            for yy in y.saturating_sub(radius)..=(y + radius).min(h - 1) {
                for xx in x.saturating_sub(radius)..=(x + radius).min(w - 1) {
                    out[yy * w + xx] = true;
                }
            }
        }
    }
    out
}

/// Intersection over union of the ink (`luma < threshold`) of two tiles,
/// counted only where `mask` is set. `None` when neither tile has ink in the
/// mask, because an empty union has no IoU and `1.0` would be a claim.
pub fn ink_iou(reference: &[u8], candidate: &[u8], mask: &[bool], threshold: u8) -> Option<f64> {
    let (mut inter, mut union) = (0u64, 0u64);
    for ((&r, &c), &m) in reference.iter().zip(candidate).zip(mask) {
        if !m {
            continue;
        }
        let (ri, ci) = (r < threshold, c < threshold);
        inter += (ri && ci) as u64;
        union += (ri || ci) as u64;
    }
    (union > 0).then(|| inter as f64 / union as f64)
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
    if luma.len() != w * h || w < 2 || h < 2 {
        return None;
    }
    // Lines run along the axis we do not measure: for X, one profile per row.
    let (lines, len) = match axis {
        EdgeAxis::X => (h, w),
        EdgeAxis::Y => (w, h),
    };
    let at = |line: usize, i: usize| match axis {
        EdgeAxis::X => luma[line * w + i] as f64,
        EdgeAxis::Y => luma[i * w + line] as f64,
    };
    let (mut sum, mut found) = (0.0, 0usize);
    for line in 0..lines {
        // The step between pixel i-1 and i sits at boundary i.
        let (mut weight, mut moment) = (0.0, 0.0);
        for i in 1..len {
            let g = (at(line, i) - at(line, i - 1)).abs();
            weight += g;
            moment += g * i as f64;
        }
        if weight >= 1.0 {
            sum += moment / weight;
            found += 1;
        }
    }
    (found > 0).then(|| sum / found as f64)
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
