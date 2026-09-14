//! Environment provenance captured with every benchmark snapshot.
//!
//! A wall-time or RSS number only means something *relative to the
//! machine and toolchain that produced it*. Comparing a libvips-8.16 run
//! on a 4-core CI box against a libvips-8.18 run on a 10-core laptop is
//! not a version delta — it is an environment delta wearing a version
//! delta's clothes. [`Provenance`] records enough of the environment
//! (libvips version — both measured and pinned, measurement path, host
//! CPU/OS/arch, container flag, rustc, build profile) that `cross_version`
//! can *group by* fingerprint and refuse — or at least loudly flag —
//! cross-environment deltas.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::emulation::{self, Emulated, EmulationReport};

/// The exact upstream libvips release the benchmark container is pinned to
/// build from source and measure against.
///
/// Canonical declaration of the pinned oracle version, kept in lockstep with
/// its other homes by `tests/libvips_provenance.rs`: the `Dockerfile` builds
/// `vips-{PINNED_LIBVIPS_VERSION}.tar.xz` from upstream (checksum-verified
/// against [`PINNED_LIBVIPS_SHA256`]), the `libvips-rs` binding in
/// `Cargo.toml` tracks the same major.minor series, and
/// [`Provenance::capture`] stamps it into every snapshot as
/// [`Provenance::pinned_libvips_version`]. Those tests fail the moment any of
/// those homes drift from this constant.
///
/// Chosen to match the `libvips-rs` 8.18 bindings — replacing Debian
/// bookworm's frozen ~8.14 `libvips-dev`, which trailed the bindings by
/// years and made the C baseline an unfair, mismatched oracle (issue #33).
pub const PINNED_LIBVIPS_VERSION: &str = "8.18.4";

/// SHA-256 of `vips-{PINNED_LIBVIPS_VERSION}.tar.xz`, the digest the
/// `Dockerfile` verifies the downloaded tarball against before it is built.
///
/// Lives next to the version it belongs to so a pin bump and its digest have
/// a single home, the same lockstep treatment [`PINNED_LIBVIPS_VERSION`]
/// already enjoys; `tests/libvips_provenance.rs` asserts the Dockerfile pins
/// exactly this value. Cross-checked against the upstream
/// `vips-{PINNED_LIBVIPS_VERSION}.tar.xz.sha256sum` companion file — refresh
/// it in the same edit whenever [`PINNED_LIBVIPS_VERSION`] is bumped.
/// [`crate::pin_check::classify_libvips_pin`] validates this digest (and the
/// version) against the live upstream GitHub releases feed — run it on demand
/// via `tools/check-libvips-pin.sh` or the `#[ignore]`d live test
/// (libviprs-bench #36).
pub const PINNED_LIBVIPS_SHA256: &str =
    "2677bad6c422617fd1172d359c16af34e736965d042c214203a87187d26ff037";

/// Host + toolchain fingerprint for one benchmark snapshot.
///
/// `PartialEq` but not `Eq`: the [`load_average`](Provenance::load_average) axis
/// carries `f64`s (which are only `PartialEq`). Nothing compares a `Provenance`
/// for `Eq` — grouping is by the string [`fingerprint`](Provenance::fingerprint),
/// never by whole-struct equality.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Provenance {
    /// libvips runtime version actually measured (e.g. `"8.18.4"`), or
    /// `"unknown"`. Queried from the linked library / `vips` CLI at capture.
    pub libvips_version: String,
    /// The libvips release the environment was *pinned to build and measure*
    /// ([`PINNED_LIBVIPS_VERSION`]), recorded so every snapshot carries the
    /// intended oracle next to the one actually measured above. In the
    /// container the two are equal by construction; a divergence flags a run
    /// that measured a different libvips than it was pinned to (issue #33 —
    /// see [`Provenance::libvips_matches_pinned`]). Kept out of
    /// [`Provenance::fingerprint`] on purpose: the *measured* version is what
    /// groups comparable runs. Defaults to `"unknown"` for history written
    /// before this axis existed.
    #[serde(default = "unknown_libvips")]
    pub pinned_libvips_version: String,
    /// rustc version string captured at build time, or `"unknown"`.
    pub rustc_version: String,
    /// Cargo build profile the harness was compiled with: `"release"` or
    /// `"debug"`. Timing numbers are only meaningful for `"release"`.
    pub build_profile: String,
    /// `[profile.release]` codegen knobs the harness documents as the
    /// measured configuration (lto / codegen-units), captured at build
    /// time. Empty when unknown.
    pub build_flags: String,
    pub host: HostInfo,
    /// Host load average (1/5/15-minute) sampled at capture time, or `None`
    /// when it is not cheaply available on the platform. A run measured while
    /// the box was busy is slower for reasons unrelated to the code under test,
    /// so recording the load lets a reader discount (or discard) a contended
    /// measurement rather than mistake it for a regression. Deliberately kept
    /// *out* of [`Provenance::fingerprint`] — like the pinned-oracle axis, it is
    /// a per-run *condition*, not part of the environment identity that groups
    /// comparable runs. Defaults to `None` for history written before this axis
    /// existed (via `#[serde(default)]`).
    #[serde(default)]
    pub load_average: Option<LoadAverage>,
    /// Best-effort CPU thermal-throttle indicator sampled at capture time: the
    /// maximum cumulative throttle-event count across every core's and the
    /// package's Linux sysfs counters
    /// (`.../cpu*/thermal_throttle/{core,package}_throttle_count`), or `None`
    /// when it is not cheaply available (macOS, or a container without those
    /// sysfs nodes). Taking the max over all cores/package — rather than reading
    /// only `cpu0` — means a throttle event on any core is seen, not just core 0.
    /// `Some(0)` means the counters are readable and nothing has throttled;
    /// `Some(n > 0)` flags a box that has thermally throttled at some point since
    /// boot (the counter is cumulative, so a non-zero value is a coarse "runs
    /// hot" signal, not proof of throttling during this particular run). Like
    /// [`load_average`](Self::load_average) it is a per-run condition, kept out
    /// of the fingerprint. Defaults to `None` for legacy history (via
    /// `#[serde(default)]`).
    #[serde(default)]
    pub thermal_throttle_count: Option<u64>,
    /// Whether this process's instruction stream is being translated, plus the
    /// evidence the probe used. `None` for history written before the axis
    /// existed, which is the state `docs/pmtiles-benchmarks.md` is in and the
    /// reason the archive refuses anything that is not an explicit `false`.
    #[serde(default)]
    pub emulation: Option<EmulationRecord>,
    /// What the scratch directory is on. `None` when no scratch directory was
    /// named, which for a storage sweep is itself a refusal.
    #[serde(default)]
    pub filesystem: Option<FilesystemInfo>,
    /// The cgroup CPU and memory ceilings the run was actually under.
    #[serde(default)]
    pub cgroup: CgroupLimits,
    /// The toolchain axes the legacy fields above do not carry: cargo's own
    /// version, the flags cargo really handed rustc, and whether debug
    /// assertions are compiled in.
    #[serde(default)]
    pub toolchain: ToolchainInfo,
    /// Commit and dirty flag for both trees, with a note saying why a field is
    /// missing when it is.
    #[serde(default)]
    pub trees: SourceTrees,
    /// `sha256:` over `Cargo.lock`, stamped at build time.
    #[serde(default)]
    pub lockfile_hash: Option<String>,
    /// The resolved dependency graph, `{name: {version, source, checksum}}`.
    ///
    /// Empty from [`Provenance::capture`] and filled only by
    /// [`Provenance::capture_for_storage`]. That split is deliberate: this is a
    /// few hundred entries and `report/benchmark_history.json` appends one
    /// provenance per snapshot forever, so filling it on the everyday path
    /// would grow the history file by an order of magnitude to record the same
    /// graph over and over. A storage document is written once per sweep and
    /// archived by digest, which is where the graph earns its size.
    #[serde(default)]
    pub dependencies: BTreeMap<String, LockedDependency>,
}

