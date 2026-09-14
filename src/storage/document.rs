//! The document a `storage` sweep writes, and the shape it cannot express.
//!
//! Three things are load bearing here and none of them is the field list.
//!
//! **A field called `median` holds a median.** The harness this replaces
//! measured one process per cell, once, and copied that single value into a
//! field called `median` with no `samples` beside it, so nothing downstream
//! could tell a real move from the noise floor. Here a cell carries
//! `samples[]` and every summary statistic is computed from it, so a
//! single-shot cell is a cell with one sample and says so.
//!
//! **An unmeasured column is `null`.** Never `0`, and never absent either: a
//! missing key and a null are different claims, so nothing in this module is
//! `skip_serializing_if`. The reason is `filesystem_entries`, the one column
//! this whole comparison exists to move. Lower is better on it, an archive
//! really costs `1`, and the old shape published `0` whenever the measurement
//! broke, which is the best possible score. A hole is a hole a consumer can
//! see.
//!
//! **Field order is the struct's order.** `serde` serialises a derived struct
//! in declaration order, which is why the document is built out of structs
//! rather than out of `serde_json::Map`: that map is a `BTreeMap` unless
//! `preserve_order` is on, and a round trip through it alphabetises every
//! object in the file. The feature is on so a consumer that parses the
//! document back into a `Value` reads the columns in the order the producer
//! wrote them, and [`CELL_FIELDS`] is what the shape guard checks against.

use serde::{Deserialize, Serialize};

use super::cells::{Backend, Cell, Profile, SEED};
use super::scenarios::{Invariants, Isolation, MetricSpec, Outcome, Warmup};
use super::stats::{self, Summary};

/// The shape of this document. Bump it when a field changes meaning.
///
/// Numbered per document family: this is version 1 of `libviprs-storage`,
/// unrelated to causl's version 1 of its own family, which is why the family
/// name travels next to it and the importer checks both.
pub const SCHEMA_VERSION: u32 = 1;

/// The family name an importer matches on before it reads anything else.
pub const FAMILY: &str = "libviprs-storage";

/// The one runner this family has.
pub const RUNNER: &str = "libviprs-storage";

/// Every top-level key, in order.
pub const DOCUMENT_FIELDS: [&str; 14] = [
    "schemaVersion",
    "family",
    "runner",
    "profile",
    "startedAt",
    "finishedAt",
    "runId",
    "measurement",
    "provenance",
    "cells",
    "invariants",
    "modelled",
    "replicate",
    "integrity",
];

/// Every key a cell carries, in order.
pub const CELL_FIELDS: [&str; 32] = [
    "backend",
    "scale",
    "source",
    "cell",
    "scenario",
    "metric",
    "key",
    "unit",
    "direction",
    "isolation",
    "warmup",
    "discardedWarmup",
    "reps",
    "minReps",
    "outcome",
    "reason",
    "samples",
    "median",
    "min",
    "max",
    "iqr",
    "cov",
    "ci95",
    "ciHalfWidthPct",
    "p95OfSamples",
    "tail",
    "timerSaturated",
    "steadyState",
    "confidence",
    "lowConfidenceReasons",
    "machineLoad",
    "invariants",
];

// ---------------------------------------------------------------------------
// The measurement block
// ---------------------------------------------------------------------------

/// How many timed repetitions each kind of scenario takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepsByKind {
    pub generate: u32,
    pub read: u32,
}

/// How the interval under `median` was computed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntervalSpec {
    pub statistic: String,
    pub method: String,
    pub level: f64,
    pub resamples: usize,
}

/// What the run declared about how it measured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    pub unit: String,
    pub isolation: String,
    pub reps: RepsByKind,
    #[serde(rename = "minReps")]
    pub min_reps: RepsByKind,
    pub seed: String,
    pub clock: String,
    #[serde(rename = "timerTickNs")]
    pub timer_tick_ns: Option<f64>,
    #[serde(rename = "timerCallNs")]
    pub timer_call_ns: Option<f64>,
    #[serde(rename = "minTicksPerSample")]
    pub min_ticks_per_sample: f64,
    /// The pass scenarios' policy. Fresh-process scenarios carry `null` on
    /// their own cell instead.
    pub warmup: Option<WarmupBlock>,
    pub interval: IntervalSpec,
    #[serde(rename = "tieBandPct")]
    pub tie_band_pct: f64,
    #[serde(rename = "covLowConfidence")]
    pub cov_low_confidence: f64,
    /// There is no forced GC to declare; the fresh process is the isolation.
    #[serde(rename = "freshProcessPerCell")]
    pub fresh_process_per_cell: bool,
    #[serde(rename = "pageCache")]
    pub page_cache: String,
}

