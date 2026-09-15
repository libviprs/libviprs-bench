//! Dispersion, and the rule about which tail statistic a row is allowed to
//! publish.
//!
//! # Why a tail statistic has a name and a floor
//!
//! A nearest-rank p99 of 64 samples is `sorted[ceil(64 * 0.99) - 1]`, which is
//! `sorted[63]`, which is the maximum. It is not an estimate of the 99th
//! percentile, it is the largest sample wearing a percentile's name, and the
//! two behave nothing alike: a maximum is the noisiest statistic in the set
//! and it moves with the worst scheduling accident in the pass. The committed
//! sweeps show what that costs. Across the free replicate pair those exports
//! contain, p99 moves 74.5% where wall moves 10.7% and p50 moves 7.1%, on an
//! idle host running identical code.
//!
//! So [`tail`] refuses to call anything a p99 under [`P99_MIN_SAMPLES`]
//! samples. It publishes the maximum, under the name `max`, and the page
//! renders a maximum as a maximum.
//!
//! # The percentile
//!
//! Linearly interpolated between order statistics, the same way
//! [`crate::RunStats`] does it, so a p50 here and a median there are the same
//! number. Nearest-rank is what produced the p99-is-the-maximum shape and it
//! is not used anywhere in this module.

/// Below this many samples a row publishes `max`, never `p99`.
///
/// A hundred samples is the first size at which the 99th percentile has two
/// order statistics under it, so the interpolation has something to
/// interpolate between and the answer is not simply the largest sample.
pub const P99_MIN_SAMPLES: usize = 100;

/// A coefficient of variation above this makes a cell low-confidence.
pub const COV_LOW_CONFIDENCE: f64 = 0.15;

/// Resamples in the percentile bootstrap of the median.
pub const BOOTSTRAP_RESAMPLES: usize = 2000;

/// The interval level the bootstrap reports.
pub const BOOTSTRAP_LEVEL: f64 = 0.95;

/// A sample whose duration is fewer ticks than this is timer-saturated: the
/// clock, not the work, is what the number measures.
pub const MIN_TICKS_PER_SAMPLE: f64 = 100.0;

/// A cheap deterministic sequence, so a shuffle and a bootstrap mean the same
/// thing on every run and on every backend.
///
/// `splitmix64`, taken from the harness this replaces so the coordinate sets
/// are literally the same ones.
#[derive(Debug, Clone)]
pub struct Splitmix(u64);

impl Splitmix {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }
}

/// Which tail statistic a sample set earned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailKind {
    P99,
    Max,
}

impl TailKind {
    /// The name the document publishes. This is the whole point of the type:
    /// a maximum is labelled `max`.
    pub fn as_str(self) -> &'static str {
        match self {
            TailKind::P99 => "p99",
            TailKind::Max => "max",
        }
    }
}

/// A tail statistic and the name it is allowed to carry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tail {
    pub kind: TailKind,
    pub value: f64,
}

/// The tail statistic of a sample set, named for what it really is.
///
/// Under [`P99_MIN_SAMPLES`] this is the maximum and it says so.
pub fn tail(samples: &[f64]) -> Option<Tail> {
    if samples.is_empty() {
        return None;
    }
    let sorted = sorted(samples);
    if sorted.len() < P99_MIN_SAMPLES {
        Some(Tail {
            kind: TailKind::Max,
            value: *sorted.last().expect("non-empty"),
        })
    } else {
        Some(Tail {
            kind: TailKind::P99,
            value: percentile_sorted(&sorted, 0.99),
        })
    }
}

fn sorted(samples: &[f64]) -> Vec<f64> {
    let mut v: Vec<f64> = samples.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).expect("a sample is never NaN"));
    v
}

/// The `p`th percentile of a *sorted* slice, linearly interpolated.
pub fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    if n == 1 {
        return sorted[0];
    }
    let rank = p * (n - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    let frac = rank - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

/// The `p`th percentile of an unsorted sample.
pub fn percentile(samples: &[f64], p: f64) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    Some(percentile_sorted(&sorted(samples), p))
}

/// The median of a sample.
pub fn median(samples: &[f64]) -> Option<f64> {
    percentile(samples, 0.5)
}

/// Everything a cell publishes about the spread of one metric's samples.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub n: usize,
    pub median: f64,
    pub min: f64,
    pub max: f64,
    pub iqr: f64,
    /// Standard deviation over the mean. `None` when the mean is zero, which
    /// would make the ratio meaningless rather than large.
    pub cov: Option<f64>,
    /// Percentile bootstrap of the median.
    pub ci95: (f64, f64),
    /// Half the interval, as a percentage of the median.
    pub ci_half_width_pct: Option<f64>,
    /// The 95th percentile of the samples themselves, which is the run-to-run
    /// spread the page draws as a band.
    pub p95_of_samples: f64,
    /// The tail statistic, named for what it is.
    pub tail: Tail,
}