/// Host load average — the 1/5/15-minute run-queue length averages — sampled at
/// benchmark capture time.
///
/// A load average near or above [`HostInfo::ncpu`] means the box was saturated
/// during the run, so its wall-time numbers are contended and not comparable to
/// an idle-host measurement. Read from `/proc/loadavg` on Linux and `getloadavg`
/// on macOS by [`Provenance::capture`]. `PartialEq` (not `Eq`) because the
/// components are `f64`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct LoadAverage {
    /// 1-minute load average.
    pub one_min: f64,
    /// 5-minute load average.
    pub five_min: f64,
    /// 15-minute load average.
    pub fifteen_min: f64,
}

/// Host machine identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostInfo {
    pub cpu_model: String,
    pub ncpu: u32,
    pub arch: String,
    pub os: String,
    /// Best-effort "are we inside a container?" flag. Container CPU quotas
    /// and memory limits change both timing and RSS, so it is part of the
    /// fingerprint.
    pub in_container: bool,
}

/// serde default for [`Provenance::pinned_libvips_version`] when a snapshot
/// predates the pinned-version axis: the same `"unknown"` sentinel the rest
/// of a pre-provenance fingerprint uses.
fn unknown_libvips() -> String {
    "unknown".to_string()
}

impl Default for Provenance {
    /// The fingerprint used for history written before provenance existed:
    /// everything `"unknown"`. Its [`Provenance::fingerprint`] never
    /// matches a real capture, so `cross_version` treats pre-provenance
    /// snapshots as their own environment bucket.
    fn default() -> Self {
        Provenance {
            libvips_version: "unknown".to_string(),
            pinned_libvips_version: "unknown".to_string(),
            rustc_version: "unknown".to_string(),
            build_profile: "unknown".to_string(),
            build_flags: String::new(),
            host: HostInfo {
                cpu_model: "unknown".to_string(),
                ncpu: 0,
                arch: "unknown".to_string(),
                os: "unknown".to_string(),
                in_container: false,
            },
            load_average: None,
            thermal_throttle_count: None,
            emulation: None,
            filesystem: None,
            cgroup: CgroupLimits::default(),
            toolchain: ToolchainInfo::default(),
            trees: SourceTrees::default(),
            lockfile_hash: None,
            dependencies: BTreeMap::new(),
        }
    }
}

/// Outcome of comparing the libvips actually measured against the pinned
/// build target ([`PINNED_LIBVIPS_VERSION`]) at `major.minor`.
///
/// Distinguishes a genuine mismatched oracle — a containerized run that built
/// or linked a different libvips than it was pinned to (issue #33) — from the
/// merely *indeterminate* case where a version string could not be parsed
/// (e.g. the `"unknown"` sentinel a host run without libvips records). The two
/// warrant different handling: a mismatch is a loud warning that the run's
/// numbers are not comparable to a pinned-oracle run; an indeterminate result
/// is the ordinary "no libvips here" state and must not cry wolf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleMatch {
    /// Measured and pinned agree at `major.minor`.
    Match,
    /// Both sides parsed but differ — the mismatched oracle #33 guards against.
    Mismatch {
        /// `(major, minor)` actually measured.
        measured: (u32, u32),
        /// `(major, minor)` the environment was pinned to.
        pinned: (u32, u32),
    },
    /// Either side is unparseable (e.g. `"unknown"`), so no verdict is
    /// possible — treated as "not a match" by
    /// [`Provenance::libvips_matches_pinned`].
    Indeterminate,
}

impl Provenance {
    /// Capture the current environment.
    pub fn capture() -> Provenance {
        Provenance {
            libvips_version: libvips_version(),
            pinned_libvips_version: PINNED_LIBVIPS_VERSION.to_string(),
            rustc_version: option_env!("BENCH_RUSTC_VERSION")
                .unwrap_or("unknown")
                .to_string(),
            build_profile: if cfg!(debug_assertions) {
                "debug".to_string()
            } else {
                "release".to_string()
            },
            build_flags: option_env!("BENCH_BUILD_FLAGS").unwrap_or("").to_string(),
            host: HostInfo {
                cpu_model: cpu_model(),
                ncpu: std::thread::available_parallelism()
                    .map(|n| n.get() as u32)
                    .unwrap_or(0),
                arch: std::env::consts::ARCH.to_string(),
                os: std::env::consts::OS.to_string(),
                in_container: detect_container(),
            },
            load_average: load_average(),
            thermal_throttle_count: thermal_throttle_count(),
            emulation: Some(EmulationRecord::probe()),
            // No scratch directory is named on this path, and a filesystem
            // recorded for a directory nobody asked about would be worse than
            // none: it would look like an answer.
            filesystem: None,
            cgroup: CgroupLimits::read(),
            toolchain: ToolchainInfo::stamped(),
            trees: SourceTrees::stamped(),
            lockfile_hash: option_env!("BENCH_LOCKFILE_HASH")
                .filter(|h| !h.is_empty())
                .map(str::to_string),
            dependencies: BTreeMap::new(),
        }
    }

