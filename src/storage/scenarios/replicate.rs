//! One cell, measured through the whole sweep, publishing its own dispersion.
//!
//! On a host with no calibrated baseline this is the only noise figure there
//! is, and it is a floor rather than a calibration. It exists because the two
//! committed profiles both walked the same cell, which made them a free
//! replicate pair, and nobody had looked: across that pair p99 moved by three
//! quarters on an idle machine running identical code.
//!
//! It used to be two measurements, one at each end of the sweep, and the bare
//! gap between them was published as the floor. Three captures of the same cell
//! now exist and they report three different floors: 3.46% and 36.89% on the
//! arm64 laptop eleven hours apart, and 21.31% on the native x86_64 box. None of
//! them is wrong, and nothing in any of the three documents can say which is the
//! outlier, because a gap between two points has no dispersion of its own.
//! Simulated, two independent two-point gaps disagree by more than 2x on 58.9%
//! of pairs and by more than 10x on 12.7% of them, so a 10x disagreement was not
//! an accident waiting to happen, it was the estimator arriving on schedule
//! (libviprs-bench #84).
//!
//! What replaces it is three numbers per metric, from every placement rather
//! than from the two ends:
//!
//! * `spreadPct`, the floor: a 95% prediction half-width for one more
//!   measurement of this cell, as a percent of the centre. That is the
//!   question `covered_by_noise` asks, phrased as the interval that answers it,
//!   and its multiplier widens as the placement count falls, so a floor resting
//!   on few points is visibly wider rather than invisibly lucky.
//! * `driftPct`, the fitted first-to-last change across the sweep, signed.
//! * `residualPct`, the floor again with that trend removed.
//!
//! The split is what the two arm64 captures needed. In the noisier one the
//! opening placement and the closing placement of the control differ by more
//! than ten sigma of their own sampling error on half the metrics, and the
//! direction is uniform: reads more than doubled in throughput between the start
//! of the sweep and the end of it, on a host whose fifteen-minute load average
//! was 3.38 when the sweep began and 1.96 when it finished. That is a real trend
//! in the machine, not scatter, and a single number could not say so. It also
//! settles what those two captures looked like they were saying about the
//! harness: at the closing placement, which is the quiet end of both sweeps,
//! they agree to a median of 1.73% across the 48 metrics, while at the opening
//! placement they differ by a median of 31.62%. The measurement path did not
//! move between the two commits; the machine did.
//!
//! The control is still placed through the sweep rather than twice in a row:
//! once before the first cell and once after every non-control cell. Back to
//! back it would see none of the thermal, neighbour and page-cache drift the
//! control is there to catch. That rule also bounds the count, and the bound is
//! the sweep's own length rather than a number somebody picked: the `full`
//! storage profile walks five non-control cells and so holds six placements,
//! `xl` holds seven, and the `engines` profile holds sixteen.

use std::collections::BTreeMap;

use super::Cell;

/// The fewest placements a published floor may rest on.
///
/// Four points leave two degrees of freedom once a trend is fitted, which is
/// not enough to call the leftover scatter anything. Five leave three. Below
/// this the block is not published at all, because a floor from too few points
/// is the defect this module was rewritten to remove and publishing a wider one
/// instead would still let a reader compare it with a real one.
pub const MIN_REPLICATE_REPS: usize = 5;

/// The two-sided coverage the published floor is built for.
pub const COVERAGE: f64 = 0.95;

/// What the document calls the estimator behind the floor.
///
/// Published next to the number so a reader can tell a floor from six
/// placements from a floor from two without knowing which era the run is in.
pub const ESTIMATOR: &str = "t-prediction-interval-over-placement-medians";

/// Put the control before the first cell and after every other one.
///
/// The rest keeps its order, and no two placements are adjacent: a placement
/// pair taken back to back sees none of the drift across the sweep and would
/// publish a flatteringly narrow floor. That is also what fixes the count at
/// `rest.len() + 1` rather than at a constant.
///
/// Generic over the cell type because both families schedule one, and a control
/// is a control whether the thing being repeated is a storage cell or an engine
/// cell (#75).
pub fn schedule<T: Copy>(replicate: T, rest: &[T]) -> Vec<T> {
    let mut out = Vec::with_capacity(2 * rest.len() + 1);
    out.push(replicate);
    for cell in rest {
        out.push(*cell);
        out.push(replicate);
    }
    out
}

/// How many times a schedule measures the control.
pub fn placements<T: Copy + PartialEq>(schedule: &[T], replicate: T) -> usize {
    schedule.iter().filter(|cell| **cell == replicate).count()
}

