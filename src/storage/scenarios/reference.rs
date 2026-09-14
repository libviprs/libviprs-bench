//! The reference scenarios: enough to prove the skeleton measures something.
//!
//! One generation scenario and two pass scenarios. K1.4 adds `open`,
//! `first_lookup`, `decode_root`, `read_tileid_order`, the `read_concurrent@T`
//! curve and `requests` against the shapes here.
//!
//! # The one line that matters
//!
//! [`open_fresh`] is where a pass scenario gets its reader, and it asks the
//! factory for one that has never served a lookup. The harness this replaces
//! opened one reader per cell and let every read scenario share it, so
//! `read_random` walked the reader `read_sequential` had just warmed and
//! published the result as a random read. On a leaf-bearing archive that is
//! the difference between a lookup that fetches a leaf directory and one that
//! finds it already cached, which is most of what the archive's read path is.
//! Replace the body of [`open_fresh`] with anything that hands back a reader
//! someone else used and
//! `every_read_scenario_runs_in_its_own_process_on_a_fresh_reader` goes red.

use std::sync::Arc;
use std::time::Instant;

use libviprs::planner::TileCoord;

use super::super::cells::Profile;
use super::super::document::{generate_reps, read_reps};
use super::super::stats;
use super::super::{Scratch, archive_shape, artefact_digest, occupancy, write_pyramid};
use super::{
    Direction, Invariants, Isolation, MetricSpec, RepFacts, Scenario, ScenarioContext, ScenarioRun,
    Series, Skip, TileReader, Unit, Warmup,
};

/// Every reference scenario, in sweep order.
pub fn all() -> Vec<Box<dyn Scenario>> {
    vec![
        Box::new(Generate),
        Box::new(ReadPass::plan_order()),
        Box::new(ReadPass::random()),
    ]
}

/// A reader that has never served a lookup.
///
/// Every pass scenario goes through here, and nothing else in this module
/// constructs a reader.
fn open_fresh(ctx: &ScenarioContext<'_>) -> Result<Arc<dyn TileReader>, Skip> {
    ctx.readers.fresh().map_err(Skip::failed)
}

// ---------------------------------------------------------------------------
// generate
// ---------------------------------------------------------------------------

/// Write the pyramid, measure the write, and record what it cost on disk.
///
/// A repetition is a fresh scratch directory and a whole regeneration. That is
/// the expensive way and it is the only honest one: a harness that generated
/// once and timed nothing seven times would publish seven samples of the same
/// event, and its dispersion figure would be a measurement of the clock.
pub struct Generate;

pub const WALL: MetricSpec = MetricSpec {
    name: "wall",
    unit: Unit::Milliseconds,
    direction: Direction::LowerIsBetter,
};

pub const TILES_PER_S: MetricSpec = MetricSpec {
    name: "tiles_per_s",
    unit: Unit::PerSecond,
    direction: Direction::HigherIsBetter,
};

impl Scenario for Generate {
    fn name(&self) -> String {
        "generate".to_string()
    }

    fn isolation(&self) -> Isolation {
        Isolation::ProcessPerRep
    }

    fn warmup(&self) -> Option<Warmup> {
        // A fresh process per repetition is the warm-up: there is no state
        // inside the process for a discarded pass to warm.
        None
    }

    fn reps(&self, profile: Profile) -> u32 {
        generate_reps(profile)
    }

    fn primary(&self) -> MetricSpec {
        WALL
    }

    fn series(&self) -> Vec<MetricSpec> {
        vec![WALL, TILES_PER_S]
    }

    fn needs_artefact(&self) -> bool {
        false
    }