    /// Capture everything, including the axes a storage sweep needs and the
    /// everyday path leaves out: the filesystem under `scratch_dir` and the
    /// resolved dependency graph.
    pub fn capture_for_storage(scratch_dir: &Path) -> Provenance {
        Provenance {
            filesystem: Some(FilesystemInfo::of(scratch_dir)),
            dependencies: locked_dependencies(),
            ..Provenance::capture()
        }
    }

    /// A stable, human-readable fingerprint string. Two snapshots with the
    /// same fingerprint were measured in comparable environments; a delta
    /// across differing fingerprints is not apples-to-apples.
    ///
    /// The dynamic per-run *conditions* — [`load_average`](Self::load_average)
    /// and [`thermal_throttle_count`](Self::thermal_throttle_count) — are
    /// deliberately excluded: a busy or throttled run must still group with an
    /// idle one on the same box so the contention is visible as an outlier
    /// rather than splitting the environment into two buckets.
    pub fn fingerprint(&self) -> String {
        format!(
            "vips{}/rustc{}/{}/{}-{}x{}cpu/{}/emul:{}/fs:{}",
            self.libvips_version,
            self.rustc_version,
            self.build_profile,
            self.host.os,
            self.host.arch,
            self.host.ncpu,
            if self.host.in_container {
                "container"
            } else {
                "host"
            },
            match &self.emulation {
                Some(e) => e.emulated.as_str(),
                None => "unrecorded",
            },
            match &self.filesystem {
                Some(fs) => fs.fs_type.as_str(),
                None => "unrecorded",
            },
        )
    }

    /// Classify the libvips actually measured against the pinned build target
    /// ([`Provenance::pinned_libvips_version`]) at `major.minor`.
    ///
    /// Equal by construction inside the pinned container; an
    /// [`OracleMatch::Mismatch`] on a containerized run means the image built
    /// or linked a different libvips than it was pinned to — the failure #33
    /// closes. The `report`, `scalability`, and `cross_version` binaries call
    /// this and surface a mismatch loudly (a mismatch alone is a warning, not
    /// an [`OracleMatch::Indeterminate`] "unknown", so a plain host run with no
    /// libvips never trips a false alarm).
    pub fn libvips_oracle_match(&self) -> OracleMatch {
        match (
            parse_libvips_major_minor(&self.libvips_version),
            parse_libvips_major_minor(&self.pinned_libvips_version),
        ) {
            (Some(measured), Some(pinned)) if measured == pinned => OracleMatch::Match,
            (Some(measured), Some(pinned)) => OracleMatch::Mismatch { measured, pinned },
            _ => OracleMatch::Indeterminate,
        }
    }

    /// Whether the measured libvips matches the pinned build target at
    /// `major.minor`. A thin `bool` view of [`Provenance::libvips_oracle_match`]:
    /// `false` for both a real [`OracleMatch::Mismatch`] and an unparseable
    /// ([`OracleMatch::Indeterminate`], e.g. `"unknown"`) side.
    pub fn libvips_matches_pinned(&self) -> bool {
        matches!(self.libvips_oracle_match(), OracleMatch::Match)
    }

    /// Whether the host looked contended when the load was sampled: the
    /// 1-minute load average met or exceeded the CPU count, so ready threads
    /// were already queued behind busy cores. The binaries sample this *before*
    /// the timed work (see [`report`] / [`scalability`]), so a `true` here means
    /// the box was under load at the start of the run — an ambient condition
    /// that inflates wall-time for reasons unrelated to the code under test —
    /// not a proof that the run itself was contended end to end (the 1-minute
    /// average is a lagging figure that can miss contention arriving mid-run).
    /// `false` when no load average was captured or the CPU count is unknown —
    /// a missing signal never cries wolf. Consumers surface a warning on `true`.
    pub fn host_looked_contended(&self) -> bool {
        match self.load_average {
            Some(la) if self.host.ncpu > 0 => la.one_min >= self.host.ncpu as f64,
            _ => false,
        }
    }

    /// Whether the CPU has thermally throttled *at some point since boot* (a
    /// non-zero [`thermal_throttle_count`](Self::thermal_throttle_count)). The
    /// underlying sysfs counter is cumulative since boot, so this cannot prove
    /// throttling *during this run* — only that the box has throttled before,
    /// possibly hours ago on a now-cool machine. It is a coarse "this host runs
    /// hot" flag, not a per-run attribution. `false` when the indicator is
    /// unavailable or reads zero.
    pub fn thermally_throttled(&self) -> bool {
        matches!(self.thermal_throttle_count, Some(n) if n > 0)
    }

