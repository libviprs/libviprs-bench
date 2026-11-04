//! The `format` family runner (#102): cells keyed on (content class, pyramid level, format).
//!
//! The control is the one content class whose corpus is committed, because it is generated here
//! from known geometry: a white field, a black vertical line whose edges sit at fractional pixel
//! positions, and a coloured horizontal line, area-sampled so the truth is exact at every level.
//! The other three classes need images this repository may not carry, and are named as blocked
//! rather than approximated.

use std::time::Instant;

use libviprs::webp::SaveOptions;
use libviprs::{JpegSubsample, PixelFormat, Raster};

use crate::format::fidelity::{
    EdgeAxis, edge_displacement, exact_match_rate, gradient_mask, ink_iou, luma,
};
use crate::sha256::sha256_hex;
use crate::storage::stats::percentile;

/// Edge length of a tile in pixels.
const TILE: usize = 256;
/// Edge length of the control at level 0.
const SIZE: usize = 1024;
/// Where the left edge of the black line sits at level 0, in pixels (fractional on purpose).
const LINE_LEFT: f64 = 301.37;
/// Width of the black line at level 0.
const LINE_WIDTH: f64 = 6.0;
/// Encodes timed per tile, so p99 is not one sample.
const TIMING_PASSES: usize = 5;

/// A tile codec the family compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// PNG, the baseline.
    Png,
    /// JPEG quality 85 with full-resolution chroma.
    Jpeg85,
    /// JPEG quality 85 with 4:2:0 chroma, the cheap alternative.
    Jpeg85Chroma420,
    /// Lossless WebP through `image-webp`, the one that would ship.
    WebpLossless,
}

impl Codec {
    /// Every codec, in report order.
    pub const ALL: [Codec; 4] = [
        Codec::Png,
        Codec::Jpeg85,
        Codec::Jpeg85Chroma420,
        Codec::WebpLossless,
    ];

    /// The name a row carries.
    pub fn name(self) -> &'static str {
        match self {
            Codec::Png => "png",
            Codec::Jpeg85 => "jpeg85",
            Codec::Jpeg85Chroma420 => "jpeg85-420",
            Codec::WebpLossless => "webp-lossless",
        }
    }

    /// Whether the codec must reproduce the pixels exactly.
    pub fn is_lossless(self) -> bool {
        matches!(self, Codec::Png | Codec::WebpLossless)
    }

    fn encode(self, tile: &Raster) -> Result<Vec<u8>, String> {
        match self {
            Codec::Png => tile.encode_png(6),
            Codec::Jpeg85 => tile.encode_jpeg_options(85, JpegSubsample::Off),
            Codec::Jpeg85Chroma420 => tile.encode_jpeg_options(85, JpegSubsample::On),
            Codec::WebpLossless => tile.encode_webp(SaveOptions::default()),
        }
        .map_err(|e| format!("{}: {e}", self.name()))
    }

    fn decode(self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        let format = match self {
            Codec::Png => image::ImageFormat::Png,
            Codec::Jpeg85 | Codec::Jpeg85Chroma420 => image::ImageFormat::Jpeg,
            Codec::WebpLossless => image::ImageFormat::WebP,
        };
        image::load_from_memory_with_format(bytes, format)
            .map(|i| i.to_rgb8().into_raw())
            .map_err(|e| format!("{}: {e}", self.name()))
    }
}

/// Whether a content class has a committed corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassStatus {
    /// Measured by this runner.
    Measured,
    /// Not measurable yet, and why.
    Blocked(&'static str),
}

/// One cell of the family.
#[derive(Debug, Clone, PartialEq)]
pub struct CellRow {
    /// Content class.
    pub class: &'static str,
    /// Pyramid level, 0 the full resolution.
    pub level: u32,
    /// Codec name.
    pub format: &'static str,
    /// Tiles in the level.
    pub tiles: u64,
    /// Encoded bytes summed over every tile.
    pub bytes_before_dedupe: u64,
    /// Encoded bytes summed over the distinct payloads.
    pub bytes_after_dedupe: u64,
    /// Distinct payloads.
    pub distinct_payloads: u64,
    /// Median encode time per tile.
    pub encode_ms_p50: f64,
    /// 99th percentile encode time per tile.
    pub encode_ms_p99: f64,
    /// Total CPU seconds spent encoding.
    pub cpu_secs: f64,
    /// Fraction of samples identical to the reference.
    pub exact_match_rate: f64,
    /// Ink IoU at threshold 128, restricted to pixels within 2 px of a real gradient.
    pub ink_iou: Option<f64>,
    /// Edge displacement of the black line's left edge against the known truth, in pixels.
    pub edge_displacement: Option<f64>,
}

/// The four content classes and whether each can be measured.
pub fn content_classes() -> Vec<(&'static str, ClassStatus)> {
    vec![
        (
            "cad_vector",
            ClassStatus::Blocked(
                "blueprint.pdf lives in libviprs and carries no licence statement here",
            ),
        ),
        (
            "scan",
            ClassStatus::Blocked("no scanned drawing is committed anywhere"),
        ),
        (
            "photo",
            ClassStatus::Blocked("no photographs chosen, and no licence for any"),
        ),
        ("synthetic_control", ClassStatus::Measured),
    ]
}

