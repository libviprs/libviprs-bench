//! libvips pin + provenance guard (#24, sub-issue #33).
//!
//! The benchmark container must measure a *recent, matched* libvips, built
//! from a pinned upstream source tarball — not Debian bookworm's frozen
//! `libvips-dev` (~8.14.x), which trails the `libvips-rs` 8.18 bindings by
//! years and makes the C oracle an unfair, mismatched baseline (#33). The
//! pinned version is recorded as a provenance axis
//! ([`PINNED_LIBVIPS_VERSION`]) so every containerized run stamps the exact
//! libvips it was built to measure.
//!
//! A Dockerfile can't be unit-tested in-process, so these are cheap
//! source-level checks in the style of `tests/pdfium_provenance.rs`: they
//! read only committed files (the Dockerfile, Cargo.toml, and the recorded
//! provenance constant) and fail the moment the pin drifts or the Dockerfile
//! regresses to an unpinned / apt-installed libvips — without needing Docker
//! or libvips in the loop.
//!
//! Sub-issue #35 extends the same reproducibility guard to the rest of the
//! build environment: the base images must be digest-pinned (`FROM …@sha256:`)
//! and the apt toolchain (libpng et al. — the DeepZoom PNG hot path) pinned to
//! a dated `snapshot.debian.org` mirror, so no layer floats on a live tag or a
//! live mirror between benchmark runs.

use libviprs_bench::provenance::{
    OracleMatch, PINNED_LIBVIPS_SHA256, PINNED_LIBVIPS_VERSION, Provenance,
    parse_libvips_major_minor,
};

const DOCKERFILE: &str = include_str!("../Dockerfile");
const CARGO_TOML: &str = include_str!("../Cargo.toml");

/// Test A — the recorded libvips version parses to the expected major.minor.
///
/// Exercises the shared parser over a sample `vips --version` line (the exact
/// shape `provenance::libvips_version` reads from the CLI fallback) and over
/// the already-stripped form it stores, and confirms the pin sits on the
/// 8.18 series the bindings target.
#[test]
fn recorded_libvips_version_parses_to_expected_major_minor() {
    // The pin is itself a well-formed version on the expected series.
    let parsed = parse_libvips_major_minor(PINNED_LIBVIPS_VERSION)
        .expect("PINNED_LIBVIPS_VERSION must parse to major.minor");
    assert_eq!(
        parsed,
        (8, 18),
        "the pinned libvips must be on the 8.18 series the bindings target (#33)"
    );

    // Raw `vips --version` output ("vips-8.18.4") parses identically to the
    // `vips-`-stripped form provenance stores ("8.18.4").
    assert_eq!(parse_libvips_major_minor("vips-8.18.4"), Some((8, 18)));
    assert_eq!(parse_libvips_major_minor("8.18.4"), Some((8, 18)));
    assert_eq!(
        parse_libvips_major_minor("vips-8.18.4"),
        parse_libvips_major_minor(PINNED_LIBVIPS_VERSION),
        "the raw CLI line and the recorded pin must agree on major.minor"
    );

    // The frozen Debian oracle (#33) is a *different* series — the parser
    // must distinguish it so the drift is detectable, not silently equal.
    assert_eq!(parse_libvips_major_minor("vips-8.14.1"), Some((8, 14)));
    assert_ne!(
        parse_libvips_major_minor("vips-8.14.1"),
        parse_libvips_major_minor(PINNED_LIBVIPS_VERSION),
        "Debian's 8.14 must not compare equal to the 8.18 pin"
    );

    // Non-versions never masquerade as a real capture.
    assert_eq!(parse_libvips_major_minor("unknown"), None);
    assert_eq!(parse_libvips_major_minor(""), None);
}

/// The Dockerfile must build libvips from a pinned upstream *source* tarball,
/// not install Debian's frozen `libvips-dev` (#33).
#[test]
fn dockerfile_builds_pinned_libvips_from_source() {
    // Downloads the official upstream release tarball...
    assert!(
        DOCKERFILE.contains("github.com/libvips/libvips/releases/download"),
        "Dockerfile must download the upstream libvips source tarball (#33)"
    );
    // ...and compiles it from source via libvips' meson/ninja build.
    assert!(
        DOCKERFILE.contains("meson setup") && DOCKERFILE.contains("ninja -C build install"),
        "Dockerfile must build libvips from source via meson/ninja (#33)"
    );

    // The mismatched oracle is gone: libvips is built from source, not
    // apt-installed as a version-pinned Debian `libvips-dev` / `libvips-tools`
    // package. Checked against build instructions only — comments may still
    // explain what the ~8.14 apt oracle is being replaced with.
    let instructions = dockerfile_instructions(DOCKERFILE);
    assert!(
        !instructions.contains("libvips-dev=") && !instructions.contains("libvips-tools="),
        "Dockerfile must not apt-install a version-pinned Debian libvips; that \
         is the frozen ~8.14 oracle #33 replaces with a source build"
    );
}