    /// The measurement-condition warnings for this run, one string per line, in
    /// a stable order (contention, then thermal, then oracle mismatch). Empty
    /// when the run looks clean.
    ///
    /// Centralises the wording the `report` and `scalability` binaries print to
    /// stderr so the two can never drift (they used to hand-roll near-identical
    /// blocks that had already diverged). Each consumer just does
    /// `for w in prov.measurement_condition_warnings() { eprintln!("{w}"); }`.
    pub fn measurement_condition_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.host_looked_contended() {
            warnings.push(format!(
                "WARNING: 1-minute host load {} >= {} CPUs when sampled at the start of the \
                 run — the host was already under load, so these wall-time numbers are inflated \
                 by scheduling pressure, not the code under test.",
                self.load_average_line(),
                self.host.ncpu,
            ));
        }
        if self.thermally_throttled() {
            warnings.push(
                "WARNING: CPU thermal-throttle counter is non-zero — this host has thermally \
                 throttled at some point since boot (the counter is cumulative, so this is not \
                 proof it throttled during this run); if it did, timing numbers may understate \
                 true throughput."
                    .to_string(),
            );
        }
        if let OracleMatch::Mismatch { measured, pinned } = self.libvips_oracle_match() {
            warnings.push(format!(
                "WARNING: measured libvips {}.{} != pinned oracle {}.{} — this run measured a \
                 different libvips than the environment was pinned to build (issue #33); its \
                 numbers are NOT comparable to a pinned-oracle run.",
                measured.0, measured.1, pinned.0, pinned.1,
            ));
        }
        warnings
    }

    /// One-line host-load summary for banners: `"1.23 / 1.05 / 0.98"` (the
    /// 1/5/15-minute averages), or `"unavailable"` when no load average was
    /// captured on this platform.
    pub fn load_average_line(&self) -> String {
        match self.load_average {
            Some(la) => format!(
                "{:.2} / {:.2} / {:.2}",
                la.one_min, la.five_min, la.fifteen_min
            ),
            None => "unavailable".to_string(),
        }
    }
}

/// Strip the `vips-` and/or a leading `v` prefix a libvips version string may
/// carry, normalizing the GitHub release tag (`"v8.18.4"`), the `vips
/// --version` line (`"vips-8.18.4"`), and the bare pin (`"8.18.4"`) to the same
/// digit string.
///
/// Shared by [`parse_libvips_major_minor`] and
/// [`crate::pin_check::parse_libvips_version`] so the two parsers accept an
/// identical set of prefixes and differ only in how many components they
/// require — never in what they will strip.
pub(crate) fn strip_libvips_prefixes(version: &str) -> &str {
    let trimmed = version.trim();
    let no_vips = trimmed.strip_prefix("vips-").unwrap_or(trimmed);
    no_vips.strip_prefix('v').unwrap_or(no_vips)
}

/// Parse a libvips version string down to `(major, minor)`.
///
/// Accepts the raw `vips --version` line (`"vips-8.18.4"`), a GitHub release
/// tag (`"v8.18.4"`), and the already-stripped form [`libvips_version`] stores
/// (`"8.18.4"` / `"8.18"`) — prefix handling is shared with
/// [`crate::pin_check::parse_libvips_version`] via `strip_libvips_prefixes`.
/// Returns `None` for anything without at least a numeric `major.minor`
/// (e.g. the `"unknown"` sentinel), so a missing capture never compares
/// equal to a real version. A component carrying a non-digit suffix — a
/// pre-release tag like `"8.18-rc1"` — also yields `None` by design: the
/// pinned oracle is always a finished release, so a suffixed string is an
/// unexpected capture, not a version worth comparing.
pub fn parse_libvips_major_minor(version: &str) -> Option<(u32, u32)> {
    let digits = strip_libvips_prefixes(version);
    let mut parts = digits.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next()?.parse::<u32>().ok()?;
    Some((major, minor))
}

/// Query the libvips version. Prefers the linked library's own
/// `vips_version()` (FFI feature), falling back to `vips --version`.
pub fn libvips_version() -> String {
    #[cfg(feature = "libvips")]
    {
        // vips_version(0)=major, (1)=minor, (2)=micro.
        let major = unsafe { libvips_rs::bindings::vips_version(0) };
        let minor = unsafe { libvips_rs::bindings::vips_version(1) };
        let micro = unsafe { libvips_rs::bindings::vips_version(2) };
        if major > 0 {
            return format!("{major}.{minor}.{micro}");
        }
    }
    // CLI fallback: parse "vips-8.18.4".
    if let Ok(out) = std::process::Command::new("vips").arg("--version").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            if let Some(v) = s.trim().strip_prefix("vips-") {
                return v.to_string();
            }
            return s.trim().to_string();
        }
    }
    "unknown".to_string()
}

/// Best-effort host CPU model string.
fn cpu_model() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
        {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") {
            for line in text.lines() {
                if let Some(rest) = line.split_once(':') {
                    if line.starts_with("model name") {
                        return rest.1.trim().to_string();
                    }
                }
            }
            // aarch64 has no `model name` line at all: it reports `CPU
            // implementer`, `CPU architecture` and `CPU part` as separate hex
            // fields. Falling through to "unknown" there is not a cosmetic
            // gap, because the storage archive hashes the cpu model into the
            // run id's environment bucket, so every arm64 host in the world
            // would land in one bucket and two incomparable runs would look
            // comparable. Composing the fields gives a stable, distinguishing
            // string; `/proc/device-tree/model` is tried first because on a
            // board or a Pi it is the human name.
            if let Some(model) = std::fs::read_to_string("/proc/device-tree/model")
                .ok()
                .map(|m| m.trim_end_matches('\0').trim().to_string())
                .filter(|m| !m.is_empty())
            {
                return model;
            }
            let mut parts: Vec<String> = Vec::new();
            for field in ["CPU implementer", "CPU architecture", "CPU part", "CPU variant"] {
                if let Some(value) = text
                    .lines()
                    .find(|l| l.starts_with(field))
                    .and_then(|l| l.split_once(':'))
                    .map(|(_, v)| v.trim())
                {
                    parts.push(format!("{field}={value}"));
                }
            }
            if !parts.is_empty() {
                return format!("{} {}", std::env::consts::ARCH, parts.join(" "));
            }
        }
    }
    "unknown".to_string()
}

