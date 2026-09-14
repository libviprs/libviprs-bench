//! Re-measure the varint loop instead of quoting it.
//!
//! The cold split attributes 85% of an open on arm64 and 72% on x86_64 to
//! `deserialize_entries`, and libviprs#1023 fits that at 12.65 ns an entry on
//! arm64 and 13.95 on x86_64. Those came out of the engine's own
//! micro-benchmark, and a published figure that nothing in the producing sweep
//! re-measures is a figure that goes stale silently the first time somebody
//! makes the varint loop cheaper.
//!
//! So this scenario decodes the archive's real root bytes, in memory, and
//! reports what it decoded. What makes it a measurement rather than a
//! restatement is that the entry count comes back out of the decode: on a
//! source whose neighbouring tiles share a payload the writer's run-length
//! encoding collapses a 16369-tile plan into a root of 261 entries, and this
//! scenario says 261.

use std::path::Path;
use std::time::{Duration, Instant};

use libviprs::pmtiles::directory::deserialize_entries;
use libviprs::pmtiles::header::HEADER_BYTES;

use super::super::cells::{Backend, Profile};
use super::super::document::read_reps;
use super::{
    Direction, Invariants, Isolation, MetricSpec, RepFacts, Scenario, ScenarioContext, ScenarioRun,
    Series, Skip, Unit, Warmup,
};
use libviprs::pmtiles::reader::MAX_DIRECTORY_BYTES;
use libviprs::pmtiles::{FileRangeReader, Header, RangeReader};

/// What a decode pass found and what it cost.
#[derive(Debug, Clone)]
pub struct DecodeRoot {
    /// Entries the decode produced. This is the archive's answer, not the
    /// cell's tile count.
    pub entries: u64,
    /// The root as stored, compressed.
    pub compressed_bytes: u64,
    /// The root after the inflate, which is what the varint loop walks.
    pub plain_bytes: u64,
    /// One sample per repetition.
    pub samples: Vec<Duration>,
}

impl DecodeRoot {
    /// Nanoseconds an entry, from this run's own samples.
    ///
    /// `None` when there is nothing to divide by, which is the only honest
    /// answer for a root of no entries and the reason this is not an `f64`
    /// that quietly becomes infinity.
    pub fn nanos_per_entry(&self, sample: Duration) -> Option<f64> {
        if self.entries == 0 {
            return None;
        }
        Some(sample.as_secs_f64() * 1e9 / self.entries as f64)
    }
}

/// Read the archive's root bytes once, then decode them `reps` times.
///
/// The fetch and the inflate are outside the timed section: they are their own
/// phases in [`super::open`], and the point of this scenario is the loop that
/// the split says costs the most.
pub fn observe(archive: &Path, reps: usize) -> Result<DecodeRoot, String> {
    let source = FileRangeReader::try_open(archive)
        .map_err(|e| format!("the archive does not open: {e}"))?;
    let header = Header::try_decode(
        &source
            .read_range(0, HEADER_BYTES)
            .map_err(|e| format!("the header cannot be read: {e}"))?,
    )
    .map_err(|e| format!("the header does not decode: {e}"))?;
    let raw = source
        .read_range(
            header.root_offset,
            usize::try_from(header.root_length)
                .map_err(|_| "the root length does not fit a usize".to_string())?,
        )
        .map_err(|e| format!("the root cannot be read: {e}"))?;
    let plain = header
        .internal_compression
        .decompress(&raw, MAX_DIRECTORY_BYTES)
        .map_err(|e| format!("the root does not inflate: {e}"))?;

    let mut samples = Vec::with_capacity(reps);
    let mut entries = 0u64;
    for _ in 0..reps {
        let at = Instant::now();
        let decoded = std::hint::black_box(
            deserialize_entries(&plain).map_err(|e| format!("the root does not decode: {e}"))?,
        );
        samples.push(at.elapsed());
        entries = decoded.len() as u64;
    }

    Ok(DecodeRoot {
        entries,
        compressed_bytes: raw.len() as u64,
        plain_bytes: plain.len() as u64,
        samples,
    })
}

// ---------------------------------------------------------------------------
// The scenario
// ---------------------------------------------------------------------------

/// `decode_root`: the varint loop, re-measured rather than quoted.
///
/// The tree has no root to decode, so on the directory backend this is
/// `skipped` with that as the reason rather than a zero. A zero would be the
/// best possible score on a lower-is-better column, published as a measurement,
/// which is the exact failure the `null`-never-`0` rule exists to stop.
pub struct DecodeRootScenario;

pub const DECODE_US: MetricSpec = MetricSpec {
    name: "p50",
    unit: Unit::Microseconds,
    direction: Direction::LowerIsBetter,
};

pub const NS_PER_ENTRY: MetricSpec = MetricSpec {
    name: "ns_per_entry",
    unit: Unit::Ratio,
    direction: Direction::LowerIsBetter,
};

impl Scenario for DecodeRootScenario {
    fn name(&self) -> String {
        "decode_root".to_string()
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
        DECODE_US
    }

    fn series(&self) -> Vec<MetricSpec> {
        vec![DECODE_US, NS_PER_ENTRY]
    }

    fn run(&self, ctx: &ScenarioContext<'_>, reps: u32) -> Result<ScenarioRun, Skip> {
        if ctx.backend != Backend::PmTiles {
            return Err(Skip::skipped(
                "a directory tree has no root directory to decode; its index work happens in the \
                 kernel one path resolution at a time",
            ));
        }
        let Some(archive) = ctx.artefact else {
            return Err(Skip::failed("no archive to decode"));
        };

        let warmup = self.warmup().map(|w| w.passes).unwrap_or(0);
        let discarded = if warmup > 0 {
            let pass = observe(archive, warmup as usize).map_err(Skip::failed)?;
            pass.samples
                .iter()
                .map(|d| d.as_secs_f64() * 1e6)
                .collect()
        } else {
            Vec::new()
        };

        let measured = observe(archive, reps.max(1) as usize).map_err(Skip::failed)?;
        let micros: Vec<f64> = measured
            .samples
            .iter()
            .map(|d| d.as_secs_f64() * 1e6)
            .collect();
        let per_entry: Vec<f64> = measured
            .samples
            .iter()
            .filter_map(|d| measured.nanos_per_entry(*d))
            .collect();

        let mut invariants = Invariants::default();
        invariants.root_entries = Some(measured.entries);
        let facts = vec![
            RepFacts {
                invariants,
                scratch: None,
            };
            micros.len()
        ];

        let mut series = vec![Series {
            metric: DECODE_US,
            samples: micros,
        }];
        if per_entry.len() == series[0].samples.len() {
            series.push(Series {
                metric: NS_PER_ENTRY,
                samples: per_entry,
            });
        }

        Ok(ScenarioRun {
            series,
            reps: facts,
            discarded_warmup: discarded,
            peak_rss_bytes: None,
            heap_peak_bytes: None,
        })
    }
}
