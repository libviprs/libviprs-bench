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
/// Single source of truth for the pinned oracle. The `Dockerfile` builds
/// `vips-{PINNED_LIBVIPS_VERSION}.tar.xz` from upstream (checksum-verified),
/// the `libvips-rs` binding in `Cargo.toml` tracks the same major.minor
/// series, and [`Provenance::capture`] stamps it into every snapshot as
/// [`Provenance::pinned_libvips_version`]. `tests/libvips_provenance.rs`
/// fails the moment any of those three drift from this constant.
///
/// Chosen to match the `libvips-rs` 8.18 bindings — replacing Debian
/// bookworm's frozen ~8.14 `libvips-dev`, which trailed the bindings by
/// years and made the C baseline an unfair, mismatched oracle (issue #33).
pub const PINNED_LIBVIPS_VERSION: &str = "8.18.4";

/// Host + toolchain fingerprint for one benchmark snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
        }
    }
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
        }
    }

    /// A stable, human-readable fingerprint string. Two snapshots with the
    /// same fingerprint were measured in comparable environments; a delta
    /// across differing fingerprints is not apples-to-apples.
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

    /// Whether the libvips actually measured matches the pinned build target
    /// ([`Provenance::pinned_libvips_version`]) at `major.minor`.
    ///
    /// Equal by construction inside the pinned container; a `false` here on a
    /// containerized run means the image built or linked a different libvips
    /// than it was pinned to — the mismatched-oracle failure #33 closes.
    /// `false` whenever either side is unparseable (e.g. `"unknown"`).
    pub fn libvips_matches_pinned(&self) -> bool {
        match (
            parse_libvips_major_minor(&self.libvips_version),
            parse_libvips_major_minor(&self.pinned_libvips_version),
        ) {
            (Some(measured), Some(pinned)) => measured == pinned,
            _ => false,
        }
    }
}

/// Parse a libvips version string down to `(major, minor)`.
///
/// Accepts both the raw `vips --version` line (`"vips-8.18.4"`) and the
/// already-stripped form [`libvips_version`] stores (`"8.18.4"` / `"8.18"`).
/// Returns `None` for anything without at least a numeric `major.minor`
/// (e.g. the `"unknown"` sentinel), so a missing capture never compares
/// equal to a real version.
pub fn parse_libvips_major_minor(version: &str) -> Option<(u32, u32)> {
    let trimmed = version.trim();
    let digits = trimmed.strip_prefix("vips-").unwrap_or(trimmed);
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