/// Sample the host 1/5/15-minute load average at capture time.
///
/// Reads `/proc/loadavg` on Linux (always present in the benchmark container)
/// and calls `getloadavg` on macOS (the host path); mirrors the same
/// per-platform split [`cpu_model`] uses. `None` on any other platform or when
/// the source is unreadable, so a missing sample is honestly absent rather than
/// a fabricated zero.
fn load_average() -> Option<LoadAverage> {
    // Exactly one cfg block compiles; each is the function's tail expression.
    #[cfg(target_os = "linux")]
    {
        // `/proc/loadavg`: "0.52 0.58 0.59 1/1234 5678" — the first three
        // whitespace-separated fields are the 1/5/15-minute averages.
        let text = std::fs::read_to_string("/proc/loadavg").ok()?;
        let mut parts = text.split_whitespace();
        let one_min = parts.next()?.parse::<f64>().ok()?;
        let five_min = parts.next()?.parse::<f64>().ok()?;
        let fifteen_min = parts.next()?.parse::<f64>().ok()?;
        Some(LoadAverage {
            one_min,
            five_min,
            fifteen_min,
        })
    }
    #[cfg(target_os = "macos")]
    {
        // `getloadavg` fills up to `nelem` averages (1/5/15-min) and returns the
        // count written, or -1 on failure. Not exposed by the `libc` crate for
        // glibc Linux, which is why Linux takes the `/proc/loadavg` path above.
        let mut loads = [0f64; 3];
        // SAFETY: `getloadavg` writes up to `nelem` `f64`s into the buffer and
        // returns the count written (or -1). `loads` is a 3-element `f64` stack
        // array and `nelem` is 3, so the pointer is valid for exactly the writes
        // the call can make; we read a component only when the return count is 3.
        let n = unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) };
        (n == 3).then_some(LoadAverage {
            one_min: loads[0],
            five_min: loads[1],
            fifteen_min: loads[2],
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Best-effort CPU thermal-throttle indicator: the maximum cumulative
/// throttle-event count across all cores and the package, from Linux sysfs, or
/// `None` when not cheaply available.
///
/// Walks `/sys/devices/system/cpu/cpu<N>/thermal_throttle/` and takes the max
/// over every readable `core_throttle_count` and `package_throttle_count` (a
/// handful of cheap file reads). Reading every core — not just `cpu0` — means a
/// throttle event on any core is caught; a host where `cpu0` stayed cool but
/// another core throttled would otherwise read a misleading zero. A non-zero
/// value means *some* core/package entered a thermal-throttle state at least
/// once since boot. `None` on macOS (no equivalent cheap counter — it would
/// need IOKit/SMC) and on Linux hosts/containers without those sysfs nodes
/// (i.e. when not a single counter could be read).
fn thermal_throttle_count() -> Option<u64> {
    // Exactly one cfg block compiles; each is the function's tail expression.
    #[cfg(target_os = "linux")]
    {
        let cpu_root = std::path::Path::new("/sys/devices/system/cpu");
        let entries = std::fs::read_dir(cpu_root).ok()?;
        let mut max: Option<u64> = None;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Match `cpu<N>` (a CPU dir), skip `cpufreq`, `cpuidle`, etc.
            if !(name.starts_with("cpu") && name[3..].chars().all(|c| c.is_ascii_digit()))
                || name.len() == 3
            {
                continue;
            }
            let throttle_dir = entry.path().join("thermal_throttle");
            for counter in ["core_throttle_count", "package_throttle_count"] {
                if let Some(n) = std::fs::read_to_string(throttle_dir.join(counter))
                    .ok()
                    .and_then(|text| text.trim().parse::<u64>().ok())
                {
                    max = Some(max.map_or(n, |m| m.max(n)));
                }
            }
        }
        max
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Best-effort container detection: cgroup hints on Linux, or the
/// conventional `/.dockerenv` marker.
fn detect_container() -> bool {
    if std::path::Path::new("/.dockerenv").exists() {
        return true;
    }
    if let Ok(text) = std::fs::read_to_string("/proc/1/cgroup") {
        if text.contains("docker") || text.contains("kubepods") || text.contains("containerd") {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// The storage suite's provenance axes (libviprs-bench #66).
//
// `docs/pmtiles-benchmarks.md` publishes 480 lines of numbers and records no
// platform at all. Everything below is the set of things that document would
// have needed to say for a reader to be able to check it, and every one of them
// is observed rather than declared.
// ---------------------------------------------------------------------------

/// Whether the run was instruction-translated, as the document spells it.
///
/// Three states, serialised as `true`, `false` and the string `"unknown"`,
/// which is what `SUITE-PLAN.md` §5.4 asks for and what causl's importer reads.
/// The third state has to survive the round trip intact: folding it into
/// `false` is exactly how a run nobody observed comes to look like a run
/// somebody checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmulationVerdict {
    /// Translation was observed.
    Emulated,
    /// No translation, observed by a source in a position to see it.
    Native,
    /// Nothing was in a position to observe either way.
    Unknown,
}

impl EmulationVerdict {
    /// The word this verdict goes by in a fingerprint or a log line.
    pub fn as_str(self) -> &'static str {
        match self {
            EmulationVerdict::Emulated => "true",
            EmulationVerdict::Native => "false",
            EmulationVerdict::Unknown => "unknown",
        }
    }
}

impl From<Emulated> for EmulationVerdict {
    fn from(value: Emulated) -> Self {
        match value {
            Emulated::Yes => EmulationVerdict::Emulated,
            Emulated::No => EmulationVerdict::Native,
            Emulated::Unknown => EmulationVerdict::Unknown,
        }
    }
}

impl Serialize for EmulationVerdict {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            EmulationVerdict::Emulated => serializer.serialize_bool(true),
            EmulationVerdict::Native => serializer.serialize_bool(false),
            EmulationVerdict::Unknown => serializer.serialize_str("unknown"),
        }
    }
}

impl<'de> Deserialize<'de> for EmulationVerdict {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        match Value::deserialize(deserializer)? {
            Value::Bool(true) => Ok(EmulationVerdict::Emulated),
            Value::Bool(false) => Ok(EmulationVerdict::Native),
            Value::String(s) if s == "unknown" => Ok(EmulationVerdict::Unknown),
            other => Err(D::Error::custom(format!(
                "emulated must be true, false or \"unknown\", not {other}"
            ))),
        }
    }
}

/// One source's observation, carried into the document so it can say which
/// evidence it used and not only what it concluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmulationEvidence {
    /// `proc-self-maps`, `binfmt-misc`, `daemon-arch` or `uname`.
    pub source: String,
    /// `emulated`, `native` or `inconclusive`.
    pub verdict: String,
    /// What that source actually saw, including why it saw nothing.
    pub detail: String,
}

/// The emulation probe's answer and its working.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmulationRecord {
    /// The combined verdict.
    pub emulated: EmulationVerdict,
    /// Every source consulted, including the ones that saw nothing.
    pub evidence: Vec<EmulationEvidence>,
    /// The architecture the binary was compiled for.
    pub binary_arch: String,
    /// What the kernel reports, when `uname` could be run.
    pub uname_arch: Option<String>,
    /// What the runner said the Docker daemon runs on.
    pub daemon_arch: Option<String>,
}

