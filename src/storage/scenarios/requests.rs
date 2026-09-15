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

use libviprs::planner::TileCoord;

use super::super::cells::{Backend, Profile};
use super::super::document::Origin;
use super::counting::CountingFactory;
use super::{
    Direction, Invariants, Isolation, MetricSpec, RepFacts, Scenario, ScenarioContext, ScenarioRun,
    Series, Skip, TileReader, Unit, Warmup,
};

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
    readers: &CountingFactory,
    root_coord: TileCoord,
    leaf_coord: Option<TileCoord>,
    walk: &[TileCoord],
) -> Result<Vec<OperationCount>, String> {
    let reader = readers.fresh_counting()?;

    let mut out = Vec::with_capacity(OPERATIONS.len());
    let open = reader.open_requests();
    out.push(OperationCount {
        operation: "open",
        requests: open.len() as u64,
        bytes: open.iter().map(|r| r.len as u64).sum(),
        origin: Origin::Observed,
    });

    let measure = |operation: &'static str, coords: &[TileCoord]| {
        let (looked, requests) = reader.counted(|r| {
            for coord in coords {
                r.tile(*coord)?;
            }
            Ok::<(), String>(())
        });
        looked.map(|()| OperationCount {
            operation,
            requests: requests.len() as u64,
            bytes: requests.iter().map(|r| r.len as u64).sum(),
            origin: Origin::Observed,
        })
    };

    out.push(measure("root_lookup", &[root_coord])?);
    if let Some(leaf) = leaf_coord {
        out.push(measure("leaf_lookup_cold", &[leaf])?);
        out.push(measure("leaf_lookup_warm", &[leaf])?);
    }
    out.push(measure("random_walk", walk)?);
    Ok(out)
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

// ---------------------------------------------------------------------------
// The scenario
// ---------------------------------------------------------------------------

/// `requests`: round trips and bytes, counted on the archive and declared on
/// the tree.
///
/// Deterministic, so one repetition is the whole measurement and a dispersion
/// figure over it would be a measurement of the clock.
///
/// The two backends publish under **different metric names**, and that is the
/// point rather than an oversight: the archive's row is `requests.requests` and
/// the tree's is `requests.requests_declared`. The document keys on
/// `scenario.metric`, so a page cannot plot one as the other by accident, and
/// nothing had to change in the document schema to say so.
pub struct Requests;

pub const OBSERVED_REQUESTS: MetricSpec = MetricSpec {
    name: "requests",
    unit: Unit::Count,
    direction: Direction::LowerIsBetter,
};

pub const DECLARED_REQUESTS: MetricSpec = MetricSpec {
    name: "requests_declared",
    unit: Unit::Count,
    direction: Direction::LowerIsBetter,
};

pub const OBSERVED_BYTES: MetricSpec = MetricSpec {
    name: "request_bytes",
    unit: Unit::Bytes,
    direction: Direction::LowerIsBetter,
};

pub const DECLARED_BYTES: MetricSpec = MetricSpec {
    name: "request_bytes_declared",
    unit: Unit::Bytes,
    direction: Direction::LowerIsBetter,
};

impl Scenario for Requests {
    fn name(&self) -> String {
        "requests".to_string()
    }

    fn isolation(&self) -> Isolation {
        Isolation::ProcessPerScenario
    }

    fn warmup(&self) -> Option<Warmup> {
        None
    }

    fn reps(&self, _profile: Profile) -> u32 {
        1
    }

    fn primary(&self) -> MetricSpec {
        OBSERVED_REQUESTS
    }

    fn run(&self, ctx: &ScenarioContext<'_>, _reps: u32) -> Result<ScenarioRun, Skip> {
        let Some(root) = ctx.coords.root_addressed else {
            return Err(Skip::skipped("the cell has no root-addressed coordinate"));
        };
        let walk = &ctx.coords.random;

        let (counts, requests_metric, bytes_metric) = match ctx.backend {
            Backend::PmTiles => {
                let Some(archive) = ctx.artefact else {
                    return Err(Skip::failed("no archive to count"));
                };
                let counting = CountingFactory::new(archive);
                let counts = pmtiles(&counting, root, ctx.coords.leaf_addressed, walk)
                    .map_err(Skip::failed)?;
                (counts, OBSERVED_REQUESTS, OBSERVED_BYTES)
            }
            Backend::Directory => {
                let tiles = ctx.cell.planned_tiles().unwrap_or(0) as u64;
                let bytes_per_tile = ctx
                    .artefact
                    .and_then(|root| mean_tile_bytes(root, tiles))
                    .unwrap_or(0);
                let counts = directory(
                    tiles,
                    bytes_per_tile,
                    walk.len() as u64,
                    ctx.coords.leaf_addressed.is_some(),
                );
                (counts, DECLARED_REQUESTS, DECLARED_BYTES)
            }
        };

        let total_requests: u64 = counts.iter().map(|c| c.requests).sum();
        let total_bytes: u64 = counts.iter().map(|c| c.bytes).sum();

        let invariants = Invariants {
            requests: Some(total_requests),
            request_bytes: Some(total_bytes),
            ..Invariants::default()
        };

        Ok(ScenarioRun {
            series: vec![
                Series {
                    metric: requests_metric,
                    samples: vec![total_requests as f64],
                },
                Series {
                    metric: bytes_metric,
                    samples: vec![total_bytes as f64],
                },
            ],
            reps: vec![RepFacts {
                invariants,
                scratch: None,
            }],
            discarded_warmup: Vec::new(),
            peak_rss_bytes: None,
            heap_peak_bytes: None,
        })
    }
}

/// Mean stored bytes a tile occupies in a tree, for the declared model.
///
/// `None` when the tree cannot be walked, so the model publishes nothing rather
/// than a zero it would price at no transfer cost.
fn mean_tile_bytes(root: &std::path::Path, tiles: u64) -> Option<u64> {
    if tiles == 0 {
        return None;
    }
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return None;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    Some(total / tiles)
}
