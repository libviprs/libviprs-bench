//! A thread ladder, not a thread count.
//!
//! The old sweep ran one `read_concurrent` row at whatever
//! `available_parallelism` said, which cannot tell a contention problem from a
//! per-lookup cost and cannot say where a curve bends. libviprs#1024 measured
//! the bend: on the leaf-bearing cell p99 on arm64 goes 2.04, 2.27, 10.38,
//! 43.06 us at one, two, four and eight threads, so **the knee is at four**,
//! while on native x86_64 the first three points are flat and the whole move is
//! at eight. A sweep taking one thread count from the host reports the move in
//! the wrong place on at least one of those two machines, and there is no
//! thread count that is right on both.
//!
//! So the ladder is fixed at 1, 2, 4 and 8, T=1 is the control, and an arm the
//! host has no cores for is **skipped with a reason and stays in the
//! document**. Dropping it would read as a measurement nobody took rather than
//! one this host declined, and those are different claims.

use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use libviprs::planner::TileCoord;

use super::super::cells::Profile;
use super::super::document::read_reps;
use super::{
    Direction, Isolation, MetricSpec, Outcome, RepFacts, Scenario, ScenarioContext, ScenarioRun,
    Series, Skip, TileReader, Unit, Warmup,
};

/// The thread counts every sweep reports, whether or not the host can run them.
pub const THREAD_LADDER: [usize; 4] = [1, 2, 4, 8];

/// One rung of the ladder and what this host did with it.
///
/// `skip` is `None` on a rung this host measures. K1.2's [`Outcome`] taxonomy
/// is what the document keys on and [`Skip`] is what carries the reason, so a
/// declined rung is `Outcome::Skipped` with a reason rather than a row that is
/// simply absent.
#[derive(Debug, Clone)]
pub struct Arm {
    pub threads: usize,
    pub skip: Option<Skip>,
}

impl Arm {
    pub fn is_ok(&self) -> bool {
        self.skip.is_none()
    }

    pub fn outcome(&self) -> Outcome {
        match &self.skip {
            None => Outcome::Ok,
            Some(skip) => skip.outcome,
        }
    }

    pub fn reason(&self) -> Option<&str> {
        self.skip.as_ref().map(|skip| skip.reason.as_str())
    }
}

/// Every rung, in order, with the ones above `ncpu` marked skipped.
///
/// Every element of [`THREAD_LADDER`] comes back. That is the point: a 6-core
/// host publishes four rows, three measured and one that says why it is not.
pub fn ladder(ncpu: usize) -> Vec<Arm> {
    THREAD_LADDER
        .iter()
        .map(|&threads| Arm {
            threads,
            skip: (threads > ncpu).then(|| {
                Skip::skipped(format!(
                    "this host has {ncpu} cores, so {threads} threads would measure \
                     oversubscription rather than concurrency"
                ))
            }),
        })
        .collect()
}

/// Split a coordinate set into `threads` contiguous chunks.
///
/// Contiguous rather than interleaved, so each thread walks a run of the
/// sequence the way a client reading a region would, and so the concatenation
/// of the chunks is the original sequence. That last property is what makes the
/// T=1 arm the same work as `read_random` rather than merely a similar amount
/// of it.
pub fn chunks(coords: &[TileCoord], threads: usize) -> Vec<&[TileCoord]> {
    assert!(threads >= 1, "a thread ladder rung is at least one thread");
    if coords.is_empty() {
        return vec![&coords[0..0]; threads];
    }
    let base = coords.len() / threads;
    let remainder = coords.len() % threads;
    let mut out = Vec::with_capacity(threads);
    let mut at = 0;
    for index in 0..threads {
        let take = base + usize::from(index < remainder);
        out.push(&coords[at..at + take]);
        at += take;
    }
    out
}

