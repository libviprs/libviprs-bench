//! On-demand validation of the pinned libvips against the upstream feed.
//!
//! [`provenance`](crate::provenance) fingerprints the environment *per run*.
//! This module serves a different, maintenance-time need with a different
//! collaborator: before a pin bump, confirm the recorded pin
//! ([`PINNED_LIBVIPS_VERSION`] / [`PINNED_LIBVIPS_SHA256`]) is still the latest
//! stable upstream libvips and that its tarball digest still matches the bytes
//! GitHub serves. It talks to the remote GitHub releases API rather than the
//! local host, so it is deliberately kept out of the per-snapshot
//! [`provenance`](crate::provenance) capture path.
//!
//! The classification itself is network-free: [`classify_libvips_pin`] takes a
//! payload the caller already fetched, so the *same* logic runs over a captured
//! fixture in tests and a live `curl` in the `check-libvips-pin` binary that
//! `tools/check-libvips-pin.sh` wraps. There is no second implementation to
//! drift from it. It is advisory, never a PR gate: this repo gates locally and
//! skips GitHub CI on PR commits (libviprs-bench #36).

use serde::Deserialize;

use crate::provenance::strip_libvips_prefixes;

// The pin constants live in `provenance` (stamped into every snapshot); re-export
// them here so a consumer of the validator can reach the pin it validates from
// one place.
pub use crate::provenance::{PINNED_LIBVIPS_SHA256, PINNED_LIBVIPS_VERSION};

/// Parse a libvips version/tag string down to `(major, minor, patch)`.
///
/// Accepts the GitHub release tag (`"v8.18.4"`), the `vips --version` line
/// (`"vips-8.18.4"`), and the bare pin (`"8.18.4"`) — prefix handling is shared
/// with [`crate::provenance::parse_libvips_major_minor`] via
/// `strip_libvips_prefixes`. Unlike that sibling, the patch component is
/// *required*: upstream releases always carry one, and [`classify_libvips_pin`]
/// compares whole `(major, minor, patch)` tuples to decide whether a newer
/// release exists. A pre-release tag (`"v8.18.3-rc1"`, whose patch is
/// non-numeric) yields `None`, so a release candidate can never rank as — or
/// newer than — a finished release.
pub fn parse_libvips_version(tag: &str) -> Option<(u32, u32, u32)> {
    let digits = strip_libvips_prefixes(tag);
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
/// `GET /repos/libvips/libvips/releases` payload. The on-demand validator (the
/// `check-libvips-pin` binary, `tools/check-libvips-pin.sh`, and the
/// `#[ignore]`d live test) renders it; it is deliberately advisory, never a PR
/// gate — this repo gates locally and skips GitHub CI on PR commits
/// (libviprs-bench #36).
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
    /// The pinned tarball's digest could not be located in the feed, so the
    /// SHA could not be validated — indeterminate, not a pass. Fires both when
    /// the pinned release is absent from the feed window (e.g. a pin newer than
    /// anything upstream) and when the release is present but its
    /// `vips-<version>.tar.xz` asset (or that asset's `digest`) is missing.
    /// Mirrors [`crate::provenance::OracleMatch::Indeterminate`]'s "no verdict"
    /// role.
    PinnedReleaseNotFound,
}

/// Why [`classify_libvips_pin`] could not reach a verdict from a payload.
#[derive(Debug)]
pub enum LibvipsPinError {
    /// The payload was not valid JSON in the expected releases-array shape.
    /// Wraps the underlying [`serde_json::Error`], preserved so
    /// [`std::error::Error::source`] exposes its structured line/column cause.
    Parse(serde_json::Error),
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

impl std::error::Error for LibvipsPinError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse(e) => Some(e),
            Self::NoStableRelease => None,
        }
    }
}

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
///
/// `pinned_version` may be given in any form [`parse_libvips_version`] accepts
/// (`"v8.18.4"` / `"vips-8.18.4"` / `"8.18.4"`); the tarball asset name is
/// rebuilt from the *parsed* `major.minor.patch`, so every accepted form
/// resolves to the same `vips-<x.y.z>.tar.xz` and the version compare and the
/// digest lookup can never disagree on the input's shape.
pub fn classify_libvips_pin(
    releases_json: &str,
    pinned_version: &str,
    pinned_sha256: &str,
) -> Result<LibvipsPinStatus, LibvipsPinError> {
    let releases: Vec<GhRelease> =
        serde_json::from_str(releases_json).map_err(LibvipsPinError::Parse)?;

    // Latest *stable* release: drafts and pre-releases (e.g. `v8.18.3-rc1`) are
    // never a bump target.
    let latest_stable = releases
        .iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| parse_libvips_version(&r.tag_name))
        .max()
        .ok_or(LibvipsPinError::NoStableRelease)?;

    // Locate the pinned *stable* release's tarball digest, if the feed carries
    // it. The asset name is rebuilt from the parsed tuple so a `v`-/`vips-`
    // prefixed pin resolves to the same `vips-<x.y.z>.tar.xz` the version
    // compare keys off, and only finished stable releases (never a draft or
    // pre-release parsing to the same version) may supply the digest.
    let pinned_ver = parse_libvips_version(pinned_version);
    let upstream_digest: Option<&str> = pinned_ver.and_then(|pv| {
        let (major, minor, patch) = pv;
        let tarball = format!("vips-{major}.{minor}.{patch}.tar.xz");
        releases
            .iter()
            .filter(|r| !r.draft && !r.prerelease && parse_libvips_version(&r.tag_name) == Some(pv))
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
