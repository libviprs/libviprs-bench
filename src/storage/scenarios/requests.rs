//! Round trips and bytes, which is the quantity the remote-storage question
//! actually turns on.
//!
//! The whole argument for one archive entry over a tree of millions is about
//! what happens when the pyramid lives somewhere that charges per request. The
//! crate has no HTTP range reader and its object-store sink is put-only, so
//! nothing in a normal run can time that. What a run *can* do is count, because
//! `pmtiles::Reader::try_new` is generic over `RangeReader` and
//! [`CountingSource`](super::counting::CountingSource) sits underneath it.
//!
//! # One column is observed and one is declared, and they must never look alike
//!
//! The archive's counts come out of a reader that really made those requests.
//! The tree's do not: a lookup there is `plan.tile_path(...)` and
//! `std::fs::read(...)`, with the index work happening in the kernel one path
//! resolution at a time and nothing underneath it to count. Its numbers are a
//! declaration from the model "one whole object per tile", they are correct,
//! and rendering them beside the archive's observed counts without saying so
//! would be the single most misleading thing this family could publish.

use std::path::Path;

use libviprs::planner::TileCoord;
use libviprs::pmtiles::Reader;

use super::Origin;
use super::counting::CountingSource;

/// The operations a request count is broken down by.
pub const OPERATIONS: [&str; 5] = [
    "open",
    "root_lookup",
    "leaf_lookup_cold",
    "leaf_lookup_warm",
    "random_walk",
];

/// What one operation cost in round trips and bytes, and where those numbers
/// came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationCount {
    pub operation: &'static str,
    pub requests: u64,
    pub bytes: u64,
    pub origin: Origin,
}

/// Count what the archive really asks for, operation by operation.
///
/// `leaf_coord` is `None` on a root-only archive, and the two leaf rows then
/// carry no count at all rather than a zero: a root-only archive does not
/// perform a free leaf lookup, it performs none.
pub fn pmtiles(
    archive: &Path,
    root_coord: TileCoord,
    leaf_coord: Option<TileCoord>,
    walk: &[TileCoord],
) -> Vec<OperationCount> {
    let source = CountingSource::try_open(archive).expect("the archive opens");
    let reader = Reader::try_new(source).expect("the archive's index is readable");

    let mut out = Vec::with_capacity(OPERATIONS.len());
    out.push(OperationCount {
        operation: "open",
        requests: reader.source().count(),
        bytes: reader.source().bytes(),
        origin: Origin::Observed,
    });

    let measure = |operation: &'static str, coords: &[TileCoord]| {
        reader.source().forget();
        for coord in coords {
            let z = u8::try_from(coord.level).expect("a level PMTiles can address");
            reader
                .get_tile(z, coord.col, coord.row)
                .expect("a lookup succeeds");
        }
        OperationCount {
            operation,
            requests: reader.source().count(),
            bytes: reader.source().bytes(),
            origin: Origin::Observed,
        }
    };

    out.push(measure("root_lookup", &[root_coord]));
    if let Some(leaf) = leaf_coord {
        out.push(measure("leaf_lookup_cold", &[leaf]));
        out.push(measure("leaf_lookup_warm", &[leaf]));
    }
    out.push(measure("random_walk", walk));
    out
}

/// What the tree costs, from the model rather than from a counter.
///
/// One whole object per tile, at the object's size on disk. Every row is
/// [`Origin::Declared`] and the page has to render it as declared.
pub fn directory(
    tiles_in_archive: u64,
    bytes_per_tile: u64,
    walk_length: u64,
    has_leaves: bool,
) -> Vec<OperationCount> {
    let declared = |operation: &'static str, tiles: u64| OperationCount {
        operation,
        requests: tiles,
        bytes: tiles * bytes_per_tile,
        origin: Origin::Declared,
    };

    // Opening a tree reads nothing. `DirectoryPyramidReader::try_open` is one
    // `is_dir()` stat, so the declared request count is zero and that zero is a
    // real measurement of the model rather than a hole.
    let mut out = vec![OperationCount {
        operation: "open",
        requests: 0,
        bytes: 0,
        origin: Origin::Declared,
    }];
    out.push(declared("root_lookup", 1));
    if has_leaves {
        out.push(declared("leaf_lookup_cold", 1));
        out.push(declared("leaf_lookup_warm", 1));
    }
    out.push(declared("random_walk", walk_length.min(tiles_in_archive)));
    out
}

/// Whether every row of a set was observed.
pub fn all_observed(counts: &[OperationCount]) -> bool {
    counts.iter().all(|row| row.origin == Origin::Observed)
}

/// Whether every row of a set was declared.
pub fn all_declared(counts: &[OperationCount]) -> bool {
    counts.iter().all(|row| row.origin == Origin::Declared)
}