impl Measurement {
    /// The declared measurement block for a profile, with the clock probed.
    pub fn probed(profile: Profile) -> Measurement {
        let probe = stats::probe_timer();
        let reps = RepsByKind {
            generate: generate_reps(profile),
            read: read_reps(profile),
        };
        Measurement {
            unit: "fresh-process-per-cell".to_string(),
            isolation: "subprocess-per-cell".to_string(),
            reps,
            min_reps: reps,
            seed: format!("{SEED:#018x}"),
            clock: "std::time::Instant".to_string(),
            timer_tick_ns: Some(probe.tick_ns),
            timer_call_ns: Some(probe.call_ns),
            min_ticks_per_sample: stats::MIN_TICKS_PER_SAMPLE,
            warmup: Some(WarmupBlock::from(Warmup::ONE_DISCARDED_PASS)),
            interval: IntervalSpec {
                statistic: "median".to_string(),
                method: "percentile-bootstrap".to_string(),
                level: stats::BOOTSTRAP_LEVEL,
                resamples: stats::BOOTSTRAP_RESAMPLES,
            },
            tie_band_pct: 3.0,
            cov_low_confidence: stats::COV_LOW_CONFIDENCE,
            fresh_process_per_cell: true,
            page_cache: "warm-unknown".to_string(),
        }
    }
}

/// Timed repetitions a `generate` scenario takes on a profile.
pub fn generate_reps(profile: Profile) -> u32 {
    match profile {
        Profile::Ci => 3,
        Profile::Full | Profile::Xl => 7,
    }
}

/// Timed repetitions a read scenario takes on a profile.
pub fn read_reps(profile: Profile) -> u32 {
    match profile {
        Profile::Ci => 3,
        Profile::Full | Profile::Xl => 20,
    }
}

/// The warm-up policy as the document carries it.
///
/// The policy is an owned `String` and not the `&'static str` the scenario
/// declares, because the document has to round trip and `serde` cannot fill a
/// borrowed field from owned input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarmupBlock {
    pub policy: String,
    pub passes: u32,
}

impl From<Warmup> for WarmupBlock {
    fn from(w: Warmup) -> WarmupBlock {
        WarmupBlock {
            policy: w.policy.to_string(),
            passes: w.passes,
        }
    }
}

// ---------------------------------------------------------------------------
// A cell
// ---------------------------------------------------------------------------

/// The machine's load while a cell was measured.
///
/// Every field is nullable because `/proc/loadavg` exists on Linux and nowhere
/// else this crate builds for, and an unknown load is not a quiet one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MachineLoad {
    pub cores: Option<usize>,
    #[serde(rename = "loadAvg1m")]
    pub load_avg_1m: Option<f64>,
    #[serde(rename = "contentionPerCore")]
    pub contention_per_core: Option<f64>,
    /// `None` where the load could not be read: unknown, not quiet.
    pub quiet: Option<bool>,
}

impl MachineLoad {
    /// Sample the load, where the platform has one to sample.
    pub fn sample() -> MachineLoad {
        let cores = std::thread::available_parallelism().map(|n| n.get()).ok();
        let load = std::fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok());
        let contention = match (load, cores) {
            (Some(l), Some(c)) if c > 0 => Some(l / c as f64),
            _ => None,
        };
        MachineLoad {
            cores,
            load_avg_1m: load,
            contention_per_core: contention,
            quiet: contention.map(|c| c < 1.0),
        }
    }

    pub fn unknown() -> MachineLoad {
        MachineLoad {
            cores: None,
            load_avg_1m: None,
            contention_per_core: None,
            quiet: None,
        }
    }
}

/// The tail statistic a cell published, and the name it earned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TailBlock {
    /// `"p99"` or `"max"`.
    pub statistic: String,
    pub value: f64,
}

