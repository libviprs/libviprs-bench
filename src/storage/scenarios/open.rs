//! The cold open, on its own and split into the six things it is.
//!
//! The old `read_cold` row opened a brand new reader for each of its lookups
//! and timed the open together with the lookup, so one number carried a file
//! open, a header read, a ranged read, a gzip inflate, four varint passes and a
//! lookup. The fix for a slow inflate, a slow varint loop and a slow `pread`
//! are three different pieces of work, and the combined row cannot say which
//! one is the problem (libviprs#1021).
//!
//! libviprs PR #1022 split it in the engine's own harness and measured the
//! result: 98% of a cold open is the inflate plus the root decode, and the
//! decode is 13 ns an entry on both architectures (libviprs#1023). This is that
//! split, re-homed where the producer lives. The walk is by hand through the
//! public API in the order `Reader::try_new` runs its steps, exactly so that
//! nothing on the product's hot path has to change to be measured.
//!
//! # The phases have to add up
//!
//! Six numbers that do not reconcile with the combined row are six numbers
//! about some other piece of work. [`reconciles`] is the check, and
//! [`reconciliation_is_meaningful`] is the part the engine's guard learned the
//! hard way: on the 93-entry cell the split drifts to **-24% on arm64 and -34%
//! on x86_64**, because the whole open there is 11 us and a few hundred
//! nanoseconds of per-iteration overhead is a fifth of it. That is not a broken
//! split, it is a cell too small to reconcile on, so the guard refuses it
//! instead of loosening its allowance until everything passes.

use std::path::Path;
use std::time::{Duration, Instant};

use super::super::cells::{Backend, Profile};
use super::super::document::read_reps;
use super::counting::{CountingFactory, Request};
use super::{
    Direction, Invariants, Isolation, MetricSpec, ReaderFactory, RepFacts, Scenario,
    ScenarioContext, ScenarioRun, Series, Skip, TileReader, Unit, Warmup,
};
use libviprs::planner::TileCoord;
use libviprs::pmtiles::directory::deserialize_entries;
use libviprs::pmtiles::header::HEADER_BYTES;
use libviprs::pmtiles::reader::MAX_DIRECTORY_BYTES;
use libviprs::pmtiles::{FileRangeReader, Header, RangeReader};

/// The phases a cold PMTiles open goes through, in the order `Reader::try_new`
/// runs them, plus the lookup that follows.
pub const COLD_PHASES: [&str; 6] = [
    "open_file",
    "open_header",
    "open_root_fetch",
    "open_root_inflate",
    "open_root_decode",
    "open_lookup",
];

/// How far the six phases may drift from the combined row before the split is
/// measuring something else.
///
/// 25%, which is the allowance the engine's guard settled on, and it is wide
/// because the combined row runs one open per iteration while the split runs
/// two (the lookup phase needs a reader the crate built, and building it is
/// real work this file does not time).
pub const RECONCILIATION_ALLOWANCE_PCT: f64 = 25.0;

/// The smallest root a reconciliation check may be run against.
///
/// Under about a thousand entries the root stops dominating the open and
/// per-iteration overhead starts to. The engine's guard measures a cell of
/// about 1400 entries for exactly this reason, and the 93-entry cell drifts
/// -24% and -34% on the two architectures with nothing wrong with the split at
/// all. Any new tiny cell inherits that, so the guard says no rather than
/// widening until it passes.
pub const MIN_RECONCILABLE_ROOT_ENTRIES: u64 = 1_000;

// ---------------------------------------------------------------------------
// What an open costs in requests
// ---------------------------------------------------------------------------

/// What one open asked the byte source for.
#[derive(Debug, Clone)]
pub struct OpenObservation {
    /// Every range the reader fetched while constructing itself, oldest first.
    pub requests: Vec<Request>,
    /// Entries in the root the open decoded, as the archive answers it.
    pub root_entries: u64,
    /// Where the archive's tile data section starts.
    pub tile_data_offset: u64,
    /// How long that section is.
    pub tile_data_length: u64,
}