/// What one rung of the ladder did.
#[derive(Debug, Clone)]
pub struct ArmRun {
    pub threads: usize,
    /// Every per-lookup latency, pooled across the threads.
    pub latencies: Vec<Duration>,
    /// The wall time of the whole arm, which is what a throughput is over.
    pub elapsed: Duration,
    /// Which threads actually ran lookups.
    ///
    /// The evidence for the T=1 control. An implementation that hands the T=1
    /// arm to a worker thread and joins it reports a thread id that is not the
    /// caller's, and the control stops being a control because it carries a
    /// spawn and a join the other passes do not.
    pub thread_ids: Vec<ThreadId>,
    pub hits: u64,
    pub coordinates_walked: usize,
}

impl ArmRun {
    pub fn lookups_per_s(&self) -> Option<f64> {
        let secs = self.elapsed.as_secs_f64();
        if secs <= 0.0 {
            return None;
        }
        Some(self.coordinates_walked as f64 / secs)
    }

    /// Whether the arm ran entirely on the thread that asked for it.
    pub fn ran_on_only(&self, thread: ThreadId) -> bool {
        self.thread_ids.iter().all(|id| *id == thread)
    }
}

/// Run one rung.
///
/// T=1 runs inline on the calling thread. Not "a pool of one": a scope, a spawn
/// and a join are real work, and a control that pays them is measuring the pool
/// as well as the lookups, which is exactly the thing the curve is supposed to
/// isolate.
pub fn run_arm(
    reader: &dyn TileReader,
    coords: &[TileCoord],
    threads: usize,
) -> Result<ArmRun, String> {
    let hits = AtomicU64::new(0);

    if threads == 1 {
        let started = Instant::now();
        let (latencies, thread_id, hit) = walk(reader, coords)?;
        let elapsed = started.elapsed();
        hits.fetch_add(hit, Ordering::Relaxed);
        return Ok(ArmRun {
            threads,
            latencies,
            elapsed,
            thread_ids: vec![thread_id],
            hits: hits.load(Ordering::Relaxed),
            coordinates_walked: coords.len(),
        });
    }

    let pieces = chunks(coords, threads);
    let started = Instant::now();
    // A thread that panics is a bug in this file rather than a condition the
    // archive can put the sweep in, so a join failure is reported as one
    // instead of unwinding the parent: `Scenario::run` turns it into a
    // `Skip::failed` and the rest of the sweep continues.
    let per_thread: Vec<Result<(Vec<Duration>, ThreadId, u64), String>> =
        std::thread::scope(|scope| {
            let handles: Vec<_> = pieces
                .iter()
                .map(|piece| scope.spawn(|| walk(reader, piece)))
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|_| Err("a lookup thread panicked".to_string()))
                })
                .collect()
        });
    let elapsed = started.elapsed();

    let mut latencies = Vec::with_capacity(coords.len());
    let mut thread_ids = Vec::with_capacity(threads);
    for outcome in per_thread {
        let (mut chunk_latencies, thread_id, hit) = outcome?;
        latencies.append(&mut chunk_latencies);
        thread_ids.push(thread_id);
        hits.fetch_add(hit, Ordering::Relaxed);
    }

    Ok(ArmRun {
        threads,
        latencies,
        elapsed,
        thread_ids,
        hits: hits.load(Ordering::Relaxed),
        coordinates_walked: coords.len(),
    })
}

fn walk(
    reader: &dyn TileReader,
    coords: &[TileCoord],
) -> Result<(Vec<Duration>, ThreadId, u64), String> {
    let mut latencies = Vec::with_capacity(coords.len());
    let mut hits = 0;
    for coord in coords {
        let at = Instant::now();
        let tile = reader.tile(*coord)?;
        latencies.push(at.elapsed());
        if tile.is_some() {
            hits += 1;
        }
    }
    Ok((latencies, std::thread::current().id(), hits))
}