/// The invariants a cell observed, as they reach the document.
///
/// Separate from [`Invariants`] only because the JSON names are the
/// document's rather than Rust's.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InvariantBlock {
    #[serde(rename = "outputBytes")]
    pub output_bytes: Option<u64>,
    #[serde(rename = "allocatedBytes")]
    pub allocated_bytes: Option<u64>,
    #[serde(rename = "filesystemEntries")]
    pub filesystem_entries: Option<u64>,
    pub directories: Option<u64>,
    #[serde(rename = "tilesProduced")]
    pub tiles_produced: Option<u64>,
    #[serde(rename = "artefactDigest")]
    pub artefact_digest: Option<String>,
    #[serde(rename = "rootEntries")]
    pub root_entries: Option<u64>,
    pub leaves: Option<u64>,
    pub requests: Option<u64>,
    #[serde(rename = "requestBytes")]
    pub request_bytes: Option<u64>,
    #[serde(rename = "peakRssMb")]
    pub peak_rss_mb: Option<f64>,
    #[serde(rename = "heapPeakBytes")]
    pub heap_peak_bytes: Option<u64>,
}

impl From<&Invariants> for InvariantBlock {
    fn from(i: &Invariants) -> InvariantBlock {
        InvariantBlock {
            output_bytes: i.output_bytes,
            allocated_bytes: i.allocated_bytes,
            filesystem_entries: i.filesystem_entries,
            directories: i.directories,
            tiles_produced: i.tiles_produced,
            artefact_digest: i.artefact_digest.clone(),
            root_entries: i.root_entries,
            leaves: i.leaves,
            requests: i.requests,
            request_bytes: i.request_bytes,
            peak_rss_mb: None,
            heap_peak_bytes: None,
        }
    }
}

/// One `(backend, cell, scenario, metric)` row, with its samples.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentCell {
    pub backend: String,
    /// The facet key: the cell's tile count. Never megapixels, never canvas
    /// size.
    pub scale: u32,
    pub source: String,
    pub cell: String,
    pub scenario: String,
    pub metric: String,
    /// `<scenario>.<metric>`, which is what the page sections on.
    pub key: String,
    pub unit: String,
    pub direction: String,
    pub isolation: String,
    /// `null` on a scenario that measures from its first repetition.
    pub warmup: Option<WarmupBlock>,
    /// The primary-metric values of the discarded passes. Present so a reader
    /// can check they are not in `samples`.
    #[serde(rename = "discardedWarmup")]
    pub discarded_warmup: Vec<f64>,
    pub reps: u32,
    #[serde(rename = "minReps")]
    pub min_reps: u32,
    pub outcome: String,
    pub reason: Option<String>,
    /// One per timed repetition, in the order they ran.
    pub samples: Vec<f64>,
    pub median: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub iqr: Option<f64>,
    pub cov: Option<f64>,
    pub ci95: Option<[f64; 2]>,
    #[serde(rename = "ciHalfWidthPct")]
    pub ci_half_width_pct: Option<f64>,
    #[serde(rename = "p95OfSamples")]
    pub p95_of_samples: Option<f64>,
    pub tail: Option<TailBlock>,
    #[serde(rename = "timerSaturated")]
    pub timer_saturated: Option<bool>,
    /// The aggregator computes this from the published samples. The runner
    /// never asserts it, so it leaves here as `null`.
    #[serde(rename = "steadyState")]
    pub steady_state: Option<String>,
    pub confidence: String,
    #[serde(rename = "lowConfidenceReasons")]
    pub low_confidence_reasons: Vec<String>,
    #[serde(rename = "machineLoad")]
    pub machine_load: MachineLoad,
    pub invariants: InvariantBlock,
}

/// What a cell needs to become a row.
pub struct CellReport<'a> {
    pub backend: Backend,
    pub cell: Cell,
    pub scenario: &'a str,
    pub metric: MetricSpec,
    pub isolation: Isolation,
    pub warmup: Option<Warmup>,
    pub discarded_warmup: Vec<f64>,
    pub reps_declared: u32,
    pub min_reps: u32,
    pub samples: Vec<f64>,
    pub outcome: Outcome,
    pub reason: Option<String>,
    pub invariants: InvariantBlock,
    pub machine_load: MachineLoad,
    pub timer: Option<stats::TimerProbe>,
}

