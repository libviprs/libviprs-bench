//! Time-weighted folded-stack construction for the `flamegraph` binary.
//!
//! A flame graph's frame WIDTH is proportional to its sample weight, so for the
//! width to mean *time* the weight must be time — not a flat per-event count. The
//! in-tree engines stamp each [`EngineEvent::TileCompleted`] with a
//! coordinating-thread timestamp (issue #67), so [`events_to_folded_stacks`]
//! folds the wall-clock gap between consecutive tiles into inferno's
//! `stack <weight>` format with the weight in MICROSECONDS, and
//! [`tile_weight_micros`] is the pure per-tile conversion.
//!
//! # What a frame width means — and when
//!
//! The inter-tile gap equals a single tile's *production* time only when tiles
//! are produced and emitted ONE AT A TIME, IN ORDER, on ONE thread. That holds
//! for the **monolithic** engine, which extracts and writes tiles in a serial
//! row-major loop, so its per-tile widths are faithful per-tile times. It does
//! NOT hold for the strip-based **streaming** and **MapReduce** engines: each
//! renders a whole strip and then emits that strip's tiles back-to-back, so the
//! first tile of a strip absorbs the strip's render time and the rest drain in
//! near-zero gaps. Under tile concurrency (`tile_concurrency > 0`)
//! `TileCompleted` is additionally emitted in channel-*arrival* order — an
//! interleaving the core documents as arbitrary (see
//! `streaming_mapreduce::emit_strip_tiles_parallel`) — so a per-tile width there
//! is a coordinator drain delta, not that tile's compute time.
//!
//! What stays true for EVERY engine is the AGGREGATE: each tile's weight is the
//! gap since the previous one, so the widths tile the timeline by construction —
//! a level frame's width is the total wall time spent producing that level and
//! the root's width is the total tiling time. Read the strip-based graphs at
//! level/root granularity; read only the monolithic graph tile-by-tile. The SVG
//! titles say which is which.
//!
//! Only tile frames are emitted (`engine;level_N;tile_rR_cC`). The earlier folded
//! output also injected standalone marker lines whose weight was a tile COUNT
//! (`level_N_start <tile_count>`, `level_N_complete <tiles_produced>`) — a count
//! on a time axis, which made those frames dwarf the tiles they summarised. Those
//! markers are gone; the level grouping survives as the parent frame of its
//! tiles.

use std::time::SystemTime;

use libviprs::EngineEvent;

/// inferno `--countname` for the time-weighted graph: the sample unit is
/// microseconds of wall time, not a count of events.
pub const FLAMEGRAPH_COUNT_NAME: &str = "microseconds";

/// Convert the wall-clock gap between the previous tile and this one into a
/// flame-graph sample weight, in microseconds.
///
/// Argument order is `(prev, cur)`: the *previous* tile's emit timestamp, then
/// *this* tile's. Passing them the other way round is masked by the backwards-
/// clock clamp below (it returns 1 rather than the true gap), so mind the order.
///
/// The weight floors at 1: inferno silently drops a zero-count frame, and the
/// first tile in a stream (no predecessor), a missing timestamp, a zero gap, or
/// a backwards clock (skew) would otherwise yield 0 — so each still contributes a
/// single-microsecond sliver rather than vanishing. A backwards gap is clamped
/// rather than allowed to underflow.
///
/// See the module docs for when the resulting width is a faithful per-tile time
/// (serial monolithic engine) versus an emission-cadence artifact (the strip-
/// based streaming / MapReduce engines).
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
///
/// This is a public function that accepts any event stream. Per-tile widths are
/// faithful per-tile times only for a SERIAL, in-order producer (the monolithic
/// engine); for a strip-batched or concurrent stream only the aggregate
/// level/root width is a true wall-time measure — see the module docs.
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