    fn run(&self, ctx: &ScenarioContext<'_>, reps: u32) -> Result<ScenarioRun, Skip> {
        let plan = ctx
            .cell
            .plan()
            .ok_or_else(|| Skip::failed("the cell does not plan"))?;
        let mut wall = Vec::new();
        let mut rate = Vec::new();
        let mut facts = Vec::new();

        for _ in 0..reps.max(1) {
            let scratch = Scratch::new(ctx.scratch_root)
                .map_err(|e| Skip::failed(format!("no scratch directory: {e}")))?;
            let written = write_pyramid(ctx.backend, ctx.cell, &plan, scratch.path())
                .map_err(Skip::failed)?;
            let occupancy = occupancy(&written.output);
            let shape = archive_shape(ctx.backend, &written.output);
            wall.push(written.wall_ms);
            rate.push(if written.wall_ms > 0.0 {
                written.tiles_produced as f64 / (written.wall_ms / 1000.0)
            } else {
                f64::NAN
            });
            facts.push(RepFacts {
                invariants: Invariants {
                    output_bytes: occupancy.map(|o| o.0),
                    allocated_bytes: occupancy.map(|o| o.3),
                    filesystem_entries: occupancy.map(|o| o.1),
                    directories: occupancy.map(|o| o.2),
                    tiles_produced: Some(written.tiles_produced),
                    artefact_digest: artefact_digest(&written.output),
                    root_entries: shape.map(|s| s.0),
                    leaves: shape.map(|s| s.1),
                    requests: None,
                    request_bytes: None,
                },
                // The name is unique within the process and never reused, so
                // two repetitions reporting one path did not regenerate.
                scratch: Some(scratch.path().to_path_buf()),
            });
        }

        // A rate that divided by a zero duration is not a rate. `NaN` never
        // reaches the document: the series drops to `null` instead.
        let rate_ok = rate.iter().all(|v| v.is_finite());
        let mut series = vec![Series {
            metric: WALL,
            samples: wall,
        }];
        if rate_ok {
            series.push(Series {
                metric: TILES_PER_S,
                samples: rate,
            });
        }

        Ok(ScenarioRun {
            series,
            reps: facts,
            discarded_warmup: Vec::new(),
            peak_rss_bytes: None,
            heap_peak_bytes: None,
        })
    }
}

// ---------------------------------------------------------------------------
// The pass scenarios
// ---------------------------------------------------------------------------

/// Which order a pass walks its coordinates in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// Level, row, column.
    Plan,
    /// The archive's own byte order.
    TileId,
    /// Seeded shuffle.
    Random,
}

/// One pass over N coordinates, repeated, with a discarded warm-up in front.
pub struct ReadPass {
    name: &'static str,
    order: Order,
}

pub const P50: MetricSpec = MetricSpec {
    name: "p50",
    unit: Unit::Microseconds,
    direction: Direction::LowerIsBetter,
};

pub const P99: MetricSpec = MetricSpec {
    name: "p99",
    unit: Unit::Microseconds,
    direction: Direction::LowerIsBetter,
};

pub const MAX: MetricSpec = MetricSpec {
    name: "max",
    unit: Unit::Microseconds,
    direction: Direction::LowerIsBetter,
};

pub const LOOKUPS_PER_S: MetricSpec = MetricSpec {
    name: "lookups_per_s",
    unit: Unit::PerSecond,
    direction: Direction::HigherIsBetter,
};

impl ReadPass {
    pub fn plan_order() -> ReadPass {
        ReadPass {
            name: "read_plan_order",
            order: Order::Plan,
        }
    }

    pub fn tileid_order() -> ReadPass {
        ReadPass {
            name: "read_tileid_order",
            order: Order::TileId,
        }
    }

    pub fn random() -> ReadPass {
        ReadPass {
            name: "read_random",
            order: Order::Random,
        }
    }

    fn coords<'a>(&self, ctx: &'a ScenarioContext<'a>) -> &'a [TileCoord] {
        match self.order {
            Order::Plan => &ctx.coords.plan_order,
            Order::TileId => &ctx.coords.tileid_order,
            Order::Random => &ctx.coords.random,
        }
    }
}