/// The magnitude below which `integrity::canonical_number` refuses a non-zero
/// value, because it is where `Number.prototype.toString` switches to exponent
/// form while Rust still prints digits, so the two languages would digest
/// different bytes for one number.
const CANONICAL_FLOOR: f64 = 1e-6;

/// Collapse a dimensionless ratio below [`CANONICAL_FLOOR`] to exactly zero.
///
/// A cell whose samples agree perfectly still produces a non-zero `cov`. The
/// variance is computed the stable way, but `mean` is the sum over the count and
/// that lands an ulp from the value every sample holds, so each `(v - mean)` is
/// about `1e-17` rather than zero. The first full capture hit it on exactly one
/// cell of 343, the brink cell's `read_tileid_order`, whose twenty samples are
/// all `0.458`, and that one number refused the whole document (#72).
///
/// This says the rule rather than rounding to reach it: a ratio under the floor
/// means the samples agree to seven decimal places, which is no dispersion by
/// any reading, and zero canonicalises in both languages. Everything at or above
/// the floor keeps every digit it had, which matters because the round-trip
/// witness in `tests/storage_provenance_k13.rs` is a `cov` and its point is that
/// the printer's exact digits survive a write and a read.
///
/// Only ratios come through here. The measurements are microsecond figures and
/// touching those would discard what the document exists to carry.
fn floor_derived(value: f64) -> f64 {
    if value.is_finite() && value.abs() < CANONICAL_FLOOR {
        return 0.0;
    }
    value
}

/// Summarise one metric's samples.
///
/// `seed` seeds the bootstrap, so the interval is reproducible from the
/// published samples.
pub fn summarise(samples: &[f64], seed: u64) -> Option<Summary> {
    if samples.is_empty() {
        return None;
    }
    let s = sorted(samples);
    let n = s.len();
    let median = percentile_sorted(&s, 0.5);
    let mean = s.iter().sum::<f64>() / n as f64;
    let var = if n > 1 {
        s.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1) as f64
    } else {
        0.0
    };
    let cov = if mean.abs() > f64::EPSILON {
        Some(floor_derived(var.sqrt() / mean.abs()))
    } else {
        None
    };
    let ci95 = bootstrap_median_ci(&s, seed);
    let ci_half_width_pct = if median.abs() > f64::EPSILON {
        Some(floor_derived(
            ((ci95.1 - ci95.0) / 2.0) / median.abs() * 100.0,
        ))
    } else {
        None
    };
    Some(Summary {
        n,
        median,
        min: s[0],
        max: s[n - 1],
        iqr: percentile_sorted(&s, 0.75) - percentile_sorted(&s, 0.25),
        cov,
        ci95,
        ci_half_width_pct,
        p95_of_samples: percentile_sorted(&s, 0.95),
        tail: tail(&s).expect("non-empty"),
    })
}

/// A percentile bootstrap interval for the median of a *sorted* sample.
///
/// Seeded, so two readers of the published samples get the same interval. The
/// interval always contains the sample median, because the bootstrap
/// distribution of the median is centred on it; the assertion is a test rather
/// than a clamp, because a clamp would hide a broken resampler.
pub fn bootstrap_median_ci(sorted_samples: &[f64], seed: u64) -> (f64, f64) {
    let n = sorted_samples.len();
    if n == 0 {
        return (f64::NAN, f64::NAN);
    }
    if n == 1 {
        return (sorted_samples[0], sorted_samples[0]);
    }
    let mut rng = Splitmix::new(seed);
    let mut medians = Vec::with_capacity(BOOTSTRAP_RESAMPLES);
    let mut resample = vec![0.0f64; n];
    for _ in 0..BOOTSTRAP_RESAMPLES {
        for slot in resample.iter_mut() {
            *slot = sorted_samples[rng.below(n)];
        }
        resample.sort_by(|a, b| a.partial_cmp(b).expect("a sample is never NaN"));
        medians.push(percentile_sorted(&resample, 0.5));
    }
    medians.sort_by(|a, b| a.partial_cmp(b).expect("a median is never NaN"));
    let alpha = (1.0 - BOOTSTRAP_LEVEL) / 2.0;
    (
        percentile_sorted(&medians, alpha),
        percentile_sorted(&medians, 1.0 - alpha),
    )
}

/// What the clock can resolve, measured rather than assumed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimerProbe {
    /// The smallest non-zero difference two `Instant::now()` calls report.
    pub tick_ns: f64,
    /// What one `Instant::now()` costs.
    pub call_ns: f64,
}

