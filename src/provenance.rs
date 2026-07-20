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
/// [`classify_libvips_pin`] validates this digest (and the version) against the
/// live upstream GitHub releases feed — run it on demand via
/// `tools/check-libvips-pin.sh` or the `#[ignore]`d live test (libviprs-bench
/// #36).
pub const PINNED_LIBVIPS_SHA256: &str =
    "2677bad6c422617fd1172d359c16af34e736965d042c214203a87187d26ff037";

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
}

/// Parse a libvips version string down to `(major, minor)`.
///
/// Accepts both the raw `vips --version` line (`"vips-8.18.4"`) and the
/// already-stripped form [`libvips_version`] stores (`"8.18.4"` / `"8.18"`).
/// Returns `None` for anything without at least a numeric `major.minor`
/// (e.g. the `"unknown"` sentinel), so a missing capture never compares
/// equal to a real version. A component carrying a non-digit suffix — a
/// pre-release tag like `"8.18-rc1"` — also yields `None` by design: the
/// pinned oracle is always a finished release, so a suffixed string is an
/// unexpected capture, not a version worth comparing.
pub fn parse_libvips_major_minor(version: &str) -> Option<(u32, u32)> {
    let trimmed = version.trim();
    let digits = trimmed.strip_prefix("vips-").unwrap_or(trimmed);
    let mut parts = digits.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next()?.parse::<u32>().ok()?;
    Some((major, minor))
}

/// Parse a libvips version/tag string down to `(major, minor, patch)`.
///
/// Accepts the GitHub release tag (`"v8.18.4"`), the `vips --version` line
/// (`"vips-8.18.4"`), and the bare pin (`"8.18.4"`). Unlike
/// [`parse_libvips_major_minor`], the patch component is *required*: upstream
/// releases always carry one, and [`classify_libvips_pin`] compares whole
/// `(major, minor, patch)` tuples to decide whether a newer release exists. A
/// pre-release tag (`"v8.18.3-rc1"`, whose patch is non-numeric) yields `None`,
/// so a release candidate can never rank as — or newer than — a finished
/// release.
pub fn parse_libvips_version(tag: &str) -> Option<(u32, u32, u32)> {
    let trimmed = tag.trim();
    let no_vips = trimmed.strip_prefix("vips-").unwrap_or(trimmed);
    let digits = no_vips.strip_prefix('v').unwrap_or(no_vips);
    let mut parts = digits.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next()?.parse::<u32>().ok()?;
    let patch = parts.next()?.parse::<u32>().ok()?;
    Some((major, minor, patch))
}

/// Outcome of validating the pinned libvips ([`PINNED_LIBVIPS_VERSION`] /
/// [`PINNED_LIBVIPS_SHA256`]) against the upstream GitHub releases feed.
///
/// Produced by [`classify_libvips_pin`] from a captured or live
/// `GET /repos/libvips/libvips/releases` payload. The on-demand validator
/// (`tools/check-libvips-pin.sh` and the `#[ignore]`d live test) renders it; it
/// is deliberately advisory, never a PR gate — this repo gates locally and
/// skips GitHub CI on PR commits (libviprs-bench #36).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibvipsPinStatus {
    /// The pin is the latest stable upstream release and its tarball SHA-256
    /// still matches — nothing to do.
    UpToDate,
    /// A stable upstream release strictly newer than the pin exists.
    NewerReleaseAvailable {
        /// Latest stable upstream version, `"major.minor.patch"` — the bump
        /// target an operator sees.
        latest: String,
    },
    /// The pinned release's upstream tarball digest no longer matches
    /// [`PINNED_LIBVIPS_SHA256`] — upstream re-published the asset, or the
    /// recorded digest is wrong. The integrity signal; reported ahead of a mere
    /// version bump so a re-cut pinned tarball is never masked by "there is a
    /// newer version anyway".
    Sha256Mismatch {
        /// The recorded pin (the `pinned_sha256` argument).
        pinned: String,
        /// The digest the upstream release now advertises for the pinned tarball.
        upstream: String,
    },
    /// The pinned release (or its `vips-<version>.tar.xz` asset with a digest)
    /// was not found in the feed, so the SHA could not be validated —
    /// indeterminate, not a pass. Mirrors [`OracleMatch::Indeterminate`]'s
    /// "no verdict" role.
    PinnedReleaseNotFound,
}