/// Whether a schedule ever measures the control twice in a row.
///
/// An adjacent pair is two measurements of one moment, so it inflates the
/// placement count without adding any of the window the floor is meant to
/// cover.
pub fn has_adjacent_placements<T: Copy + PartialEq>(schedule: &[T], replicate: T) -> bool {
    schedule
        .windows(2)
        .any(|pair| pair[0] == replicate && pair[1] == replicate)
}

/// Whether a schedule really measures `replicate` at both ends.
pub fn measured_first_and_last<T: Copy + PartialEq>(schedule: &[T], replicate: T) -> bool {
    schedule.len() >= 2
        && schedule.first() == Some(&replicate)
        && schedule.last() == Some(&replicate)
}

// ---------------------------------------------------------------------------
// The estimator
// ---------------------------------------------------------------------------

/// Student's two-sided 97.5% point, by degrees of freedom.
///
/// Tabulated where the tail matters and approximated where it does not. The
/// small-`df` end is the whole reason the multiplier is here at all: at one
/// degree of freedom it is 12.706, which is the arithmetic saying that two
/// points bound nothing, and no series expansion reproduces that.
const T_975: [f64; 30] = [
    12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.160,
    2.145, 2.131, 2.120, 2.110, 2.101, 2.093, 2.086, 2.080, 2.074, 2.069, 2.064, 2.060, 2.056,
    2.052, 2.048, 2.045, 2.042,
];

/// The normal quantile the `t` converges to.
const Z_975: f64 = 1.959_963_985;

/// Student's two-sided 97.5% point at `df` degrees of freedom, or `None` at
/// zero, where there is no such point.
pub fn t_975(df: usize) -> Option<f64> {
    if df == 0 {
        return None;
    }
    if df <= T_975.len() {
        return Some(T_975[df - 1]);
    }
    // Cornish-Fisher, first term. It is within 0.15% of the table by df = 30,
    // which `the_t_table_and_its_continuation_agree_at_the_seam` pins.
    let d = df as f64;
    Some(Z_975 + (Z_975 * Z_975 * Z_975 + Z_975) / (4.0 * d))
}

/// The factor a sample standard deviation is multiplied by to bound one more
/// measurement at [`COVERAGE`].
///
/// `t(n-1) * sqrt(1 + 1/n)`: the textbook prediction half-width, and the reason
/// the placement count is load bearing rather than decorative. At two
/// placements it is 15.6, at six 2.78, at sixteen 2.19, and it never falls
/// below 1.96. A floor that rests on few points is wide, and it says so in the
/// same number.
pub fn prediction_multiplier(n: usize) -> Option<f64> {
    if n < 2 {
        return None;
    }
    Some(t_975(n - 1)? * (1.0 + 1.0 / n as f64).sqrt())
}

/// The median of a slice, or `None` when it is empty.
fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = sorted.len() / 2;
    Some(if sorted.len() % 2 == 0 {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    })
}

/// What one metric's placements say about the run's own noise.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dispersion {
    /// The published floor, as a percent of the centre.
    pub floor_pct: f64,
    /// The fitted first-to-last change across the sweep, signed, as a percent
    /// of the centre. Positive means the metric rose as the sweep ran.
    pub drift_pct: f64,
    /// The floor with that trend removed, as a percent of the centre.
    pub residual_pct: f64,
}

/// Read one metric's placement medians, in sweep order, as a floor.
///
/// `None` below [`MIN_REPLICATE_REPS`] placements, on a non-finite value, and
/// on a centre at or below zero, because a percentage of nothing is not a
/// number and an infinity in a noise floor silently disables every verdict
/// downstream.
///
/// The values arrive in sweep order and the order is load bearing for exactly
/// one of the three numbers. `floor_pct` and `residual_pct` are dispersions and
/// a shuffle leaves the first unchanged; `drift_pct` is a trend and a shuffle
/// destroys it, which is the point of publishing them apart.
pub fn dispersion(values: &[f64]) -> Option<Dispersion> {
    let n = values.len();
    if n < MIN_REPLICATE_REPS || values.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let centre = median(values)?;
    if centre <= 0.0 {
        return None;
    }
    let count = n as f64;
    let mean = values.iter().sum::<f64>() / count;
    let sd = (values.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / (count - 1.0)).sqrt();
    let floor_pct = prediction_multiplier(n)? * sd / centre * 100.0;

    // Least squares against the placement index. The index and not the clock:
    // the document records no per-cell timestamp, and the placements are spread
    // evenly through the schedule by construction, so the index is the position
    // in the sweep that the schedule actually guarantees.
    let x_bar = (count - 1.0) / 2.0;
    let s_xx: f64 = (0..n)
        .map(|i| {
            let d = i as f64 - x_bar;
            d * d
        })
        .sum();
    let s_xy: f64 = values
        .iter()
        .enumerate()
        .map(|(i, v)| (i as f64 - x_bar) * (v - mean))
        .sum();
    let slope = if s_xx > 0.0 { s_xy / s_xx } else { 0.0 };
    let drift_pct = slope * (count - 1.0) / centre * 100.0;

    let residual_ss: f64 = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let fitted = mean + slope * (i as f64 - x_bar);
            let r = v - fitted;
            r * r
        })
        .sum();
    // Two parameters fitted, so two degrees of freedom gone. The multiplier
    // below drops the leverage term of the regression prediction interval,
    // which is exact at the middle of the sweep and understates the width at
    // its ends by at most a few percent at these placement counts. `residualPct`
    // is published for reading rather than for grading, and `spreadPct`, which
    // IS graded against, carries no such approximation.
    let residual_sd = (residual_ss / (count - 2.0)).sqrt();
    let residual_pct =
        t_975(n - 2)? * (1.0 + 1.0 / count).sqrt() * residual_sd / centre * 100.0;

    Some(Dispersion {
        floor_pct,
        drift_pct,
        residual_pct,
    })
}