impl EmulationRecord {
    /// Run the probe and record it.
    pub fn probe() -> EmulationRecord {
        EmulationRecord::from(emulation::probe())
    }
}

impl From<EmulationReport> for EmulationRecord {
    fn from(report: EmulationReport) -> Self {
        EmulationRecord {
            emulated: report.emulated.into(),
            evidence: report
                .evidence
                .iter()
                .map(|e| EmulationEvidence {
                    source: e.source.to_string(),
                    verdict: match e.verdict {
                        emulation::EvidenceVerdict::Emulated => "emulated",
                        emulation::EvidenceVerdict::Native => "native",
                        emulation::EvidenceVerdict::Inconclusive => "inconclusive",
                    }
                    .to_string(),
                    detail: e.detail.clone(),
                })
                .collect(),
            binary_arch: report.binary_arch.to_string(),
            uname_arch: report.uname_arch,
            daemon_arch: report.daemon_arch,
        }
    }
}

/// What the scratch directory is actually on.
///
/// A PMTiles sweep writes one file and a directory sweep writes tens of
/// thousands, so the filesystem underneath is not a detail: overlayfs, a
/// virtiofs bind mount from a Mac, tmpfs and a real ext4 give four different
/// ratios between the two backends, and only one of them is the ratio anyone
/// will see in production. The published numbers record none of this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemInfo {
    /// The directory that was asked about, canonicalised.
    pub scratch_dir: String,
    /// `ext4`, `overlay`, `tmpfs`, `apfs`, `virtiofs`, or `unknown-0x<magic>`
    /// for a filesystem this does not have a name for. The hex form is
    /// deliberate: a magic number nobody has mapped yet is still a fact, and it
    /// is a fact somebody can look up.
    pub fs_type: String,
    /// The device or source the mount came from, when `/proc` names one.
    pub mount_source: Option<String>,
    /// Whether the mount is a bind of a subtree rather than a whole
    /// filesystem. `None` where it cannot be told.
    pub bind_mount: Option<bool>,
    /// Whether the run's profile declared that it means to measure on tmpfs.
    /// Left `false` here and set by the driver; the archive refuses tmpfs
    /// without it.
    pub declared_tmpfs: bool,
}

impl FilesystemInfo {
    /// Observe the filesystem under `dir`.
    pub fn of(dir: &Path) -> FilesystemInfo {
        let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let (mount_source, bind_mount) = mount_entry(&canonical);
        FilesystemInfo {
            fs_type: fs_type_name(&canonical),
            scratch_dir: canonical.display().to_string(),
            mount_source,
            bind_mount,
            declared_tmpfs: false,
        }
    }
}

/// The ceilings the run was under, which decide how much of the box it could
/// actually use.
///
/// A four-core quota on a sixteen-core host makes a concurrent read scenario
/// measure the quota rather than the engine, and `nproc` inside the container
/// happily reports sixteen either way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CgroupLimits {
    /// CPUs available, from quota over period. `None` for unlimited or
    /// unreadable.
    pub cpu_quota: Option<f64>,
    /// Memory ceiling in bytes. `None` for unlimited or unreadable.
    pub memory_limit_bytes: Option<u64>,
}

impl CgroupLimits {
    /// Read the limits, cgroup v2 first and v1 as the fallback.
    pub fn read() -> CgroupLimits {
        CgroupLimits {
            cpu_quota: cgroup_cpu_quota(),
            memory_limit_bytes: cgroup_memory_limit(),
        }
    }
}

/// The toolchain axes the legacy fields do not carry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolchainInfo {
    /// `cargo --version` at build time.
    pub cargo_version: String,
    /// The flags cargo really handed rustc, from `CARGO_ENCODED_RUSTFLAGS`,
    /// which unlike bare `RUSTFLAGS` also carries what `.cargo/config.toml`
    /// contributed. A run that picks up `-C target-cpu=native` from a config
    /// file and reports an empty flag set is a run whose numbers cannot be
    /// reproduced from what it wrote down.
    pub rustflags: String,
    /// `cfg!(debug_assertions)` at runtime. The archive refuses `true`.
    pub debug_assertions: bool,
}

impl ToolchainInfo {
    /// Read what `build.rs` stamped, plus the one thing only the running binary
    /// knows.
    pub fn stamped() -> ToolchainInfo {
        ToolchainInfo {
            cargo_version: option_env!("BENCH_CARGO_VERSION")
                .unwrap_or("unknown")
                .to_string(),
            rustflags: option_env!("BENCH_RUSTFLAGS_ENCODED")
                .unwrap_or("")
                .to_string(),
            debug_assertions: cfg!(debug_assertions),
        }
    }
}

/// One source tree's identity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeState {
    /// The full commit, or `None` when git could not answer.
    pub commit: Option<String>,
    /// Whether tracked files differ from that commit, ignoring mode-only
    /// differences. `None` when git could not answer.
    pub dirty: Option<bool>,
    /// Why the two above are not better than they are. `"clean read"` when
    /// nothing went wrong.
    pub note: String,
}

/// Both trees, because a benchmark of one library run by another harness is
/// pinned by two commits and quoting one of them is quoting half the answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceTrees {
    /// `libviprs-bench`, the harness doing the measuring.
    pub harness: TreeState,
    /// `libviprs`, the library being measured.
    pub library: TreeState,
}

impl SourceTrees {
    /// Read what `build.rs` stamped.
    pub fn stamped() -> SourceTrees {
        SourceTrees {
            harness: stamped_tree(
                option_env!("BENCH_HARNESS_COMMIT"),
                option_env!("BENCH_HARNESS_DIRTY"),
                option_env!("BENCH_HARNESS_GIT_NOTE"),
            ),
            library: stamped_tree(
                option_env!("BENCH_LIBRARY_COMMIT"),
                option_env!("BENCH_LIBRARY_DIRTY"),
                option_env!("BENCH_LIBRARY_GIT_NOTE"),
            ),
        }
    }
}

