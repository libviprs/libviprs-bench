//! The `format` family runner (#102). Skeleton: the cells are not measured yet.

/// A tile codec the family compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// PNG.
    Png,
}

impl Codec {
    /// Every codec, in report order.
    pub const ALL: [Codec; 1] = [Codec::Png];

    /// The name a row carries.
    pub fn name(self) -> &'static str {
        "png"
    }

    /// Whether the codec must reproduce the pixels exactly.
    pub fn is_lossless(self) -> bool {
        true
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
    /// Gradient-masked ink IoU.
    pub ink_iou: Option<f64>,
    /// Edge displacement against the known truth, in pixels.
    pub edge_displacement: Option<f64>,
}

/// The four content classes and whether each can be measured.
pub fn content_classes() -> Vec<(&'static str, ClassStatus)> {
    Vec::new()
}

/// Measure the synthetic control over `levels` pyramid levels.
///
/// # Errors
///
/// A message when a cell cannot be measured.
pub fn run_synthetic_control(_levels: u32) -> Result<Vec<CellRow>, String> {
    Err("not built".into())
}

/// A run is invalid when a lossless cell is not exact.
///
/// # Errors
///
/// A message naming the offending cell.
pub fn validate(_rows: &[CellRow]) -> Result<(), String> {
    Ok(())
}