/// Why [`classify_libvips_pin`] could not reach a verdict from a payload.
#[derive(Debug)]
pub enum LibvipsPinError {
    /// The payload was not valid JSON in the expected releases-array shape.
    /// Carries the underlying `serde_json` message.
    Parse(String),
    /// The payload carried no stable (non-draft, non-pre-release) release, so
    /// there is nothing to compare the pin against.
    NoStableRelease,
}

impl std::fmt::Display for LibvipsPinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "could not parse the releases payload: {e}"),
            Self::NoStableRelease => write!(f, "the releases payload carried no stable release"),
        }
    }
}

impl std::error::Error for LibvipsPinError {}

/// One GitHub release, trimmed to the fields the validator reads. Unknown
/// fields are ignored, so the real API payload deserializes unchanged.
#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

/// One release asset: its file name and (when the API provides it) the
/// `"sha256:<hex>"` digest GitHub advertises for it.
#[derive(Deserialize)]
struct GhAsset {
    name: String,
    #[serde(default)]
    digest: Option<String>,
}

/// Strip the `sha256:` scheme prefix GitHub asset digests carry.
fn strip_sha256_prefix(digest: &str) -> &str {
    digest.strip_prefix("sha256:").unwrap_or(digest)
}

/// Validate the recorded libvips pin (`pinned_version` / `pinned_sha256`,
/// normally [`PINNED_LIBVIPS_VERSION`] / [`PINNED_LIBVIPS_SHA256`]) against a
/// GitHub releases API payload (`GET /repos/libvips/libvips/releases`, a JSON
/// array).
///
/// Host-independent and network-free: the caller supplies the payload — a
/// captured sample in tests, a live `curl` fetch in the on-demand validator —
/// so the same classification logic runs in both. Integrity beats freshness: a
/// tarball digest that no longer matches upstream
/// ([`LibvipsPinStatus::Sha256Mismatch`]) is reported ahead of a mere newer
/// release, so a re-published pinned asset is never masked by a version bump.
pub fn classify_libvips_pin(
    releases_json: &str,
    pinned_version: &str,
    pinned_sha256: &str,
) -> Result<LibvipsPinStatus, LibvipsPinError> {
    let releases: Vec<GhRelease> =
        serde_json::from_str(releases_json).map_err(|e| LibvipsPinError::Parse(e.to_string()))?;

    // Latest *stable* release: drafts and pre-releases (e.g. `v8.18.3-rc1`) are
    // never a bump target.
    let latest_stable = releases
        .iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| parse_libvips_version(&r.tag_name))
        .max()
        .ok_or(LibvipsPinError::NoStableRelease)?;

    // Locate the pinned release's tarball digest, if the feed carries it.
    let pinned_ver = parse_libvips_version(pinned_version);
    let tarball = format!("vips-{pinned_version}.tar.xz");
    let upstream_digest: Option<&str> = pinned_ver.and_then(|pv| {
        releases
            .iter()
            .filter(|r| parse_libvips_version(&r.tag_name) == Some(pv))
            .flat_map(|r| r.assets.iter())
            .find(|a| a.name == tarball)
            .and_then(|a| a.digest.as_deref())
            .map(strip_sha256_prefix)
    });

    // Integrity first: a re-cut pinned tarball outranks a newer release.
    if let Some(upstream) = upstream_digest.filter(|u| !u.eq_ignore_ascii_case(pinned_sha256)) {
        return Ok(LibvipsPinStatus::Sha256Mismatch {
            pinned: pinned_sha256.to_string(),
            upstream: upstream.to_string(),
        });
    }

    // Then freshness: a strictly newer stable release is a bump target.
    if pinned_ver.is_some_and(|pv| latest_stable > pv) {
        let (major, minor, patch) = latest_stable;
        return Ok(LibvipsPinStatus::NewerReleaseAvailable {
            latest: format!("{major}.{minor}.{patch}"),
        });
    }

    // The pin is current, but its digest could not be located to confirm it.
    if upstream_digest.is_none() {
        return Ok(LibvipsPinStatus::PinnedReleaseNotFound);
    }

    Ok(LibvipsPinStatus::UpToDate)
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