/// How far two measurements of one metric sit apart, as a percentage of the
/// smaller.
///
/// Kept because the archive holds documents from the era when this WAS the
/// floor, and a reader of one of those needs the arithmetic that produced it.
/// Nothing in a new sweep publishes it.
pub fn spread_pct(first: f64, last: f64) -> Option<f64> {
    let smaller = first.min(last);
    if smaller <= 0.0 || !first.is_finite() || !last.is_finite() {
        return None;
    }
    Some((first - last).abs() / smaller * 100.0)
}

// ---------------------------------------------------------------------------
// The block
// ---------------------------------------------------------------------------

/// The estimator a floor came from, published beside it.
#[derive(Debug, Clone, PartialEq)]
pub struct Estimator {
    pub method: String,
    pub reps: usize,
    pub coverage: f64,
    pub multiplier: f64,
    /// Metrics the control measured fewer times than `reps`, and which are
    /// therefore not in the maps. A floor over a different number of points is
    /// a different statistic and the block publishes one estimator, so a short
    /// metric is dropped and counted rather than mixed in.
    pub dropped_metrics: usize,
}

/// What the sweep publishes about its own noise floor.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplicateBlock {
    pub cell: String,
    pub reps: usize,
    pub estimator: Estimator,
    /// Per metric, the published floor.
    pub spread_pct: BTreeMap<String, f64>,
    /// Per metric, the fitted first-to-last change.
    pub drift_pct: BTreeMap<String, f64>,
    /// Per metric, the floor with that trend removed.
    pub residual_pct: BTreeMap<String, f64>,
}

/// Build the block from the control's placements, in sweep order.
///
/// `Err` below [`MIN_REPLICATE_REPS`] placements, which is the case the whole
/// scenario exists to prevent: a block computed from two would publish a gap
/// with no dispersion of its own, and one computed from a single measurement
/// would publish a zero and read as a perfectly quiet host.
pub fn block(
    cell: &Cell,
    measurements: &[BTreeMap<String, f64>],
) -> Result<ReplicateBlock, String> {
    if measurements.len() < MIN_REPLICATE_REPS {
        return Err(format!(
            "the replicate cell {} was measured {} times and a published floor needs at least \
             {MIN_REPLICATE_REPS}: below that the control has no dispersion of its own, so the \
             number it publishes cannot say whether it is the outlier",
            cell.spec(),
            measurements.len()
        ));
    }
    let reps = measurements.len();
    // Symmetric, on purpose. Checking only that every key of the first
    // measurement is in the others would miss a placement that grew a key, and
    // a placement measuring something the others did not is the same disagreement
    // read from the other end.
    for (index, measurement) in measurements.iter().enumerate() {
        if measurement.len() != measurements[0].len() {
            return Err(format!(
                "measurement 1 of {} reports {} metrics and measurement {} reports {}, so the \
                 placements are not the same measurement",
                cell.spec(),
                measurements[0].len(),
                index + 1,
                measurement.len()
            ));
        }
    }

    let mut spread = BTreeMap::new();
    let mut drift = BTreeMap::new();
    let mut residual = BTreeMap::new();
    for metric in measurements[0].keys() {
        let mut values = Vec::with_capacity(reps);
        for (index, measurement) in measurements.iter().enumerate() {
            let Some(value) = measurement.get(metric) else {
                return Err(format!(
                    "measurement 1 of {} reports `{metric}` and measurement {} does not, so the \
                     placements are not the same measurement",
                    cell.spec(),
                    index + 1
                ));
            };
            values.push(*value);
        }
        let Some(found) = dispersion(&values) else {
            return Err(format!(
                "`{metric}` on {} has no dispersion as a percentage over {values:?}",
                cell.spec()
            ));
        };
        spread.insert(metric.clone(), found.floor_pct);
        drift.insert(metric.clone(), found.drift_pct);
        residual.insert(metric.clone(), found.residual_pct);
    }

    Ok(ReplicateBlock {
        cell: cell.spec(),
        reps,
        estimator: estimator_for(reps, 0),
        spread_pct: spread,
        drift_pct: drift,
        residual_pct: residual,
    })
}