/// One tree's stamps, with empty read as absent rather than as a value.
fn stamped_tree(commit: Option<&str>, dirty: Option<&str>, note: Option<&str>) -> TreeState {
    TreeState {
        commit: commit.filter(|c| !c.is_empty()).map(str::to_string),
        dirty: match dirty {
            Some("true") => Some(true),
            Some("false") => Some(false),
            _ => None,
        },
        note: note
            .filter(|n| !n.is_empty())
            .unwrap_or("this binary was built before the provenance stamps existed")
            .to_string(),
    }
}

/// One resolved crate, in Cargo's own vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedDependency {
    /// The resolved version.
    pub version: String,
    /// Where it came from. `None` for a path or workspace member.
    pub source: Option<String>,
    /// The registry checksum, Cargo's answer to npm's `integrity`. `None` for a
    /// path dependency, which is the honest answer rather than an empty string.
    pub checksum: Option<String>,
}

/// The resolved dependency graph `build.rs` wrote out of `Cargo.lock`.
///
/// Baked into the binary with `include_str!` so it is available with no
/// filesystem at runtime: a binary copied into a scratch container without its
/// source tree still knows exactly what it was built from, which is the whole
/// point of recording it.
pub fn locked_dependencies() -> BTreeMap<String, LockedDependency> {
    const GRAPH: &str = include_str!(concat!(env!("OUT_DIR"), "/lock-dependencies.json"));
    serde_json::from_str(GRAPH).unwrap_or_default()
}

impl Provenance {
    /// The `provenance` block of a storage document, in the shape
    /// `SUITE-PLAN.md` §5.4 names and `storage::archive::admit` reads.
    ///
    /// `invocation` comes from the driver rather than from here, because the
    /// argv, the working directory and the defaults that were resolved are
    /// facts about the run and not about the environment. `allow_dirty` is the
    /// operator's decision, recorded next to the dirt it allows.
    ///
    /// Every field is written, including the ones that are `null`. No
    /// `skip_serializing_if` anywhere: an absent key and an explicit null are
    /// different documents with different digests, and the cross-language
    /// digest test K2.2 will pin cannot survive a field that Rust drops and
    /// JavaScript writes.
    pub fn to_storage_block(&self, invocation: &Value, allow_dirty: bool) -> Value {
        json!({
            "library": {
                "name": "libviprs",
                "version": option_env!("LIBVIPRS_CORE_VERSION").unwrap_or("unknown"),
                "commit": self.trees.library.commit,
                "dirty": self.trees.library.dirty,
                "gitNote": self.trees.library.note,
            },
            "commit": self.trees.harness.commit,
            "dirty": self.trees.harness.dirty,
            "gitNote": self.trees.harness.note,
            "allowDirty": allow_dirty,
            "emulated": self.emulation.as_ref().map(|e| e.emulated),
            "emulationEvidence": self.emulation.as_ref().map(|e| &e.evidence),
            // Spelled out rather than serialised straight from the struct. The
            // document's keys are camelCase and `Provenance`'s on-disk shape in
            // `benchmark_history.json` is snake_case, and reconciling that with
            // a `rename_all` on one struct would leave one block in a file
            // spelled differently from its siblings. The contract this block
            // implements is §5.4's, so §5.4's spellings live here, in the one
            // function that exists to produce them.
            "filesystem": self.filesystem.as_ref().map(|fs| json!({
                "scratchDir": fs.scratch_dir,
                "fsType": fs.fs_type,
                "mountSource": fs.mount_source,
                "bindMount": fs.bind_mount,
                "declaredTmpfs": fs.declared_tmpfs,
            })),
            "node": {
                "rustc": self.rustc_version,
                "cargo": self.toolchain.cargo_version,
                "buildProfile": self.build_profile,
                "buildFlags": self.build_flags,
                "rustflags": self.toolchain.rustflags,
                "debugAssertions": self.toolchain.debug_assertions,
            },
            "os": self.host.os,
            "arch": self.host.arch,
            "cpuModel": self.host.cpu_model,
            "ncpu": self.host.ncpu,
            "inContainer": self.host.in_container,
            "cgroupCpuQuota": self.cgroup.cpu_quota,
            "cgroupMemoryLimit": self.cgroup.memory_limit_bytes,
            "loadAverage": self.load_average.map(|la| json!({
                "oneMin": la.one_min,
                "fiveMin": la.five_min,
                "fifteenMin": la.fifteen_min,
            })),
            "thermalThrottleCount": self.thermal_throttle_count,
            "lockfileHash": self.lockfile_hash,
            "dependencies": self.dependencies,
            "invocation": invocation,
        })
    }

    /// The storage-specific warnings, for stderr before a sweep starts rather
    /// than as a refusal forty minutes after it finishes.
    ///
    /// Every one of these is also a refusal in `storage::archive::admit`. Saying
    /// it twice is the point: the refusal is what keeps the archive honest, and
    /// the warning is what stops somebody burning an afternoon to earn it.
    pub fn storage_provenance_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        match self.emulation.as_ref().map(|e| e.emulated) {
            Some(EmulationVerdict::Native) => {}
            Some(EmulationVerdict::Emulated) => warnings.push(
                "WARNING: this process is being instruction-translated, so its timings \
                 describe the translator as much as the code. Nothing will archive."
                    .to_string(),
            ),
            _ => warnings.push(
                "WARNING: the emulation probe could not observe either way, and an unobserved \
                 run is refused the same as a translated one. Nothing will archive."
                    .to_string(),
            ),
        }
        for (label, tree) in [
            ("harness", &self.trees.harness),
            ("library", &self.trees.library),
        ] {
            if tree.commit.is_none() || tree.dirty.is_none() {
                warnings.push(format!(
                    "WARNING: the {label} tree has no commit or no dirty flag, so nothing will \
                     archive. The reason recorded at build time was: {}",
                    tree.note
                ));
            } else if tree.dirty == Some(true) {
                warnings.push(format!(
                    "WARNING: the {label} tree is dirty, so these numbers describe a source \
                     state that exists on one machine. Archiving needs --allow-dirty, which \
                     stamps every cell."
                ));
            }
        }
        if self.toolchain.debug_assertions {
            warnings.push(
                "WARNING: debug assertions are compiled in, so every bounds and overflow check \
                 in the engine is inside the measurement."
                    .to_string(),
            );
        }
        if let Some(fs) = &self.filesystem {
            if fs.fs_type == "tmpfs" && !fs.declared_tmpfs {
                warnings.push(format!(
                    "WARNING: the scratch directory {} is on tmpfs, which is RAM with a \
                     filesystem interface. Nothing will archive unless the profile declares it.",
                    fs.scratch_dir
                ));
            }
        }
        warnings
    }
}