impl DocumentCell {
    /// Build a row, computing every summary statistic from the samples.
    ///
    /// There is no way in to set `median` directly, which is the point: a
    /// cell with one sample publishes a one-element `samples` array and a
    /// median of that one element, and a reader can see which it is.
    pub fn from_report(report: CellReport<'_>) -> DocumentCell {
        let summary: Option<Summary> = stats::summarise(&report.samples, SEED);
        let mut reasons: Vec<String> = Vec::new();
        if (report.samples.len() as u32) < report.min_reps {
            reasons.push(format!(
                "fewer than minReps: {} of {}",
                report.samples.len(),
                report.min_reps
            ));
        }
        if let Some(cov) = summary.as_ref().and_then(|s| s.cov) {
            if cov > stats::COV_LOW_CONFIDENCE {
                reasons.push(format!("cov {cov:.3} above {}", stats::COV_LOW_CONFIDENCE));
            }
        }
        if report.machine_load.quiet != Some(true) {
            reasons.push(match report.machine_load.quiet {
                Some(false) => "machine not quiet".to_string(),
                _ => "machine load unknown".to_string(),
            });
        }
        let timer_saturated = match (summary.as_ref(), report.timer, report.metric.unit) {
            (Some(s), Some(probe), super::scenarios::Unit::Microseconds) => {
                Some(stats::timer_saturated(s.median * 1000.0, probe))
            }
            (Some(s), Some(probe), super::scenarios::Unit::Milliseconds) => {
                Some(stats::timer_saturated(s.median * 1_000_000.0, probe))
            }
            _ => None,
        };
        if timer_saturated == Some(true) {
            reasons.push("timer saturated".to_string());
        }
        let confidence = if reasons.is_empty() { "high" } else { "low" };

        DocumentCell {
            backend: report.backend.as_str().to_string(),
            scale: report.cell.declared_tiles,
            source: report.cell.source.as_str().to_string(),
            cell: report.cell.spec(),
            scenario: report.scenario.to_string(),
            metric: report.metric.name.to_string(),
            key: format!("{}.{}", report.scenario, report.metric.name),
            unit: report.metric.unit.as_str().to_string(),
            direction: report.metric.direction.as_str().to_string(),
            isolation: report.isolation.as_str().to_string(),
            warmup: report.warmup.map(WarmupBlock::from),
            discarded_warmup: report.discarded_warmup,
            reps: report.reps_declared,
            min_reps: report.min_reps,
            outcome: report.outcome.as_str().to_string(),
            reason: report.reason,
            median: summary.as_ref().map(|s| s.median),
            min: summary.as_ref().map(|s| s.min),
            max: summary.as_ref().map(|s| s.max),
            iqr: summary.as_ref().map(|s| s.iqr),
            cov: summary.as_ref().and_then(|s| s.cov),
            ci95: summary.as_ref().map(|s| [s.ci95.0, s.ci95.1]),
            ci_half_width_pct: summary.as_ref().and_then(|s| s.ci_half_width_pct),
            p95_of_samples: summary.as_ref().map(|s| s.p95_of_samples),
            tail: summary.as_ref().map(|s| TailBlock {
                statistic: s.tail.kind.as_str().to_string(),
                value: s.tail.value,
            }),
            timer_saturated,
            steady_state: None,
            confidence: confidence.to_string(),
            low_confidence_reasons: reasons,
            machine_load: report.machine_load,
            invariants: report.invariants,
            samples: report.samples,
        }
    }
}

// ---------------------------------------------------------------------------
// The page's two flat tables
// ---------------------------------------------------------------------------

/// One invariant, flattened for the page's equality table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvariantEntry {
    pub library: String,
    pub scale: u32,
    pub source: String,
    pub name: String,
    pub value: serde_json::Value,
    pub unit: String,
}

/// One modelled quantity. Never charted on an axis carrying a measured one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelledEntry {
    pub library: String,
    pub scale: u32,
    pub name: String,
    pub value: f64,
    pub unit: String,
    pub model: serde_json::Value,
}