impl OpenObservation {
    pub fn request_count(&self) -> u64 {
        self.requests.len() as u64
    }

    pub fn bytes(&self) -> u64 {
        self.requests.iter().map(|r| r.len as u64).sum()
    }

    /// The requests that touched the tile data section, which an open should
    /// have none of.
    pub fn tile_section_requests(&self) -> Vec<Request> {
        let end = self.tile_data_offset + self.tile_data_length;
        self.requests
            .iter()
            .copied()
            .filter(|r| r.offset < end && r.end() > self.tile_data_offset)
            .collect()
    }
}

/// Construct a reader over the archive and answer what that cost, without
/// looking a tile up.
///
/// This is the scenario: the old cold row conflated the open with the lookup,
/// and a client that opens an archive pays the open whether or not it goes on
/// to read anything.
pub fn observe(readers: &CountingFactory) -> Result<OpenObservation, String> {
    let reader = readers.fresh_counting()?;
    let (offset, length) = reader.tile_data_range();
    Ok(OpenObservation {
        requests: reader.open_requests().to_vec(),
        root_entries: reader.root_entries(),
        tile_data_offset: offset,
        tile_data_length: length,
    })
}

/// Open, then look one tile up, counting the two separately.
///
/// The positive control for [`observe`]: if an open lands no request in the
/// tile data section because nothing is counted at all, this one lands none
/// either and the assertion is vacuous. Here the lookup must land one.
pub fn observe_with_lookup(
    readers: &CountingFactory,
    coord: TileCoord,
) -> Result<(OpenObservation, Vec<Request>), String> {
    let reader = readers.fresh_counting()?;
    let (offset, length) = reader.tile_data_range();
    let observation = OpenObservation {
        requests: reader.open_requests().to_vec(),
        root_entries: reader.root_entries(),
        tile_data_offset: offset,
        tile_data_length: length,
    };
    let (looked, lookup) = reader.counted(|r| r.tile(coord));
    looked?;
    Ok((observation, lookup))
}

// ---------------------------------------------------------------------------
// The six phases
// ---------------------------------------------------------------------------

/// One iteration of the split, phase by phase, in `COLD_PHASES` order.
#[derive(Debug, Clone, Copy)]
pub struct SplitSample {
    pub phases: [Duration; 6],
}

impl SplitSample {
    pub fn total(&self) -> Duration {
        self.phases.iter().copied().sum()
    }
}

/// A pass of the split, and the combined open measured alongside it.
#[derive(Debug, Clone)]
pub struct SplitPass {
    pub samples: Vec<SplitSample>,
    /// The same work measured the way the old row measured it: one open plus
    /// one lookup, timed as a single number. It is what the split has to
    /// reconcile with.
    pub combined: Vec<Duration>,
    /// Entries the root held, as the archive answers it.
    pub root_entries: u64,
}

impl SplitPass {
    /// Per-phase samples, in `COLD_PHASES` order.
    pub fn by_phase(&self) -> Vec<Vec<Duration>> {
        (0..COLD_PHASES.len())
            .map(|index| self.samples.iter().map(|s| s.phases[index]).collect())
            .collect()
    }

    pub fn totals(&self) -> Vec<Duration> {
        self.samples.iter().map(SplitSample::total).collect()
    }
}