/// The libvips download must be integrity-checked before it is built: a
/// pinned URL without a digest still trusts the remote end forever. Mirrors
/// the PDFium checksum guard (`tests/pdfium_provenance.rs`).
#[test]
fn dockerfile_verifies_libvips_tarball_checksum() {
    assert!(
        DOCKERFILE.contains("sha256sum -c"),
        "Dockerfile must verify the libvips tarball against a pinned SHA-256 \
         with `sha256sum -c` (#33)"
    );
    assert!(
        DOCKERFILE.contains(PINNED_LIBVIPS_SHA256),
        "Dockerfile is missing the pinned SHA-256 digest \
         (`provenance::PINNED_LIBVIPS_SHA256`) for \
         vips-{PINNED_LIBVIPS_VERSION}.tar.xz (#33)"
    );
}

/// Repo-consistency (Test B): the libvips version pinned in the Dockerfile
/// and the version recorded in provenance/config must not silently drift.
/// Both are committed; this reads them and compares.
#[test]
fn dockerfile_libvips_version_matches_recorded_provenance() {
    let arg = dockerfile_arg(DOCKERFILE, "LIBVIPS_VERSION")
        .expect("Dockerfile must declare `ARG LIBVIPS_VERSION=<version>`");
    assert_eq!(
        arg, PINNED_LIBVIPS_VERSION,
        "Dockerfile `ARG LIBVIPS_VERSION` ({arg}) must equal \
         `provenance::PINNED_LIBVIPS_VERSION` ({PINNED_LIBVIPS_VERSION}) so the \
         built oracle and the recorded provenance axis cannot drift (#33)"
    );

    // The download path builds the tarball name from that same ARG (Docker
    // expands `${LIBVIPS_VERSION}` at build time), so the URL, the pin, and
    // the recorded axis are one value, not three that can drift apart.
    assert!(
        DOCKERFILE.contains("vips-${LIBVIPS_VERSION}.tar.xz"),
        "the libvips download path must build the tarball name from \
         `${{LIBVIPS_VERSION}}` so the URL and the pin cannot drift (#33)"
    );
}

/// The `libvips-rs` binding must track the pinned libvips series: measuring
/// libvips 8.18 through a different-series binding would be its own mismatch.
/// Assert the Cargo dependency's major.minor equals the pin's.
#[test]
fn cargo_libvips_binding_aligns_with_pinned_version() {
    let pin = parse_libvips_major_minor(PINNED_LIBVIPS_VERSION).unwrap();
    let req = cargo_libvips_rs_req(CARGO_TOML)
        .expect("Cargo.toml must declare a `libvips-rs` dependency with a version");
    let binding = parse_libvips_major_minor(&req)
        .expect("the libvips-rs version requirement must be a major.minor string");
    assert_eq!(
        binding, pin,
        "libvips-rs binding ({req}) must track the pinned libvips series \
         {}.{} (#33)",
        pin.0, pin.1
    );
}

/// The recorded pinned axis is compared against the measured version to flag
/// a mismatched oracle (#33). Host-independent: the version strings are set
/// explicitly rather than probed from the host libvips.
#[test]
fn provenance_flags_measured_vs_pinned_libvips_drift() {
    // A pre-provenance/default fingerprint (everything "unknown") must never
    // read as a match — a missing capture is not a hit.
    let mut prov = Provenance::default();
    assert!(!prov.libvips_matches_pinned());

    prov.pinned_libvips_version = PINNED_LIBVIPS_VERSION.to_string();

    // Measured the pinned oracle (patch may differ) → match at major.minor.
    prov.libvips_version = "8.18.2".to_string();
    assert!(
        prov.libvips_matches_pinned(),
        "same major.minor as the pin must match"
    );

    // Measured Debian's frozen 8.14 while pinned to 8.18 → the mismatched
    // oracle #33 closes must be flagged.
    prov.libvips_version = "8.14.1".to_string();
    assert!(
        !prov.libvips_matches_pinned(),
        "an 8.14 measurement against an 8.18 pin must be flagged as drift"
    );
}