/// The estimator block for a floor resting on `reps` placements.
pub fn estimator_for(reps: usize, dropped_metrics: usize) -> Estimator {
    Estimator {
        method: ESTIMATOR.to_string(),
        reps,
        coverage: COVERAGE,
        multiplier: prediction_multiplier(reps).unwrap_or(f64::NAN),
        dropped_metrics,
    }
}

/// Whether a delta is inside the run's own noise floor for that metric.
///
/// A delta the floor covers is `noise`, not a pass and not a regression, and
/// the verdict has to say which floor caused it.
pub fn covered_by_noise(block: &ReplicateBlock, metric: &str, delta_pct: f64) -> bool {
    block
        .spread_pct
        .get(metric)
        .is_some_and(|floor| delta_pct.abs() <= *floor)
}

// ---------------------------------------------------------------------------
// Reaching the document
// ---------------------------------------------------------------------------

use super::super::cells::{Profile, replicate_cell};
use super::super::document::{Document, Replicate, ReplicateEstimator};

/// The replicate block a finished sweep publishes, or `None`.
///
/// `None` on a profile with no control, and `None` when the control was placed
/// fewer than [`MIN_REPLICATE_REPS`] times.
pub fn block_for(doc: &Document, profile: Profile) -> Option<Replicate> {
    block_for_cell(doc, &replicate_cell(profile)?.spec())
}

/// The same block for a control cell named by its own spec.
///
/// Family-independent: it needs the control's name in the document and nothing
/// else, so the `engines` family gets its noise floor from this rather than from
/// a second copy of the arithmetic (#75).
///
/// The placements are found by position. `Document::push` appends, so for a
/// given `(scenario, backend, scale)` the control's rows arrive in the order
/// the sweep measured them, which is the order the trend is fitted against.
///
/// Matched on the cell's own spec rather than on `(scale, source)`. Two
/// `engines` cells can share a tile count and a source and differ only in their
/// thread budget, and a filter that could not tell them apart would compute a
/// floor across two different measurements and call it drift.
pub fn block_for_cell(doc: &Document, spec: &str) -> Option<Replicate> {
    let mut placements: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for cell in &doc.cells {
        if cell.cell != spec {
            continue;
        }
        let Some(median) = cell.median else { continue };
        placements
            .entry(format!("{}.{}", cell.backend, cell.key))
            .or_default()
            .push(median);
    }

    // The run's placement count is the most any one metric saw. A metric the
    // control measured fewer times is dropped rather than averaged in on its
    // own count, because the block publishes one estimator and a floor over
    // four points is not the same statistic as a floor over six.
    let reps = placements.values().map(Vec::len).max()?;
    if reps < MIN_REPLICATE_REPS {
        return None;
    }

    let mut spread = serde_json::Map::new();
    let mut drift = serde_json::Map::new();
    let mut residual = serde_json::Map::new();
    let mut dropped = 0usize;
    for (metric, values) in &placements {
        if values.len() != reps {
            dropped += 1;
            continue;
        }
        let Some(found) = dispersion(values) else {
            dropped += 1;
            continue;
        };
        let number = |v: f64| {
            serde_json::Number::from_f64(v)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)
        };
        spread.insert(metric.clone(), number(found.floor_pct));
        drift.insert(metric.clone(), number(found.drift_pct));
        residual.insert(metric.clone(), number(found.residual_pct));
    }
    if spread.is_empty() {
        return None;
    }

    let estimator = estimator_for(reps, dropped);
    Some(Replicate {
        cell: spec.to_string(),
        replicate_reps: reps as u32,
        estimator: Some(ReplicateEstimator {
            method: estimator.method,
            reps: estimator.reps as u32,
            coverage: estimator.coverage,
            multiplier: estimator.multiplier,
            dropped_metrics: estimator.dropped_metrics as u32,
        }),
        spread_pct: serde_json::Value::Object(spread),
        drift_pct: serde_json::Value::Object(drift),
        residual_pct: serde_json::Value::Object(residual),
    })
}