/// Walk `Reader::try_new`'s own steps by hand, timing each one, then time a
/// lookup through a reader the crate built.
///
/// Doing it by hand rather than instrumenting the engine keeps this a
/// measurement: nothing on the product's hot path changes to be measured, and
/// the check that the hand-rolled walk really is the same work is that its
/// phases reconcile with the combined row.
pub fn split_pass(
    archive: &Path,
    readers: &dyn ReaderFactory,
    coords: &[TileCoord],
    root_entries: u64,
) -> Result<SplitPass, String> {
    let mut samples = Vec::with_capacity(coords.len());
    let mut combined = Vec::with_capacity(coords.len());

    for coord in coords {
        let at = Instant::now();
        let source = FileRangeReader::try_open(archive).map_err(|e| format!("the archive does not open: {e}"))?;
        std::hint::black_box(source.size().map_err(|e| format!("the archive has no size: {e}"))?);
        let open = at.elapsed();

        let at = Instant::now();
        let header = Header::try_decode(
            &source
                .read_range(0, HEADER_BYTES)
                .map_err(|e| format!("the header cannot be read: {e}"))?,
        )
        .map_err(|e| format!("the header does not decode: {e}"))?;
        let header_time = at.elapsed();

        let at = Instant::now();
        let raw = source
            .read_range(
                header.root_offset,
                usize::try_from(header.root_length)
                    .map_err(|_| "the root length does not fit a usize".to_string())?,
            )
            .map_err(|e| format!("the root cannot be read: {e}"))?;
        let fetch = at.elapsed();

        let at = Instant::now();
        let plain = header
            .internal_compression
            .decompress(&raw, MAX_DIRECTORY_BYTES)
            .map_err(|e| format!("the root does not inflate: {e}"))?;
        let inflate = at.elapsed();

        let at = Instant::now();
        let entries = std::hint::black_box(
            deserialize_entries(&plain).map_err(|e| format!("the root does not decode: {e}"))?,
        );
        let decode = at.elapsed();
        if entries.is_empty() {
            return Err("a root of no entries is not a root".to_string());
        }

        // Untimed on purpose: the lookup phase has to run against a reader the
        // crate built, because that is the path a caller takes, and it comes
        // from the factory like every other reader in the family. Only the five
        // index phases above are walked by hand, and they have to be: taking
        // `Reader::try_new` apart is the measurement.
        let reader = readers.fresh()?;
        let at = Instant::now();
        std::hint::black_box(reader.tile(*coord)?);
        let lookup = at.elapsed();

        samples.push(SplitSample {
            phases: [open, header_time, fetch, inflate, decode, lookup],
        });

        // And the same work as one number, which is what the split reconciles
        // against.
        let at = Instant::now();
        let whole = readers.fresh()?;
        std::hint::black_box(whole.tile(*coord)?);
        combined.push(at.elapsed());
    }

    Ok(SplitPass {
        samples,
        combined,
        root_entries,
    })
}

// ---------------------------------------------------------------------------
// Reconciliation
// ---------------------------------------------------------------------------

/// Whether a cell's root is big enough for a reconciliation check to say
/// anything.
///
/// `Err` carries the reason, which goes into the document as the outcome's
/// reason rather than being swallowed. A cell that cannot reconcile still
/// publishes its phases; what it does not do is claim they were checked.
pub fn reconciliation_is_meaningful(root_entries: u64) -> Result<(), String> {
    if root_entries >= MIN_RECONCILABLE_ROOT_ENTRIES {
        return Ok(());
    }
    Err(format!(
        "a root of {root_entries} entries is under the {MIN_RECONCILABLE_ROOT_ENTRIES} this check \
         needs: the whole open there is a few microseconds, per-iteration overhead is a fifth of \
         it, and the split drifts -24% on arm64 and -34% on x86_64 with nothing wrong with it"
    ))
}

/// How far the summed phases sit from the combined row, as a percentage of the
/// combined row. Negative means the split came out lower.
pub fn drift_pct(split_us: f64, combined_us: f64) -> f64 {
    (split_us - combined_us) / combined_us * 100.0
}

/// Whether the summed phases reconcile with the combined row.
pub fn reconciles(split_us: f64, combined_us: f64) -> bool {
    drift_pct(split_us, combined_us).abs() <= RECONCILIATION_ALLOWANCE_PCT
}

// ---------------------------------------------------------------------------
// The scenario
// ---------------------------------------------------------------------------

/// `open`: construct a reader and stop.
///
/// One repetition is one fresh process, so the open it measures is the open a
/// client pays: cold reader, cold branch predictors, nothing warmed by a
/// previous lookup. That is why `read_cold` was not this measurement even
/// though it opened a reader per lookup.
pub struct Open;