/// The richer [`OracleMatch`] classification distinguishes a genuine
/// mismatched oracle (both sides parse and differ) from an indeterminate
/// capture (either side `"unknown"`), so a consumer can warn loudly on the
/// former without crying wolf on the latter (#33).
#[test]
fn oracle_match_classifies_mismatch_vs_indeterminate() {
    // Default fingerprint: both sides "unknown" → indeterminate, not a match.
    let mut prov = Provenance::default();
    assert_eq!(prov.libvips_oracle_match(), OracleMatch::Indeterminate);
    assert!(!prov.libvips_matches_pinned());

    prov.pinned_libvips_version = PINNED_LIBVIPS_VERSION.to_string();

    // Only the measured side unknown (a host run with no libvips) → still
    // indeterminate: the guard must not raise a false mismatch alarm.
    assert_eq!(prov.libvips_oracle_match(), OracleMatch::Indeterminate);

    // Same major.minor as the pin (patch may differ) → match.
    prov.libvips_version = "8.18.2".to_string();
    assert_eq!(prov.libvips_oracle_match(), OracleMatch::Match);
    assert!(prov.libvips_matches_pinned());

    // Debian's 8.14 against the 8.18 pin → a concrete mismatch carrying both
    // parsed series, so a consumer can render the exact versions.
    prov.libvips_version = "8.14.1".to_string();
    assert_eq!(
        prov.libvips_oracle_match(),
        OracleMatch::Mismatch {
            measured: (8, 14),
            pinned: (8, 18),
        }
    );
    assert!(!prov.libvips_matches_pinned());
}

/// The build must force-enable the hot-path codecs rather than leave them to
/// meson's `auto` detection, so a missing codec `-dev` library hard-fails the
/// build instead of silently yielding a codec-less oracle — a PNG-less libvips
/// would quietly invalidate the DeepZoom PNG-tile hot path (#33).
#[test]
fn dockerfile_force_enables_hot_path_codecs() {
    for opt in [
        "-Dpng=enabled",
        "-Djpeg=enabled",
        "-Dtiff=enabled",
        "-Dwebp=enabled",
    ] {
        assert!(
            DOCKERFILE.contains(opt),
            "Dockerfile must force-enable the codec with `{opt}` so a missing \
             library fails the build instead of silently disabling the format (#33)"
        );
    }
}

/// The post-build check must assert the discoverable libvips is *exactly* the
/// pinned version and fail the build otherwise — merely printing the version
/// (the pre-review behaviour) would let a stray/wrong libvips ahead on PATH
/// build green (#33).
#[test]
fn dockerfile_asserts_built_libvips_version_equals_pin() {
    let instructions = dockerfile_instructions(DOCKERFILE);
    assert!(
        instructions.contains("pkg-config --modversion vips")
            && instructions.contains("${LIBVIPS_VERSION}")
            && instructions.contains("exit 1"),
        "Dockerfile must compare `pkg-config --modversion vips` against \
         ${{LIBVIPS_VERSION}} and `exit 1` on mismatch, not just print it (#33)"
    );
}

/// #35 (Dockerfile reproducibility): every base image must be *digest*-pinned
/// (`FROM …@sha256:<64-hex>`), not left on a floating tag. A tag can be re-cut
/// upstream to point at different bytes; a digest fixes the exact base layer so
/// it cannot shift under a rebuild.
#[test]
fn dockerfile_digest_pins_base_images() {
    let froms: Vec<&str> = DOCKERFILE
        .lines()
        .filter(|line| line.trim_start().starts_with("FROM "))
        .collect();
    assert!(!froms.is_empty(), "Dockerfile must declare FROM stages");
    for from in &froms {
        assert!(
            from.contains("@sha256:"),
            "every base image must be digest-pinned for reproducibility (#35); \
             `{}` is not",
            from.trim()
        );
        assert!(
            has_wellformed_sha256_digest(from),
            "`{}` must carry a well-formed 64-hex `@sha256:` digest, not a \
             placeholder (#35)",
            from.trim()
        );
    }
}

/// #35: the apt build toolchain (libpng, glib, jpeg, tiff, webp, expat `-dev`)
/// must be pinned to a dated `snapshot.debian.org` mirror, not resolved against
/// the live bookworm mirror where an intra-suite point release (e.g. a libpng
/// security update) can shift the encode hot path under a rebuild. This is
/// exactly the float the #24 scope note deferred to #35.
#[test]
fn dockerfile_pins_apt_to_a_debian_snapshot() {
    let instructions = dockerfile_instructions(DOCKERFILE);
    assert!(
        instructions.contains("snapshot.debian.org/archive/debian"),
        "Dockerfile must point apt at a snapshot.debian.org archive so the codec \
         `-dev` libraries resolve to fixed versions, not the live mirror (#35)"
    );
    // A concrete, well-formed snapshot timestamp (YYYYMMDDThhmmssZ), not a bare
    // archive root that would still float.
    assert!(
        contains_snapshot_timestamp(&instructions),
        "the snapshot.debian.org URL must carry a dated timestamp \
         (e.g. 20250929T000000Z), not a floating archive root (#35)"
    );
    // Snapshot Release files carry an old `Valid-Until`; apt must be told to
    // accept them or `apt-get update` rejects the dated snapshot outright.
    assert!(
        instructions.contains("Check-Valid-Until"),
        "apt must set `Acquire::Check-Valid-Until false` to read a dated \
         snapshot (#35)"
    );
}

