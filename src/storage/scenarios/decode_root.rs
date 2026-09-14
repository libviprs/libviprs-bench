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
use libviprs::pmtiles::reader::MAX_DIRECTORY_BYTES;
use libviprs::pmtiles::{FileRangeReader, Header, RangeReader};
use libviprs::pmtiles::header::HEADER_BYTES;

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
pub fn observe(archive: &Path, reps: usize) -> DecodeRoot {
    let source = FileRangeReader::try_open(archive).expect("the archive opens");
    let header = Header::try_decode(
        &source
            .read_range(0, HEADER_BYTES)
            .expect("the header can be read"),
    )
    .expect("the header decodes");
    let raw = source
        .read_range(
            header.root_offset,
            usize::try_from(header.root_length).expect("a root length fits a usize"),
        )
        .expect("the root can be read");
    let plain = header
        .internal_compression
        .decompress(&raw, MAX_DIRECTORY_BYTES)
        .expect("the root inflates");

    let mut samples = Vec::with_capacity(reps);
    let mut entries = 0u64;
    for _ in 0..reps {
        let at = Instant::now();
        let decoded = std::hint::black_box(deserialize_entries(&plain).expect("the root decodes"));
        samples.push(at.elapsed());
        entries = decoded.len() as u64;
    }

    DecodeRoot {
        entries,
        compressed_bytes: raw.len() as u64,
        plain_bytes: plain.len() as u64,
        samples,
    }
}
