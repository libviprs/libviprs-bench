//! The walk a plan describes: level, then row, then column.
//!
//! This is the old `read_sequential` scenario under a name that says which
//! sequence it means. A plan is row-major within each level and the archive's
//! bytes are in Hilbert order, so "sequential" was ambiguous between two walks
//! that touch the same tiles in genuinely different orders.

use libviprs::planner::{PyramidPlan, TileCoord};

/// The first `n` coordinates the plan covers, in level then row then column
/// order.
///
/// `n` is `min(n, plan length)`, so a profile asking for more coordinates than
/// the pyramid has gets the pyramid rather than an error.
pub fn coordinates(plan: &PyramidPlan, n: usize) -> Vec<TileCoord> {
    let mut all = super::plan_coordinates(plan);
    all.truncate(n);
    all
}