/// Name the filesystem under `dir`.
#[cfg(target_os = "linux")]
fn fs_type_name(dir: &Path) -> String {
    // Every value here is a kernel `*_SUPER_MAGIC`. An unmapped one still gets
    // recorded, in hex, because a magic number nobody has named yet is a fact
    // somebody can look up and `"unknown"` is not.
    const MAGICS: &[(i64, &str)] = &[
        (0x0000_EF53, "ext4"),
        (0x5846_5342, "xfs"),
        (0x9123_683E, "btrfs"),
        (0x0102_1994, "tmpfs"),
        (0x794c_7630, "overlay"),
        (0x0000_6969, "nfs"),
        (0x0102_1997, "9p"),
        (0x6573_7546, "fuse"),
        (0x2fc1_2fc1, "zfs"),
        (0x7371_7368, "squashfs"),
        (0x5346_544e, "ntfs"),
        (0x0000_4d44, "vfat"),
        (0x2011_BAB0, "exfat"),
        (0xCA45_1A4E, "bcachefs"),
    ];
    match statfs_type(dir) {
        Some(magic) => MAGICS
            .iter()
            .find(|(m, _)| *m == magic)
            .map(|(_, name)| (*name).to_string())
            .unwrap_or_else(|| format!("unknown-{magic:#x}")),
        None => "unknown".to_string(),
    }
}

/// The raw `statfs` magic for `dir`.
#[cfg(target_os = "linux")]
fn statfs_type(dir: &Path) -> Option<i64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;
    let path = CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: `path` is a NUL-terminated C string that outlives the call, and
    // `buf` is a correctly sized, writable `statfs` the kernel fills. The return
    // value is checked before a single field is read.
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(path.as_ptr(), &mut buf) };
    (rc == 0).then(|| buf.f_type as i64)
}

/// Name the filesystem under `dir`.
#[cfg(target_os = "macos")]
fn fs_type_name(dir: &Path) -> String {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(path) = CString::new(dir.as_os_str().as_bytes()) else {
        return "unknown".to_string();
    };
    // SAFETY: as the Linux arm above. macOS hands back the filesystem's name
    // directly rather than a magic number, so there is no table to keep.
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(path.as_ptr(), &mut buf) };
    if rc != 0 {
        return "unknown".to_string();
    }
    let name = unsafe { CStr::from_ptr(buf.f_fstypename.as_ptr()) };
    name.to_string_lossy().to_string()
}

/// Name the filesystem under `dir`.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn fs_type_name(_dir: &Path) -> String {
    "unknown".to_string()
}

/// The mount `dir` sits on: what it came from, and whether it is a bind of a
/// subtree.
///
/// `/proc/self/mountinfo` rather than `/proc/mounts`, because only mountinfo
/// carries the mount *root* field, and that field is the only way to tell a
/// bind mount of `/Users/rom/workspace` from a mount of a whole filesystem. A
/// bind mount is the ordinary case on this Mac and it is worth recording: the
/// bytes cross a virtiofs boundary on their way to the disk.
#[cfg(target_os = "linux")]
fn mount_entry(dir: &Path) -> (Option<String>, Option<bool>) {
    let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return (None, None);
    };
    let target = dir.to_string_lossy();
    let mut best: Option<(usize, String, bool)> = None;
    for line in text.lines() {
        // `id parent major:minor root mountpoint options... - fstype source superopts`
        let fields: Vec<&str> = line.split(' ').collect();
        if fields.len() < 7 {
            continue;
        }
        let root = fields[3];
        let mount_point = fields[4];
        if !(target == mount_point
            || (target.starts_with(mount_point)
                && (mount_point == "/" || target.as_bytes().get(mount_point.len()) == Some(&b'/'))))
        {
            continue;
        }
        let Some(separator) = fields.iter().position(|f| *f == "-") else {
            continue;
        };
        let source = fields.get(separator + 2).copied().unwrap_or("").to_string();
        let candidate = (mount_point.len(), source, root != "/");
        if best.as_ref().map(|b| b.0).unwrap_or(0) <= candidate.0 {
            best = Some(candidate);
        }
    }
    match best {
        Some((_, source, bind)) => (Some(source), Some(bind)),
        None => (None, None),
    }
}

/// The mount `dir` sits on.
#[cfg(not(target_os = "linux"))]
fn mount_entry(_dir: &Path) -> (Option<String>, Option<bool>) {
    (None, None)
}

/// CPUs the cgroup allows, v2 first then v1.
fn cgroup_cpu_quota() -> Option<f64> {
    // v2: `cpu.max` is "<quota> <period>", or "max <period>" for unlimited.
    if let Ok(text) = std::fs::read_to_string("/sys/fs/cgroup/cpu.max") {
        let mut parts = text.split_whitespace();
        let quota = parts.next()?;
        let period = parts.next()?.parse::<f64>().ok()?;
        if quota == "max" {
            return None;
        }
        return Some(quota.parse::<f64>().ok()? / period);
    }
    // v1: a negative quota means unlimited.
    let quota = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_quota_us")
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()?;
    let period = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_period_us")
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()?;
    (quota > 0.0 && period > 0.0).then(|| quota / period)
}

/// Memory ceiling in bytes, v2 first then v1.
fn cgroup_memory_limit() -> Option<u64> {
    if let Ok(text) = std::fs::read_to_string("/sys/fs/cgroup/memory.max") {
        let text = text.trim();
        if text == "max" {
            return None;
        }
        return text.parse::<u64>().ok();
    }
    let raw = std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes")
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    // v1 spells "unlimited" as a number near the top of the address space
    // rather than as a word, and reporting that as a limit would be a lie with
    // nineteen digits of confidence.
    (raw < (1u64 << 62)).then_some(raw)
}
