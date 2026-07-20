//! Flame-graph time-weighting guard (#25).
//!
//! A flame graph's frame WIDTH must be proportional to the TIME spent, not to a
//! flat per-event sample count. The event flame graph derives per-tile time from
//! the `TileCompleted` timestamps the in-tree engines stamp, so a tile that took
//! twice as long draws twice as wide. This pins that contract:
//!
//!   * [`tile_weight_micros`] turns an inter-tile wall-clock gap into a
//!     microsecond sample weight, flooring at 1 so inferno never drops a frame;
//!   * [`events_to_folded_stacks`] weights each tile frame by that gap and emits
//!     ONLY time-weighted tile frames — never the old count-as-weight marker
//!     lines (`level_N_start {tile_count}`) that injected a tile COUNT onto the
//!     time axis.

use std::time::{Duration, SystemTime};

use libviprs::{EngineEvent, TileCoord};
use libviprs_bench::flame::{events_to_folded_stacks, tile_weight_micros};

/// A tile whose gap since the previous tile is 30 ms weighs exactly 30 000 µs —
/// three times a 10 ms tile. The weight is TIME, in microseconds.
#[test]
fn tile_weight_is_the_inter_tile_gap_in_micros() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    assert_eq!(
        tile_weight_micros(Some(base), Some(base + Duration::from_millis(30))),
        30_000
    );
    assert_eq!(
        tile_weight_micros(Some(base), Some(base + Duration::from_millis(10))),
        10_000
    );
}

/// The floor: a first tile with no predecessor, a missing timestamp, a backwards
/// clock (skew), or a zero gap all weigh 1 µs — never 0 (inferno drops a
/// zero-count frame) and never an underflowed huge value.
#[test]
fn tile_weight_floors_at_one() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    assert_eq!(
        tile_weight_micros(None, Some(base)),
        1,
        "no predecessor floors at 1"
    );
    assert_eq!(
        tile_weight_micros(Some(base), None),
        1,
        "missing timestamp floors at 1"
    );
    assert_eq!(
        tile_weight_micros(Some(base + Duration::from_millis(5)), Some(base)),
        1,
        "a backwards clock floors at 1, never underflows"
    );
    assert_eq!(
        tile_weight_micros(Some(base), Some(base)),
        1,
        "a zero gap still keeps the frame"
    );
}

/// The folded stacks weight tile frames by TIME: three tiles at 0 / 10 / 30 ms
/// yield per-tile weights 1, 10 000, 20 000 (a 1:2 ratio between the measured
/// tiles), NOT three equal counts — and carry no count-as-weight marker frame
/// (the tile_count / tiles_produced = 3 must appear nowhere as a weight).
#[test]
fn folded_stacks_are_time_weighted_not_count_weighted() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let tile = |col: u32, at: SystemTime| EngineEvent::TileCompleted {
        coord: TileCoord::new(0, col, 0),
        worker_id: None,
        timestamp: Some(at),
    };
    let events = vec![
        EngineEvent::LevelStarted {
            level: 0,
            width: 512,
            height: 512,
            tile_count: 3,
        },
        tile(0, t0),                             // first tile: floored to 1
        tile(1, t0 + Duration::from_millis(10)), // +10 ms
        tile(2, t0 + Duration::from_millis(30)), // +20 ms
        EngineEvent::LevelCompleted {
            level: 0,
            tiles_produced: 3,
        },
    ];

    let stacks = events_to_folded_stacks(&events, "monolithic");

    // Each folded line is "stack weight"; split the trailing integer weight off.
    let parsed: Vec<(String, u64)> = stacks
        .iter()
        .map(|s| {
            let (stack, w) = s.rsplit_once(' ').expect("a folded line carries a weight");
            (
                stack.to_string(),
                w.parse::<u64>().expect("the weight is an integer"),
            )
        })
        .collect();

    // Only time-weighted tile frames are emitted — nested under engine;level_0.
    for (stack, _) in &parsed {
        assert!(
            stack.contains(";level_0;tile_"),
            "every frame is a time-weighted tile frame, got `{stack}`"
        );
    }

    let weights: Vec<u64> = parsed.iter().map(|(_, w)| *w).collect();
    assert_eq!(
        weights,
        vec![1, 10_000, 20_000],
        "tile weights track the inter-tile time gaps, not a flat count"
    );
    assert!(
        !weights.contains(&3),
        "a tile COUNT (3) must never leak in as a sample weight"
    );
}