/// The control at level 0: RGB8, `SIZE` square, area-sampled against the analytic geometry.
fn control() -> Vec<u8> {
    let (left, right) = (LINE_LEFT, LINE_LEFT + LINE_WIDTH);
    let (top, bottom) = (700.0_f64, 704.0_f64);
    let mut px = vec![255u8; SIZE * SIZE * 3];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let cover = |a: f64, b: f64, p: usize| {
                (b.min(p as f64 + 1.0) - a.max(p as f64)).clamp(0.0, 1.0)
            };
            let ink = cover(left, right, x);
            let red = cover(top, bottom, y);
            let o = (y * SIZE + x) * 3;
            // Black line over white, then a red line over whatever is there.
            let base = (255.0 * (1.0 - ink)).round();
            px[o] = base as u8;
            px[o + 1] = (base * (1.0 - red)).round() as u8;
            px[o + 2] = (base * (1.0 - red)).round() as u8;
        }
    }
    px
}

/// Halve an RGB8 square by a 2x2 box filter.
fn halve(px: &[u8], size: usize) -> Vec<u8> {
    let n = size / 2;
    let mut out = vec![0u8; n * n * 3];
    for y in 0..n {
        for x in 0..n {
            for c in 0..3 {
                let at =
                    |dy: usize, dx: usize| px[((2 * y + dy) * size + 2 * x + dx) * 3 + c] as u32;
                out[(y * n + x) * 3 + c] =
                    ((at(0, 0) + at(0, 1) + at(1, 0) + at(1, 1) + 2) / 4) as u8;
            }
        }
    }
    out
}

fn tile_of(px: &[u8], size: usize, ty: usize, tx: usize) -> Result<Raster, String> {
    let (w, h) = (TILE.min(size - tx * TILE), TILE.min(size - ty * TILE));
    let mut data = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        let o = ((ty * TILE + y) * size + tx * TILE) * 3;
        data.extend_from_slice(&px[o..o + w * 3]);
    }
    Raster::new(w as u32, h as u32, PixelFormat::Rgb8, data).map_err(|e| e.to_string())
}

fn measure(codec: Codec, level: u32, px: &[u8], size: usize) -> Result<CellRow, String> {
    let n = size.div_ceil(TILE);
    let mut rebuilt = vec![0u8; px.len()];
    let (mut before, mut tiles) = (0u64, 0u64);
    let mut distinct = std::collections::BTreeMap::new();
    let (mut times, mut cpu) = (Vec::new(), 0.0);
    for ty in 0..n {
        for tx in 0..n {
            let tile = tile_of(px, size, ty, tx)?;
            let mut bytes = Vec::new();
            let mut best = f64::MAX;
            for _ in 0..TIMING_PASSES {
                let t = Instant::now();
                bytes = codec.encode(&tile)?;
                let s = t.elapsed().as_secs_f64();
                cpu += s;
                best = best.min(s);
            }
            times.push(best * 1000.0);
            before += bytes.len() as u64;
            tiles += 1;
            let back = codec.decode(&bytes)?;
            let w = tile.width() as usize;
            for (y, row) in back.chunks(w * 3).enumerate() {
                let o = ((ty * TILE + y) * size + tx * TILE) * 3;
                rebuilt[o..o + row.len()].copy_from_slice(row);
            }
            distinct
                .entry(sha256_hex(&bytes))
                .or_insert(bytes.len() as u64);
        }
    }
    let (reference, candidate) = (luma(px), luma(&rebuilt));
    let mask = gradient_mask(&reference, size, size, 8, 2);
    // The truth halves with each level, and the left edge sits at LINE_LEFT / 2^level.
    // The line's two edges weigh the same, so the centroid of a band that misses the red line is its centre.
    let truth = (LINE_LEFT + LINE_WIDTH / 2.0) / f64::from(1u32 << level);
    let band_h = 64.min(size / 2);
    Ok(CellRow {
        class: "synthetic_control",
        level,
        format: codec.name(),
        tiles,
        bytes_before_dedupe: before,
        bytes_after_dedupe: distinct.values().sum(),
        distinct_payloads: distinct.len() as u64,
        encode_ms_p50: percentile(&times, 0.5).unwrap_or(0.0),
        encode_ms_p99: percentile(&times, 0.99).unwrap_or(0.0),
        cpu_secs: cpu,
        exact_match_rate: exact_match_rate(px, &rebuilt, 3),
        ink_iou: ink_iou(&reference, &candidate, &mask, 128),
        edge_displacement: edge_displacement(
            &candidate[..size * band_h],
            size,
            band_h,
            EdgeAxis::X,
            truth,
        ),
    })
}

/// Measure the synthetic control over `levels` pyramid levels and every codec.
///
/// # Errors
///
/// A message when a codec fails to encode or decode a tile.
pub fn run_synthetic_control(levels: u32) -> Result<Vec<CellRow>, String> {
    let mut px = control();
    let mut size = SIZE;
    let mut rows = Vec::new();
    for level in 0..levels {
        for codec in Codec::ALL {
            rows.push(measure(codec, level, &px, size)?);
        }
        px = halve(&px, size);
        size /= 2;
    }
    validate(&rows)?;
    Ok(rows)
}

/// A run is invalid when a lossless cell is not exact: the codec is then not the variable.
///
/// # Errors
///
/// A message naming the offending cell.
pub fn validate(rows: &[CellRow]) -> Result<(), String> {
    for r in rows {
        let lossless = Codec::ALL
            .iter()
            .any(|c| c.name() == r.format && c.is_lossless());
        if lossless && r.exact_match_rate != 1.0 {
            return Err(format!(
                "{} level {} is lossless but matched {:.6}: the run is invalid",
                r.format, r.level, r.exact_match_rate
            ));
        }
    }
    Ok(())
}