/// One pass: every lookup's latency in microseconds, and the pass's own wall.
struct Pass {
    latencies: Vec<f64>,
    wall_s: f64,
}

fn one_pass(reader: &dyn TileReader, coords: &[TileCoord]) -> Result<Pass, Skip> {
    let mut latencies = Vec::with_capacity(coords.len());
    let started = Instant::now();
    for coord in coords {
        let at = Instant::now();
        reader
            .tile(*coord)
            .map_err(|e| Skip::failed(format!("a lookup failed: {e}")))?;
        latencies.push(at.elapsed().as_secs_f64() * 1_000_000.0);
    }
    Ok(Pass {
        latencies,
        wall_s: started.elapsed().as_secs_f64(),
    })
}

impl Scenario for ReadPass {
    fn name(&self) -> String {
        self.name.to_string()
    }

    fn isolation(&self) -> Isolation {
        Isolation::ProcessPerScenario
    }

    fn warmup(&self) -> Option<Warmup> {
        Some(Warmup::ONE_DISCARDED_PASS)
    }

    fn reps(&self, profile: Profile) -> u32 {
        read_reps(profile)
    }

    fn primary(&self) -> MetricSpec {
        P50
    }

    fn series(&self) -> Vec<MetricSpec> {
        // The tail series is `p99` or `max` depending on how many lookups a
        // pass makes, and only `run` knows that, so this list is a hint and
        // the run is authoritative.
        vec![P50, P99, LOOKUPS_PER_S]
    }

    fn run(&self, ctx: &ScenarioContext<'_>, reps: u32) -> Result<ScenarioRun, Skip> {
        let coords = self.coords(ctx).to_vec();
        if coords.is_empty() {
            return Err(Skip::skipped("the cell has no coordinates to walk"));
        }
        let reader = open_fresh(ctx)?;

        // The warm-up pass is run and thrown away. Its value travels in
        // `discarded_warmup` so a reader can check it is not in `samples`.
        let mut discarded = Vec::new();
        let warmup = self.warmup().map(|w| w.passes).unwrap_or(0);
        for _ in 0..warmup {
            let pass = one_pass(reader.as_ref(), &coords)?;
            if let Some(p50) = stats::percentile(&pass.latencies, 0.5) {
                discarded.push(p50);
            }
        }

        let mut p50s = Vec::new();
        let mut tails = Vec::new();
        let mut rates = Vec::new();
        let mut tail_kind = None;
        for _ in 0..reps.max(1) {
            let pass = one_pass(reader.as_ref(), &coords)?;
            let Some(p50) = stats::percentile(&pass.latencies, 0.5) else {
                return Err(Skip::failed("a pass produced no latencies"));
            };
            let Some(tail) = stats::tail(&pass.latencies) else {
                return Err(Skip::failed("a pass produced no latencies"));
            };
            tail_kind = Some(tail.kind);
            p50s.push(p50);
            tails.push(tail.value);
            rates.push(if pass.wall_s > 0.0 {
                coords.len() as f64 / pass.wall_s
            } else {
                f64::NAN
            });
        }

        // The tail series is named for what it is. Under
        // `stats::P99_MIN_SAMPLES` lookups in a pass there is no 99th
        // percentile to estimate and the series is `max`.
        let tail_metric = match tail_kind {
            Some(stats::TailKind::P99) => P99,
            _ => MAX,
        };
        let mut series = vec![
            Series {
                metric: P50,
                samples: p50s,
            },
            Series {
                metric: tail_metric,
                samples: tails,
            },
        ];
        if rates.iter().all(|v| v.is_finite()) {
            series.push(Series {
                metric: LOOKUPS_PER_S,
                samples: rates,
            });
        }

        Ok(ScenarioRun {
            series,
            reps: vec![RepFacts::default(); reps.max(1) as usize],
            discarded_warmup: discarded,
            peak_rss_bytes: None,
            heap_peak_bytes: None,
        })
    }
}
