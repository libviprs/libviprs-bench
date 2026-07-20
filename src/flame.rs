//! Time-weighted folded-stack construction for the `flamegraph` binary.
//!
//! A flame graph's frame WIDTH is proportional to its sample weight, so for the
//! width to mean *time* the weight must be time — not a flat per-event count. The
//! in-tree engines stamp each [`EngineEvent::TileCompleted`] with a
//! coordinating-thread timestamp (issue #67), so the wall-clock gap between one
//! tile and the next is that tile's production time. [`events_to_folded_stacks`]
//! folds those gaps into inferno's `stack <weight>` format with the weight in
//! MICROSECONDS, and [`tile_weight_micros`] is the pure per-tile conversion.
//!
//! Only time-weighted tile frames are emitted (`engine;level_N;tile_rR_cC`). The
//! earlier folded output also injected standalone marker lines whose weight was a
//! tile COUNT (`level_N_start <tile_count>`, `level_N_complete <tiles_produced>`)
//! — a count on a time axis, which made those frames dwarf the tiles they
//! summarised. Those markers are gone; the level grouping survives as the parent
//! frame of its tiles.

use std::time::SystemTime;

use libviprs::EngineEvent;

/// inferno `--countname` for the time-weighted graph: the sample unit is
/// microseconds of wall time, not a count of events.
pub const FLAMEGRAPH_COUNT_NAME: &str = "microseconds";

/// Convert the wall-clock gap between the previous tile and this one into a
/// flame-graph sample weight, in microseconds.
///
/// The weight floors at 1: inferno silently drops a zero-count frame, and the
/// first tile in a stream (no predecessor), a missing timestamp, a zero gap, or
/// a backwards clock (skew) would otherwise yield 0 — so each still contributes a
/// single-microsecond sliver rather than vanishing. A backwards gap is clamped
/// rather than allowed to underflow.
pub fn tile_weight_micros(prev: Option<SystemTime>, cur: Option<SystemTime>) -> u64 {
    match (prev, cur) {
        (Some(prev), Some(cur)) => cur
            .duration_since(prev)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0)
            .max(1),
        _ => 1,
    }
}

/// Fold an engine's observer event stream into inferno's `stack <weight>` lines,
/// weighting each tile frame by the time it took (see [`tile_weight_micros`]).
///
/// Each [`EngineEvent::TileCompleted`] becomes one frame
/// `engine;level_<level>;tile_r<row>_c<col> <micros>`, nested under its pyramid
/// level so the level frame's width is the total time spent in that level and the
/// root's width is the total tiling time. Non-tile events (level / strip / batch
/// / finished markers) are structural, carry no timestamp, and are NOT emitted —
/// folding them in with a fabricated weight would put non-time area on a time
/// axis.
pub fn events_to_folded_stacks(events: &[EngineEvent], engine_name: &str) -> Vec<String> {
    let mut stacks = Vec::new();
    // Timestamp of the previous tile, so this tile's weight is the gap since it.
    let mut prev_tile_ts: Option<SystemTime> = None;

    for event in events {
        if let EngineEvent::TileCompleted {
            coord, timestamp, ..
        } = event
        {
            let weight = tile_weight_micros(prev_tile_ts, *timestamp);
            stacks.push(format!(
                "{engine_name};level_{};tile_r{}_c{} {weight}",
                coord.level, coord.row, coord.col
            ));
            prev_tile_ts = *timestamp;
        }
    }

    stacks
}