/// Probe the clock. Cheap: a hundred thousand calls, not the million the
/// design document names, because the answer converges long before that and
/// the probe runs in every child process.
pub fn probe_timer() -> TimerProbe {
    use std::time::Instant;
    const CALLS: u32 = 100_000;
    let started = Instant::now();
    let mut last = started;
    let mut smallest = f64::INFINITY;
    for _ in 0..CALLS {
        let now = Instant::now();
        let delta = now.duration_since(last).as_nanos() as f64;
        if delta > 0.0 && delta < smallest {
            smallest = delta;
        }
        last = now;
    }
    let total = started.elapsed().as_nanos() as f64;
    TimerProbe {
        tick_ns: if smallest.is_finite() { smallest } else { 0.0 },
        call_ns: total / f64::from(CALLS),
    }
}

/// Whether a sample of this size is measuring the work or the clock.
pub fn timer_saturated(sample_ns: f64, probe: TimerProbe) -> bool {
    if probe.tick_ns <= 0.0 {
        return false;
    }
    sample_ns / probe.tick_ns < MIN_TICKS_PER_SAMPLE
}

#[cfg(test)]
mod derived_rounding_tests {
    use super::*;

    /// Samples that agree perfectly have no dispersion, so `cov` is zero.
    ///
    /// It comes out as floating point residue instead, because `mean` is the
    /// sum divided by the count and that lands an ulp away from the value every
    /// sample holds. The residue is then small enough to trip the
    /// canonicaliser's `1e-6` refusal and take the whole document with it, which
    /// is what happened to the first full capture (#72).
    ///
    /// RED against `summarise` returning `var.sqrt() / mean.abs()` unrounded:
    /// twenty copies of 0.458 give `Some(1.243520512283748e-16)`.
    #[test]
    fn identical_samples_have_exactly_no_dispersion() {
        let samples = vec![0.458_f64; 20];
        let summary = summarise(&samples, 7).expect("twenty samples summarise");
        assert_eq!(
            summary.cov,
            Some(0.0),
            "a cov over identical samples is zero, not floating point residue"
        );
    }

    /// A ratio at or above the floor keeps every digit the printer would give
    /// it, because the byte round-trip witness in
    /// `tests/storage_provenance_k13.rs` is a `cov` and the whole point of that
    /// test is that the printer's exact digits survive a write and a read.
    ///
    /// I wrote this after breaking it. My first fix rounded every derived ratio
    /// to six decimals, which fixed the capture and silently turned the witness
    /// `0.09090909090909091` into `0.090909`, taking out both round-trip tests.
    /// RED against that version.
    #[test]
    fn a_ratio_above_the_floor_keeps_all_of_its_digits() {
        // The cov of [10, 11, 12], which is the witness those tests carry and
        // the value that demonstrates serde_json's reader bug.
        let summary = summarise(&[10.0, 11.0, 12.0], 7).expect("three samples summarise");
        let cov = summary.cov.expect("a non-zero mean gives a cov");
        assert_eq!(
            serde_json::to_string(&cov).expect("a float serialises"),
            "0.09090909090909091",
            "the witness has to survive summarise with the digits the printer chose"
        );
    }

    /// Every dimensionless derived statistic is either zero or large enough for
    /// the canonicaliser to accept, so no run can be refused for a number it
    /// computed about its own agreement.
    ///
    /// RED against the unrounded form, where `cov` lands at 1.2e-16 and this
    /// walk finds it.
    #[test]
    fn no_derived_ratio_lands_in_the_range_the_canonicaliser_refuses() {
        // The canonicaliser refuses a non-zero magnitude below this, because it
        // is where JavaScript switches to exponent form and Rust does not.
        const REFUSED_BELOW: f64 = 1e-6;

        // A positive control first: the walk has to be able to see a value in
        // the refused range, or a green result below means nothing.
        let planted = 1.243_520_512_283_748e-16_f64;
        assert!(
            planted != 0.0 && planted.abs() < REFUSED_BELOW,
            "the control value is in the range this test is looking for"
        );

        for samples in [
            vec![0.458_f64; 20],
            vec![1.0_f64; 7],
            vec![0.1, 0.1, 0.1, 0.1, 0.1, 0.1, 0.1],
            vec![12.5, 12.5000001, 12.5, 12.5, 12.5, 12.5, 12.5],
        ] {
            let summary = summarise(&samples, 7).expect("samples summarise");
            for (name, value) in [
                ("cov", summary.cov),
                ("ciHalfWidthPct", summary.ci_half_width_pct),
            ] {
                if let Some(v) = value {
                    assert!(
                        v == 0.0 || v.abs() >= REFUSED_BELOW,
                        "{name} is {v}, which the canonicaliser refuses and which would take \
                         the whole document with it"
                    );
                }
            }
        }
    }
}
