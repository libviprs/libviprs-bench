//! The walk the archive's bytes are in: ascending tile id.
//!
//! PMTiles orders its directory by tile id, and a tile id is a Hilbert index
//! within a zoom level on top of that level's base. A plan is a row-major walk.
//! Those are different walks over one set of tiles, and reading an archive in
//! its own byte order is a different measurement from reading it in the order
//! the writer was handed the tiles.
//!
//! # Why this module carries so much machinery for one sort
//!
//! Because the wrong implementation is a copy-paste of
//! [`plan_order`](super::plan_order) that sorts neither, and against that
//! mutation "the tile-id walk is monotone in tile id" is only a real assertion
//! if the plan walk is not. A hand-picked pair of coordinates is exactly the
//! wrong tool: pick two the sort happens to leave alone and the test is green
//! under the mutation it was written for. So [`positions_the_sort_moves`]
//! computes which positions the sort actually changes and
//! [`probe`] hands back one of them, and the test asserts the probe exists
//! before it asserts anything about it.

use libviprs::planner::{PyramidPlan, TileCoord};

use super::tile_id;

/// The same `n` coordinates [`plan_order::coordinates`](super::plan_order::coordinates)
/// walks, in ascending tile id.
///
/// A coordinate PMTiles cannot address sorts last and keeps its plan-order
/// position among its peers, because the sort is stable. A plan built by
/// `PyramidPlanner` with `Layout::Xyz` has none of those, and the branch is
/// here so that a layout change shows up as unaddressable tiles at the end
/// rather than as a panic in a benchmark.
pub fn coordinates(plan: &PyramidPlan, n: usize) -> Vec<TileCoord> {
    let mut coords = super::plan_order::coordinates(plan, n);
    coords.sort_by_key(|coord| match tile_id(*coord) {
        Some(id) => (0u8, id),
        None => (1u8, 0),
    });
    coords
}

/// The tile ids of a coordinate sequence, in that sequence's order.
///
/// An unaddressable coordinate contributes `None`, so a caller sees the hole
/// rather than a zero that sorts first.
pub fn tile_ids(coords: &[TileCoord]) -> Vec<Option<u64>> {
    coords.iter().copied().map(tile_id).collect()
}

/// Two neighbouring positions where the sequence goes backwards in tile id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inversion {
    pub index: usize,
    pub before: u64,
    pub after: u64,
}

/// The first place a sequence goes backwards in tile id, if it does.
pub fn first_inversion(coords: &[TileCoord]) -> Option<Inversion> {
    let ids = tile_ids(coords);
    for index in 1..ids.len() {
        let (Some(before), Some(after)) = (ids[index - 1], ids[index]) else {
            continue;
        };
        if after <= before {
            return Some(Inversion {
                index,
                before,
                after,
            });
        }
    }
    None
}

/// Whether every step of the sequence goes forwards in tile id.
pub fn is_monotone(coords: &[TileCoord]) -> bool {
    first_inversion(coords).is_none()
}

/// Every position at which the two sequences hold different coordinates.
///
/// This is the answer to "which cells does the wrong implementation move", and
/// it is computed rather than chosen. An empty result means the sort is a no-op
/// on this cell and no assertion about ordering can fail here, which is a fact
/// about the cell and has to be said out loud rather than discovered as a green
/// test.
pub fn positions_the_sort_moves(plan_order: &[TileCoord], tileid_order: &[TileCoord]) -> Vec<usize> {
    plan_order
        .iter()
        .zip(tileid_order)
        .enumerate()
        .filter(|(_, (plan, sorted))| plan != sorted)
        .map(|(index, _)| index)
        .collect()
}

/// One position the sort moves, with both coordinates and both tile ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    pub index: usize,
    pub in_plan_order: TileCoord,
    pub in_plan_order_tile_id: Option<u64>,
    pub in_tileid_order: TileCoord,
    pub in_tileid_order_tile_id: Option<u64>,
}

/// The first position the sort moves, or `None` when it moves nothing.
pub fn probe(plan_order: &[TileCoord], tileid_order: &[TileCoord]) -> Option<Probe> {
    let index = *positions_the_sort_moves(plan_order, tileid_order).first()?;
    Some(Probe {
        index,
        in_plan_order: plan_order[index],
        in_plan_order_tile_id: tile_id(plan_order[index]),
        in_tileid_order: tileid_order[index],
        in_tileid_order_tile_id: tile_id(tileid_order[index]),
    })
}

/// Both walks cover exactly the same tiles.
///
/// The other half of the ordering claim: a sort that dropped or duplicated a
/// coordinate would be monotone and wrong, and a benchmark comparing two passes
/// over different tile sets is comparing two different amounts of work.
pub fn same_multiset(plan_order: &[TileCoord], tileid_order: &[TileCoord]) -> bool {
    let key = |coord: &TileCoord| (coord.level, coord.col, coord.row);
    let mut left: Vec<_> = plan_order.iter().map(key).collect();
    let mut right: Vec<_> = tileid_order.iter().map(key).collect();
    left.sort_unstable();
    right.sort_unstable();
    left == right
}
