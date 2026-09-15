//! One cell, measured first and last in every sweep, publishing its own spread.
//!
//! On a host with no calibrated baseline this is the only noise figure there
//! is, and it is a floor rather than a calibration. It exists because the two
//! committed profiles both walked the same cell, which made them a free
//! replicate pair, and nobody had looked: across that pair p99 moved by three
//! quarters on an idle machine running identical code. A page that charts a
//! delta smaller than that as a regression is inventing one.
//!
//! First and last rather than twice in a row, because what it is trying to see
//! is drift across the sweep: thermal, a neighbour waking up, the page cache
//! filling. Two measurements back to back would see none of that and would
//! publish a flatteringly small spread.

use std::collections::BTreeMap;

use super::Cell;

/// How many times the replicate cell is measured in a sweep.
pub const REPLICATE_REPS: usize = 2;

/// Put the replicate cell first and last around the rest of the sweep.
///
/// The rest keeps its order. A sweep with no other cells still gets two
/// measurements of the replicate cell, because a spread over one measurement is
/// not a spread.
///
/// Generic over the cell type because both families schedule one, and a control
/// is a control whether the thing being repeated is a storage cell or an engine
/// cell (#75).
pub fn schedule<T: Copy>(replicate: T, rest: &[T]) -> Vec<T> {
    let mut out = Vec::with_capacity(rest.len() + 2);
    out.push(replicate);
    out.extend_from_slice(rest);
    out.push(replicate);
    out
}

/// Whether a schedule really measures `replicate` at both ends.
pub fn measured_first_and_last<T: Copy + PartialEq>(schedule: &[T], replicate: T) -> bool {
    schedule.len() >= 2
        && schedule.first() == Some(&replicate)
        && schedule.last() == Some(&replicate)
}

/// What the sweep publishes about its own noise floor.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplicateBlock {
    pub cell: String,
    pub reps: usize,
    /// Per metric, how far the two measurements sit apart as a percentage of
    /// the smaller one.
    pub spread_pct: BTreeMap<String, f64>,
}

/// How far two measurements of one metric sit apart.
///
/// A percentage of the smaller value, which is the convention the suite plan's
/// own replicate figures were computed with. `None` where the smaller value is
/// zero or negative, because a percentage of nothing is not a number and an
/// infinity in a noise floor silently disables every verdict downstream.
pub fn spread_pct(first: f64, last: f64) -> Option<f64> {
    let smaller = first.min(last);
    if smaller <= 0.0 || !first.is_finite() || !last.is_finite() {
        return None;
    }
    Some((first - last).abs() / smaller * 100.0)
}

/// Build the block from the two measurements.
///
/// `Err` when the cell was not measured twice, which is the case the whole
/// scenario exists to prevent: a block computed from one measurement would
/// publish a spread of zero and read as a perfectly quiet host.
pub fn block(
    cell: &Cell,
    measurements: &[BTreeMap<String, f64>],
) -> Result<ReplicateBlock, String> {
    if measurements.len() != REPLICATE_REPS {
        return Err(format!(
            "the replicate cell {} was measured {} times and the block needs exactly \
             {REPLICATE_REPS}: a spread over one measurement is zero, which reads as a silent \
             host rather than as a missing control",
            cell.spec(),
            measurements.len()
        ));
    }

    let (first, last) = (&measurements[0], &measurements[1]);
    let mut spread = BTreeMap::new();
    for (metric, value) in first {
        let Some(other) = last.get(metric) else {
            return Err(format!(
                "the first measurement of {} reports `{metric}` and the last does not, so the two \
                 ends of the sweep are not the same measurement",
                cell.spec()
            ));
        };
        let Some(pct) = spread_pct(*value, *other) else {
            return Err(format!(
                "`{metric}` on {} is {value} and {other}, which has no spread as a percentage",
                cell.spec()
            ));
        };
        spread.insert(metric.clone(), pct);
    }

    Ok(ReplicateBlock {
        cell: cell.spec(),
        reps: REPLICATE_REPS,
        spread_pct: spread,
    })
}

/// Whether a delta is inside the run's own noise floor for that metric.
///
/// A delta the replicate pair covers is `noise`, not a pass and not a
/// regression, and the verdict has to say which spread caused it.
pub fn covered_by_noise(block: &ReplicateBlock, metric: &str, delta_pct: f64) -> bool {
    block
        .spread_pct
        .get(metric)
        .is_some_and(|spread| delta_pct.abs() <= *spread)
}

// ---------------------------------------------------------------------------
// Reaching the document
// ---------------------------------------------------------------------------

use super::super::cells::{Profile, replicate_cell};
use super::super::document::{Document, Replicate};

/// The replicate block a finished sweep publishes, or `None`.
///
/// `None` on a profile with no control, and `None` when the control's rows did
/// not come out in pairs, which is the case this whole scenario exists to stop:
/// a block computed from one measurement publishes a spread of zero and reads
/// as a perfectly quiet host.
///
/// The two measurements are found by position. `Document::push` appends, so for
/// a given `(scenario, backend, scale)` the control's first and last rows are
/// the two ends of the sweep, which is the drift the spread is there to see.
pub fn block_for(doc: &Document, profile: Profile) -> Option<Replicate> {
    block_for_cell(doc, &replicate_cell(profile)?.spec())
}

/// The same block for a control cell named by its own spec.
///
/// Family-independent: it needs the control's name in the document and nothing
/// else, so the `engines` family gets its noise floor from this rather than from
/// a second copy of the arithmetic (#75).
///
/// Matched on the cell's own spec rather than on `(scale, source)`. Two
/// `engines` cells can share a tile count and a source and differ only in their
/// thread budget, and a filter that could not tell them apart would compute a
/// spread across two different measurements and call it drift.
pub fn block_for_cell(doc: &Document, spec: &str) -> Option<Replicate> {
    let mut firsts: BTreeMap<String, f64> = BTreeMap::new();
    let mut lasts: BTreeMap<String, f64> = BTreeMap::new();
    for cell in &doc.cells {
        if cell.cell != spec {
            continue;
        }
        let Some(median) = cell.median else { continue };
        let key = format!("{}.{}", cell.backend, cell.key);
        // `clippy::map_entry` reads this as a lookup-then-insert on one map and
        // suggests the `Entry` API. It is not: the two arms write to two
        // different maps, so the entry form would have to hold a vacant slot in
        // `firsts` while inserting into `lasts`, clone the key to keep one for
        // the other map, and end up longer and harder to read than the sentence
        // it replaces. The lint is wrong about this code, so it is allowed here
        // and nowhere else.
        #[allow(clippy::map_entry)]
        if !firsts.contains_key(&key) {
            firsts.insert(key, median);
        } else {
            lasts.insert(key, median);
        }
    }

    let mut spread = serde_json::Map::new();
    for (metric, first) in &firsts {
        let Some(last) = lasts.get(metric) else {
            continue;
        };
        if let Some(pct) = spread_pct(*first, *last) {
            spread.insert(
                metric.clone(),
                serde_json::Number::from_f64(pct)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null),
            );
        }
    }
    if spread.is_empty() {
        return None;
    }

    Some(Replicate {
        cell: spec.to_string(),
        replicate_reps: REPLICATE_REPS as u32,
        spread_pct: serde_json::Value::Object(spread),
    })
}