/// `lookups_per_s(T) / (T * lookups_per_s(1))`.
///
/// `None` when either side is missing, because a scaling figure with an
/// invented denominator is worse than no scaling figure.
pub fn scaling_efficiency(threads: usize, at_t: Option<f64>, at_one: Option<f64>) -> Option<f64> {
    let (at_t, at_one) = (at_t?, at_one?);
    if at_one <= 0.0 {
        return None;
    }
    Some(at_t / (threads as f64 * at_one))
}

// ---------------------------------------------------------------------------
// The scenario, one per rung
// ---------------------------------------------------------------------------

/// `read_concurrent@T`: one rung of the ladder.
///
/// One `Scenario` per thread count rather than one scenario that loops, because
/// the document keys on the scenario name and a rung the host declined has to
/// be its own row carrying its own reason. A single scenario emitting four
/// series could not say "this host measured three of these and refused one".
pub struct Concurrent {
    pub threads: usize,
}

pub const LOOKUPS_PER_S: MetricSpec = MetricSpec {
    name: "lookups_per_s",
    unit: Unit::PerSecond,
    direction: Direction::HigherIsBetter,
};

pub const POOLED_P50: MetricSpec = MetricSpec {
    name: "p50",
    unit: Unit::Microseconds,
    direction: Direction::LowerIsBetter,
};

/// Every rung, in ladder order.
pub fn all() -> Vec<Box<dyn Scenario>> {
    THREAD_LADDER
        .iter()
        .map(|&threads| Box::new(Concurrent { threads }) as Box<dyn Scenario>)
        .collect()
}

impl Scenario for Concurrent {
    fn name(&self) -> String {
        format!("read_concurrent@{}", self.threads)
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
        LOOKUPS_PER_S
    }

    fn series(&self) -> Vec<MetricSpec> {
        vec![LOOKUPS_PER_S, POOLED_P50]
    }

    fn run(&self, ctx: &ScenarioContext<'_>, reps: u32) -> Result<ScenarioRun, Skip> {
        let ncpu = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let rung = ladder(ncpu)
            .into_iter()
            .find(|arm| arm.threads == self.threads);
        if let Some(skip) = rung.and_then(|arm| arm.skip) {
            // Declined, not dropped. The row stays in the document carrying the
            // reason, because an absent row reads as a measurement nobody took.
            return Err(skip);
        }

        let coords = &ctx.coords.random;
        if coords.is_empty() {
            return Err(Skip::skipped("the cell has no coordinates to walk"));
        }
        let reader = ctx.readers.fresh().map_err(Skip::failed)?;

        let mut discarded = Vec::new();
        for _ in 0..self.warmup().map(|w| w.passes).unwrap_or(0) {
            let arm = run_arm(reader.as_ref(), coords, self.threads).map_err(Skip::failed)?;
            if let Some(rate) = arm.lookups_per_s() {
                discarded.push(rate);
            }
        }

        let mut rates = Vec::new();
        let mut p50s = Vec::new();
        for _ in 0..reps.max(1) {
            let arm = run_arm(reader.as_ref(), coords, self.threads).map_err(Skip::failed)?;
            let Some(rate) = arm.lookups_per_s() else {
                return Err(Skip::failed("a pass took no measurable time"));
            };
            rates.push(rate);
            let mut micros: Vec<f64> = arm
                .latencies
                .iter()
                .map(|d| d.as_secs_f64() * 1e6)
                .collect();
            micros.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            if micros.is_empty() {
                return Err(Skip::failed("a pass produced no latencies"));
            }
            p50s.push(micros[micros.len() / 2]);
        }

        Ok(ScenarioRun {
            series: vec![
                Series {
                    metric: LOOKUPS_PER_S,
                    samples: rates,
                },
                Series {
                    metric: POOLED_P50,
                    samples: p50s,
                },
            ],
            reps: vec![RepFacts::default(); reps.max(1) as usize],
            discarded_warmup: discarded,
            peak_rss_bytes: None,
            heap_peak_bytes: None,
        })
    }
}
