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

use serde::{Deserialize, Serialize};

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
            "vips{}/rustc{}/{}/{}-{}x{}cpu/{}",
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