/// Microseconds for the open itself.
pub const OPEN_US: MetricSpec = MetricSpec {
    name: "p50",
    unit: Unit::Microseconds,
    direction: Direction::LowerIsBetter,
};

impl Scenario for Open {
    fn name(&self) -> String {
        "open".to_string()
    }

    fn isolation(&self) -> Isolation {
        Isolation::ProcessPerRep
    }

    fn warmup(&self) -> Option<Warmup> {
        // A fresh process per repetition is the warm-up; there is no in-process
        // state for a discarded pass to warm, and discarding one would throw
        // away the only cold open the process has.
        None
    }

    fn reps(&self, profile: Profile) -> u32 {
        read_reps(profile)
    }

    fn primary(&self) -> MetricSpec {
        OPEN_US
    }

    fn run(&self, ctx: &ScenarioContext<'_>, reps: u32) -> Result<ScenarioRun, Skip> {
        let mut samples = Vec::new();
        let mut facts = Vec::new();
        for _ in 0..reps.max(1) {
            let at = Instant::now();
            let reader = ctx.readers.fresh().map_err(Skip::failed)?;
            let micros = at.elapsed().as_secs_f64() * 1e6;
            std::hint::black_box(&reader);
            samples.push(micros);

            // The request count is an invariant rather than a timing, and it is
            // observed through a counting factory, which is another
            // `ReaderFactory` and not a reader this scenario built.
            let mut invariants = Invariants::default();
            if ctx.backend == Backend::PmTiles
                && let Some(archive) = ctx.artefact
            {
                let counting = CountingFactory::new(archive);
                let seen = observe(&counting).map_err(Skip::failed)?;
                invariants.requests = Some(seen.request_count());
                invariants.request_bytes = Some(seen.bytes());
                invariants.root_entries = Some(seen.root_entries);
                if !seen.tile_section_requests().is_empty() {
                    return Err(Skip::failed(format!(
                        "the open fetched {} range(s) from the tile data section, so it is not an \
                         index-only open",
                        seen.tile_section_requests().len()
                    )));
                }
            }
            facts.push(RepFacts {
                invariants,
                scratch: None,
            });
        }

        Ok(ScenarioRun {
            series: vec![Series {
                metric: OPEN_US,
                samples,
            }],
            reps: facts,
            discarded_warmup: Vec::new(),
            peak_rss_bytes: None,
            heap_peak_bytes: None,
        })
    }
}

/// `first_lookup`: construct a reader and ask it for one tile.
///
/// The other half of the old `read_cold` row. `open` prices what a client pays
/// before it can ask anything; this prices what it pays to get its first
/// answer, and the difference between the two is the lookup.
pub struct FirstLookup;

impl Scenario for FirstLookup {
    fn name(&self) -> String {
        "first_lookup".to_string()
    }

    fn isolation(&self) -> Isolation {
        Isolation::ProcessPerRep
    }

    fn warmup(&self) -> Option<Warmup> {
        None
    }

    fn reps(&self, profile: Profile) -> u32 {
        read_reps(profile)
    }

    fn primary(&self) -> MetricSpec {
        OPEN_US
    }

    fn run(&self, ctx: &ScenarioContext<'_>, reps: u32) -> Result<ScenarioRun, Skip> {
        let Some(coord) = ctx.coords.root_addressed else {
            return Err(Skip::skipped("the cell has no root-addressed coordinate"));
        };
        let mut samples = Vec::new();
        for _ in 0..reps.max(1) {
            let at = Instant::now();
            let reader = ctx.readers.fresh().map_err(Skip::failed)?;
            let tile = reader.tile(coord).map_err(Skip::failed)?;
            samples.push(at.elapsed().as_secs_f64() * 1e6);
            std::hint::black_box(tile);
        }
        Ok(ScenarioRun {
            series: vec![Series {
                metric: OPEN_US,
                samples,
            }],
            reps: vec![RepFacts::default(); reps.max(1) as usize],
            discarded_warmup: Vec::new(),
            peak_rss_bytes: None,
            heap_peak_bytes: None,
        })
    }
}
