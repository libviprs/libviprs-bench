//! #125: fidelity metrics that do not lie on CAD linework.
//!
//! Each test names the wrong implementation it goes red against.

use libviprs_bench::format::fidelity::{
    EdgeAxis, edge_displacement, edge_position, exact_match_rate, gradient_mask, ink_iou, luma,
};

const W: usize = 40;
const H: usize = 24;

/// A grey field with one vertical black line `width` px wide at column `x0`.
fn line_tile(field: u8, x0: usize, width: usize) -> Vec<u8> {
    let mut v = vec![field; W * H];
    for y in 0..H {
        for x in x0..x0 + width {
            v[y * W + x] = 0;
        }
    }
    v
}

/// A vertical step at continuous position `t`, area-sampled: pixel i covers
/// [i, i+1), dark on the left.
fn step_tile(t: f64, dark: u8, light: u8) -> Vec<u8> {
    let mut v = vec![0u8; W * H];
    for y in 0..H {
        for x in 0..W {
            let covered = (t - x as f64).clamp(0.0, 1.0);
            v[y * W + x] = (covered * dark as f64 + (1.0 - covered) * light as f64).round() as u8;
        }
    }
    v
}

#[test]
fn luma_is_rec601_and_rounds() {
    assert_eq!(luma(&[255, 255, 255]), vec![255]);
    assert_eq!(luma(&[0, 0, 0]), vec![0]);
    assert_eq!(luma(&[255, 0, 0]), vec![76]);
    assert_eq!(luma(&[0, 255, 0]), vec![150]);
}

// Goes red against: a rate computed per byte, which scores a pixel that is wrong
// in one channel as two thirds right.
#[test]
fn exact_match_counts_whole_pixels() {
    let a = vec![10u8, 20, 30, 40, 50, 60];
    assert_eq!(exact_match_rate(&a, &a, 3), 1.0);
    let mut b = a.clone();
    b[1] = 21;
    assert_eq!(exact_match_rate(&a, &b, 3), 0.5);
}

// Goes red against: a rate that panics or returns a partial score on a size
// mismatch, which is how a lossless cell would pass by accident.
#[test]
fn exact_match_refuses_a_length_mismatch() {
    assert_eq!(exact_match_rate(&[1, 2, 3], &[1, 2], 1), 0.0);
    assert_eq!(exact_match_rate(&[], &[], 3), 0.0);
}

// Goes red against: a mask that is the whole tile, or only the gradient pixels
// themselves with no margin.
#[test]
fn the_mask_is_the_gradient_plus_its_margin_and_nothing_else() {
    let t = line_tile(200, 20, 1);
    let m = gradient_mask(&t, W, H, 16, 2);
    let row = 5;
    let on: Vec<usize> = (0..W).filter(|&x| m[row * W + x]).collect();
    // The line is column 20; its neighbours 19 and 21 differ by 200; two pixels
    // of margin beyond the changed pixels reaches 17..=23.
    assert_eq!(on, (17..=23).collect::<Vec<_>>());
}

// Goes red against: the naive metric the panel reported 25 px and 166 px from.
// The left of the tile sits one level either side of the ink threshold, with a
// step of 8 to the rest (under the 16 that counts as a gradient). A lossy codec
// that nudges those flat pixels across the threshold must not move a score
// that is only about where the ink is.
fn near_threshold_tile(left: u8, right: u8) -> Vec<u8> {
    let mut v = vec![right; W * H];
    for y in 0..H {
        for x in 0..10 {
            v[y * W + x] = left;
        }
        for x in 30..32 {
            v[y * W + x] = 0;
        }
    }
    v
}

#[test]
fn flat_regions_near_the_threshold_do_not_count() {
    let reference = near_threshold_tile(127, 135);
    let candidate = near_threshold_tile(129, 137); // the flat left crossed threshold 128
    let mask = gradient_mask(&reference, W, H, 16, 2);

    let everywhere = vec![true; W * H];
    let naive = ink_iou(&reference, &candidate, &everywhere, 128).unwrap();
    assert!(naive < 0.2, "the naive score is the nonsense being guarded against: {naive}");

    let masked = ink_iou(&reference, &candidate, &mask, 128).unwrap();
    assert_eq!(masked, 1.0, "the masked score only sees the line");
}

// Goes red against: a mask so generous the shift disappears, or an IoU that is
// always 1.0.
#[test]
fn a_shifted_line_lowers_the_masked_iou() {
    let reference = line_tile(255, 20, 2);
    let shifted = line_tile(255, 21, 2);
    let mask = gradient_mask(&reference, W, H, 16, 2);
    let iou = ink_iou(&reference, &shifted, &mask, 128).unwrap();
    assert!((iou - 1.0 / 3.0).abs() < 1e-9, "2px line shifted by 1px overlaps 1 of 3: {iou}");
    assert_eq!(ink_iou(&reference, &reference, &mask, 128), Some(1.0));
}

// Goes red against: returning 1.0 for two blank tiles, which is a claim.
#[test]
fn no_ink_in_the_mask_has_no_iou() {
    let blank = vec![255u8; W * H];
    let mask = gradient_mask(&blank, W, H, 16, 2);
    assert_eq!(ink_iou(&blank, &blank, &mask, 128), None);
}

// Goes red against: a centroid taken over pixel centres (off by half a pixel) or
// one that snaps to the nearest pixel.
#[test]
fn the_edge_is_found_at_subpixel_truth() {
    for t in [10.37, 10.0, 15.75, 3.5] {
        let tile = step_tile(t, 0, 255);
        let got = edge_position(&tile, W, H, EdgeAxis::X).unwrap();
        assert!((got - t).abs() < 0.01, "edge at {t} found at {got}");
    }
}

#[test]
fn displacement_is_measured_against_truth_not_against_another_codec() {
    let tile = step_tile(10.87, 0, 255);
    let d = edge_displacement(&tile, W, H, EdgeAxis::X, 10.37).unwrap();
    assert!((d - 0.5).abs() < 0.01, "{d}");
}

#[test]
fn the_y_axis_is_the_transpose() {
    let mut t = vec![255u8; W * H];
    for y in 0..H {
        for x in 0..W {
            let covered = (7.25 - y as f64).clamp(0.0, 1.0);
            t[y * W + x] = ((1.0 - covered) * 255.0).round() as u8;
        }
    }
    let got = edge_position(&t, W, H, EdgeAxis::Y).unwrap();
    assert!((got - 7.25).abs() < 0.01, "{got}");
}

// Goes red against: a displacement of 0.0 for a tile with no edge.
#[test]
fn a_tile_with_no_edge_reports_none_not_zero() {
    let flat = vec![200u8; W * H];
    assert_eq!(edge_position(&flat, W, H, EdgeAxis::X), None);
    assert_eq!(edge_displacement(&flat, W, H, EdgeAxis::X, 10.0), None);
}