/// The replicate control: the cell measured first and last in a sweep, and how
/// far its metrics moved between the two.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Replicate {
    pub cell: String,
    #[serde(rename = "replicateReps")]
    pub replicate_reps: u32,
    #[serde(rename = "spreadPct")]
    pub spread_pct: serde_json::Value,
}

// ---------------------------------------------------------------------------
// The document
// ---------------------------------------------------------------------------

/// What a sweep writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub family: String,
    pub runner: String,
    pub profile: String,
    #[serde(rename = "startedAt")]
    pub started_at: String,
    #[serde(rename = "finishedAt")]
    pub finished_at: Option<String>,
    /// `<startedAt>-<libviprsCommit>-<host8>`, derived by the aggregator from
    /// the provenance it refuses to guess. K1.3 fills it.
    #[serde(rename = "runId")]
    pub run_id: Option<String>,
    pub measurement: Measurement,
    /// K1.3's slot: library and harness commits, emulation, filesystem,
    /// cgroups, toolchain, dependencies.
    pub provenance: Option<serde_json::Value>,
    pub cells: Vec<DocumentCell>,
    pub invariants: Vec<InvariantEntry>,
    pub modelled: Vec<ModelledEntry>,
    pub replicate: Option<Replicate>,
    /// K1.3's slot: the four sha256 digests over canonical JSON.
    pub integrity: Option<serde_json::Value>,
}

impl Document {
    pub fn new(profile: Profile, started_at: String) -> Document {
        Document {
            schema_version: SCHEMA_VERSION,
            family: FAMILY.to_string(),
            runner: RUNNER.to_string(),
            profile: profile.label().to_string(),
            started_at,
            finished_at: None,
            run_id: None,
            measurement: Measurement::probed(profile),
            provenance: None,
            cells: Vec::new(),
            invariants: Vec::new(),
            modelled: Vec::new(),
            replicate: None,
            integrity: None,
        }
    }

    pub fn push(&mut self, cell: DocumentCell) {
        self.cells.push(cell);
    }

    /// Rebuild the flat invariant table from the cells.
    ///
    /// One entry per `(backend, scale, source, name)` with a measured value.
    /// A `None` invariant contributes no row rather than a row holding a zero.
    pub fn rebuild_invariant_table(&mut self) {
        let mut out: Vec<InvariantEntry> = Vec::new();
        let mut seen: Vec<(String, u32, String, String)> = Vec::new();
        for cell in &self.cells {
            let inv = &cell.invariants;
            let mut push = |name: &str, value: Option<serde_json::Value>, unit: &str| {
                let Some(value) = value else { return };
                let key = (
                    cell.backend.clone(),
                    cell.scale,
                    cell.source.clone(),
                    name.to_string(),
                );
                if seen.contains(&key) {
                    return;
                }
                seen.push(key);
                out.push(InvariantEntry {
                    library: cell.backend.clone(),
                    scale: cell.scale,
                    source: cell.source.clone(),
                    name: name.to_string(),
                    value,
                    unit: unit.to_string(),
                });
            };
            push(
                "output_bytes",
                inv.output_bytes.map(Into::into),
                Unit_BYTES,
            );
            push(
                "allocated_bytes",
                inv.allocated_bytes.map(Into::into),
                Unit_BYTES,
            );
            push(
                "filesystem_entries",
                inv.filesystem_entries.map(Into::into),
                Unit_COUNT,
            );
            push("directories", inv.directories.map(Into::into), Unit_COUNT);
            push(
                "tiles_produced",
                inv.tiles_produced.map(Into::into),
                Unit_COUNT,
            );
            push(
                "artefact_digest",
                inv.artefact_digest.clone().map(serde_json::Value::String),
                "sha256",
            );
            push("root_entries", inv.root_entries.map(Into::into), Unit_COUNT);
            push("leaves", inv.leaves.map(Into::into), Unit_COUNT);
            push("requests", inv.requests.map(Into::into), Unit_COUNT);
            push(
                "request_bytes",
                inv.request_bytes.map(Into::into),
                Unit_BYTES,
            );
        }
        self.invariants = out;
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("the document serialises")
    }
}

#[allow(non_upper_case_globals)]
const Unit_BYTES: &str = "bytes";
#[allow(non_upper_case_globals)]
const Unit_COUNT: &str = "count";
