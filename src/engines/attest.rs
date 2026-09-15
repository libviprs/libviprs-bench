//! Whether an `engines` cell measured the engine it names, observed from the
//! pyramid that engine really wrote.
//!
//! The `storage` family's attestation asks whether the backend a row names
//! produced the archive it was measured against, and whether the two backends
//! agree byte for byte about what a tile contains. This is the same question
//! asked of three engines: did this engine write a pyramid at all, did it write
//! the same one every repetition, and does it agree with its siblings about the
//! shape of what they all wrote?
//!
//! The evidence is a walk of the real sink directory, done in the child that
//! ran the engine: [`crate::RunMetrics::per_level_tiles`] counts the PNG files
//! under each level directory and [`crate::RunMetrics::artefact`] adds the byte
//! and entry totals. Nothing here reads anything the cell says about itself.
//!
//! # The shortcut this exists to prevent
//!
//! Stamping `attested: true` and moving on. The aggregator refuses an `ok` cell
//! that is not attested, so the cheapest way to make a document archivable is a
//! constant, and a constant is indistinguishable from a correct run in a sweep
//! where everything really is attested. So the verdict is a pure function of
//! the observations and it is tested where it can fail: handed a group whose
//! grids disagree, or one engine, or an engine that wrote nothing, it has to
//! move.
//!
//! # Why one engine is never attested
//!
//! An engine alone has nothing to agree with. That is the same answer the
//! storage family gives a lone backend, and it is not pedantry: the whole
//! reason the grid is worth checking is that three engines walking the same
//! plan must produce the same tiles, and a group of one cannot demonstrate it.
//! A profile that measures one engine gets honest `false` verdicts and a
//! refusal that says why.

use crate::RunMetrics;
use crate::harness::Engine;

/// What was observed about one engine's artefact in one cell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Attestation {
    /// Every repetition wrote a pyramid this crate could walk.
    pub observed: bool,
    /// Every repetition of THIS engine wrote the same per-level grid.
    pub reproduced: bool,
    /// Some other engine in the group wrote the same grid and the same tile
    /// count.
    pub agreed: bool,
    reasons: Vec<String>,
}

impl Attestation {
    /// Whether the cell may carry `attested: true`.
    pub fn is_attested(&self) -> bool {
        self.observed && self.reproduced && self.agreed
    }

    /// Why not, one sentence per half that failed. Empty when attested.
    pub fn reasons(&self) -> &[String] {
        &self.reasons
    }
}

/// The per-level PNG grid every repetition of one engine wrote, when they all
/// wrote the same one.
fn grid(runs: &[RunMetrics]) -> Option<&[u64]> {
    let first = runs.first()?.per_level_tiles.as_slice();
    if first.is_empty() {
        return None;
    }
    runs.iter()
        .all(|r| r.per_level_tiles == first)
        .then_some(first)
}

/// The tile count every repetition of one engine reported, when they agree.
fn tiles(runs: &[RunMetrics]) -> Option<u64> {
    let first = runs.first()?.tiles_produced;
    runs.iter().all(|r| r.tiles_produced == first).then_some(first)
}

/// Whether every repetition walked a real tree.
fn walked(runs: &[RunMetrics]) -> bool {
    !runs.is_empty()
        && runs.iter().all(|r| {
            r.artefact
                .is_some_and(|a| a.filesystem_entries > 0 && a.output_bytes > 0)
                && !r.per_level_tiles.is_empty()
        })
}

/// Attest every engine in one cell against the others that ran beside it.
///
/// `group` is one entry per engine that produced any repetition at all, with
/// that engine's timed repetitions in order. The returned verdicts are in the
/// same order.
pub fn attest_group(group: &[(Engine, Vec<RunMetrics>)]) -> Vec<(Engine, Attestation)> {
    let mut out = Vec::with_capacity(group.len());
    for (engine, runs) in group {
        let mut verdict = Attestation::default();
        let mut reasons: Vec<String> = Vec::new();

        verdict.observed = walked(runs);
        if !verdict.observed {
            reasons.push(format!(
                "{} left no pyramid this crate could walk in {} of {} repetitions, so nothing \
                 observed what it wrote",
                engine.as_str(),
                runs.iter()
                    .filter(|r| r.artefact.is_none() || r.per_level_tiles.is_empty())
                    .count(),
                runs.len().max(1)
            ));
        }

        let own_grid = grid(runs);
        verdict.reproduced = own_grid.is_some() && tiles(runs).is_some();
        if !verdict.reproduced {
            reasons.push(format!(
                "{} did not write the same pyramid every repetition, which is a defect rather \
                 than a delta: the plan and the source are identical across them",
                engine.as_str()
            ));
        }

        // Agreement with a sibling. The comparison is on the grid AND the tile
        // count, because the two can come apart: a level directory that lost a
        // tile keeps the level count and moves the grid, and an engine that
        // skipped a blank tile moves the count and not the grid.
        let sibling = group
            .iter()
            .filter(|(other, _)| other != engine)
            .find(|(_, other_runs)| {
                walked(other_runs)
                    && grid(other_runs).is_some()
                    && grid(other_runs) == own_grid
                    && tiles(other_runs) == tiles(runs)
            });
        verdict.agreed = own_grid.is_some() && sibling.is_some();
        if !verdict.agreed {
            reasons.push(if group.len() < 2 {
                format!(
                    "{} was the only engine measured in this cell, and one engine cannot agree \
                     with itself about the tiles three of them are supposed to produce",
                    engine.as_str()
                )
            } else {
                format!(
                    "no other engine in this cell wrote the same per-level grid and tile count \
                     as {}, so the three did not measure equal work",
                    engine.as_str()
                )
            });
        }

        verdict.reasons = if verdict.is_attested() {
            Vec::new()
        } else {
            reasons
        };
        out.push((*engine, verdict));
    }
    out
}
