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
use libviprs::pyramid_reader::PyramidReader;

use super::Outcome;

/// The thread counts every sweep reports, whether or not the host can run them.
pub const THREAD_LADDER: [usize; 4] = [1, 2, 4, 8];

/// One rung of the ladder and what this host did with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm {
    pub threads: usize,
    pub outcome: Outcome,
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
            outcome: if threads <= ncpu {
                Outcome::Ok
            } else {
                Outcome::skipped(format!(
                    "this host has {ncpu} cores, so {threads} threads would measure \
                     oversubscription rather than concurrency"
                ))
            },
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
pub fn run_arm(reader: &dyn PyramidReader, coords: &[TileCoord], threads: usize) -> ArmRun {
    let hits = AtomicU64::new(0);

    if threads == 1 {
        let started = Instant::now();
        let (latencies, thread_id, hit) = walk(reader, coords);
        let elapsed = started.elapsed();
        hits.fetch_add(hit, Ordering::Relaxed);
        return ArmRun {
            threads,
            latencies,
            elapsed,
            thread_ids: vec![thread_id],
            hits: hits.load(Ordering::Relaxed),
            coordinates_walked: coords.len(),
        };
    }

    let pieces = chunks(coords, threads);
    let started = Instant::now();
    let per_thread: Vec<(Vec<Duration>, ThreadId, u64)> = std::thread::scope(|scope| {
        let handles: Vec<_> = pieces
            .iter()
            .map(|piece| scope.spawn(|| walk(reader, piece)))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("a lookup thread does not panic"))
            .collect()
    });
    let elapsed = started.elapsed();

    let mut latencies = Vec::with_capacity(coords.len());
    let mut thread_ids = Vec::with_capacity(threads);
    for (mut chunk_latencies, thread_id, hit) in per_thread {
        latencies.append(&mut chunk_latencies);
        thread_ids.push(thread_id);
        hits.fetch_add(hit, Ordering::Relaxed);
    }

    ArmRun {
        threads,
        latencies,
        elapsed,
        thread_ids,
        hits: hits.load(Ordering::Relaxed),
        coordinates_walked: coords.len(),
    }
}

fn walk(reader: &dyn PyramidReader, coords: &[TileCoord]) -> (Vec<Duration>, ThreadId, u64) {
    let mut latencies = Vec::with_capacity(coords.len());
    let mut hits = 0;
    for coord in coords {
        let at = Instant::now();
        let tile = reader.tile(*coord).expect("a lookup succeeds");
        latencies.push(at.elapsed());
        if tile.is_some() {
            hits += 1;
        }
    }
    (latencies, std::thread::current().id(), hits)
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