/// #35 consistency: the apt snapshot and the digest-pinned Debian base must
/// describe the *same* point in time. The base image tag encodes its build date
/// (`bookworm-YYYYMMDD`) and Debian embeds the matching snapshot, so the
/// snapshot timestamp must carry that same date — one frozen point for the whole
/// image, not two that can drift apart.
#[test]
fn apt_snapshot_matches_the_pinned_base_image_date() {
    let base = base_image_date(DOCKERFILE)
        .expect("a Debian base image must carry a `bookworm-YYYYMMDD` date (#35)");
    let snap = snapshot_date(&dockerfile_instructions(DOCKERFILE))
        .expect("the apt snapshot must carry a `YYYYMMDD` timestamp (#35)");
    assert_eq!(
        base, snap,
        "the apt snapshot date ({snap}) must match the pinned Debian base image \
         date ({base}) so the whole image is one frozen point in time (#35)"
    );
}

/// Dockerfile text with `#` comment lines stripped, so *negative* assertions
/// target build instructions rather than explanatory prose (a comment may
/// still name the ~8.14 apt oracle the source build replaces).
fn dockerfile_instructions(dockerfile: &str) -> String {
    dockerfile
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract the default value of `ARG <name>=<value>` from Dockerfile text.
fn dockerfile_arg(dockerfile: &str, name: &str) -> Option<String> {
    let needle = format!("ARG {name}=");
    dockerfile
        .lines()
        .find_map(|line| line.trim().strip_prefix(&needle))
        .map(|value| {
            value
                .split_whitespace()
                .next()
                .unwrap_or(value)
                .trim()
                .to_string()
        })
}

/// Pull the version requirement string out of the `libvips-rs` dependency
/// line in a Cargo manifest, stripping any leading semver comparator so a
/// tightened `~8.18` / `=8.18.4` still parses to major.minor.
fn cargo_libvips_rs_req(cargo: &str) -> Option<String> {
    let line = cargo
        .lines()
        .find(|line| line.trim_start().starts_with("libvips-rs"))?;
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')? + start;
    Some(
        line[start..end]
            .trim_start_matches(['=', '^', '~', '>', '<', ' '])
            .to_string(),
    )
}

/// Whether a `FROM` line carries a well-formed `@sha256:<64 hex>` digest pin.
fn has_wellformed_sha256_digest(line: &str) -> bool {
    match line.split_once("@sha256:") {
        Some((_, rest)) => {
            let hex = rest
                .chars()
                .take_while(|c| c.is_ascii_hexdigit())
                .count();
            hex == 64
        }
        None => false,
    }
}

/// Whether `text` contains a `snapshot.debian.org` timestamp token
/// (`YYYYMMDDThhmmssZ`, e.g. `20250929T000000Z`).
fn contains_snapshot_timestamp(text: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .any(is_snapshot_timestamp)
}

/// Match a single `YYYYMMDDThhmmssZ` snapshot timestamp token.
fn is_snapshot_timestamp(token: &str) -> bool {
    let b = token.as_bytes();
    b.len() == 16
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'T'
        && b[9..15].iter().all(u8::is_ascii_digit)
        && b[15] == b'Z'
}

/// Extract the `YYYYMMDD` date from the first `bookworm-YYYYMMDD` base-image tag
/// (the dated Debian image); ignores undated `-bookworm` tags and the
/// `bookworm-updates` / `bookworm-security` suite names.
fn base_image_date(dockerfile: &str) -> Option<String> {
    dockerfile.split("bookworm-").skip(1).find_map(|rest| {
        let date: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        (date.len() == 8).then_some(date)
    })
}

/// Extract the `YYYYMMDD` prefix of the `snapshot.debian.org` timestamp token.
fn snapshot_date(dockerfile: &str) -> Option<String> {
    dockerfile
        .split(|c: char| !c.is_ascii_alphanumeric())
        .find(|token| is_snapshot_timestamp(token))
        .map(|token| token[..8].to_string())
}
