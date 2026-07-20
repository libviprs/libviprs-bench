//! On-demand upstream-pin validator (libviprs-bench #36).
//!
//! The libvips oracle is pinned by version + SHA-256 (`provenance::
//! PINNED_LIBVIPS_VERSION` / `PINNED_LIBVIPS_SHA256`, built from source by the
//! Dockerfile). A pin ages silently: upstream ships a newer release, or —
//! worse — re-cuts the pinned tarball so the recorded digest no longer matches
//! the bytes served. This validator flags both against the upstream GitHub
//! releases feed. It is deliberately *on-demand* (a `tools/` script that pipes
//! the feed to the `check-libvips-pin` binary, plus an `#[ignore]`d live test),
//! never a PR gate: this repo gates locally and skips GitHub CI on PR commits.
//! The script, the binary, and the live test all call the one
//! `pin_check::classify_libvips_pin` — there is no parallel classifier.
//!
//! These are cheap, host-independent checks in the style of
//! `tests/libvips_provenance.rs` / `tests/pdfium_provenance.rs`: the
//! classification logic runs over a *captured* releases payload
//! (`tests/fixtures/libvips_releases_sample.json`, a trimmed real
//! `GET /repos/libvips/libvips/releases` response) so no network is touched.
//! One `#[ignore]`d test performs the real fetch on demand.

use libviprs_bench::pin_check::{
    LibvipsPinError, LibvipsPinStatus, PINNED_LIBVIPS_SHA256, PINNED_LIBVIPS_VERSION,
    classify_libvips_pin, parse_libvips_version,
};

/// Trimmed, real `GET /repos/libvips/libvips/releases` payload captured
/// 2026-07-20: `v8.18.4` (the pinned/latest stable), `v8.18.3`, `v8.18.3-rc1`
/// (a pre-release, present to prove it is skipped), and `v8.18.2`. Each asset
/// `digest` is the upstream SHA-256 verbatim. Re-capture it whenever the pin is
/// bumped (see [`recorded_pin_matches_the_captured_upstream_sample`]).
const RELEASES_SAMPLE: &str = include_str!("fixtures/libvips_releases_sample.json");

/// Upstream `vips-8.18.4.tar.xz` digest carried by the sample (equals the
/// recorded [`PINNED_LIBVIPS_SHA256`]), spelled out so the classification cases
/// read explicitly rather than reaching back into the JSON.
const V8_18_4_SHA256: &str = "2677bad6c422617fd1172d359c16af34e736965d042c214203a87187d26ff037";
/// Upstream `vips-8.18.3.tar.xz` digest carried by the sample.
const V8_18_3_SHA256: &str = "f41285b61bfb495605494f074ca341f7791a1d406e2f157dcea606ef1ae1b146";

/// The version parser reads the GitHub tag form, the `vips --version` line, and
/// the bare pin, and rejects pre-releases so an `-rc` is never a bump target.
#[test]
fn parse_libvips_version_reads_tag_cli_and_bare_forms() {
    assert_eq!(parse_libvips_version("v8.18.4"), Some((8, 18, 4))); // GitHub tag
    assert_eq!(parse_libvips_version("vips-8.18.4"), Some((8, 18, 4))); // CLI line
    assert_eq!(parse_libvips_version("8.18.4"), Some((8, 18, 4))); // bare pin

    // The strict ordering the newer-release check relies on.
    assert!(parse_libvips_version("v8.18.4") > parse_libvips_version("v8.18.3"));
    assert!(parse_libvips_version("v8.19.0") > parse_libvips_version("v8.18.4"));
    assert!(parse_libvips_version("v9.0.0") > parse_libvips_version("v8.18.4"));

    // A pre-release tag has no finished-release version, so it can never be
    // selected as "latest" — neither can the `"unknown"` sentinel or junk.
    assert_eq!(parse_libvips_version("v8.18.3-rc1"), None);
    assert_eq!(parse_libvips_version("unknown"), None);
    assert_eq!(parse_libvips_version(""), None);
}

/// Match case: the pin is the latest stable release AND its tarball digest
/// still matches upstream → nothing to do.
#[test]
fn classify_reports_up_to_date_when_pin_is_latest_and_matches() {
    let status = classify_libvips_pin(RELEASES_SAMPLE, "8.18.4", V8_18_4_SHA256)
        .expect("sample payload classifies");
    assert_eq!(status, LibvipsPinStatus::UpToDate);
}

/// Newer-release case: pinned to `8.18.3` while `8.18.4` is the latest stable
/// in the feed → flag the bump target.
#[test]
fn classify_flags_a_newer_stable_release() {
    let status = classify_libvips_pin(RELEASES_SAMPLE, "8.18.3", V8_18_3_SHA256)
        .expect("sample payload classifies");
    assert_eq!(
        status,
        LibvipsPinStatus::NewerReleaseAvailable {
            latest: "8.18.4".to_string()
        }
    );
}

/// A pre-release strictly newer than the pin must NOT be offered as a bump: the
/// feed here carries a `v9.0.0-rc1` pre-release above the stable `8.18.4` pin,
/// yet the verdict stays [`LibvipsPinStatus::UpToDate`].
#[test]
fn classify_ignores_a_higher_prerelease() {
    let feed = format!(
        r#"[
            {{"tag_name":"v9.0.0-rc1","draft":false,"prerelease":true,
              "assets":[{{"name":"vips-9.0.0.tar.xz","digest":"sha256:{V8_18_4_SHA256}"}}]}},
            {{"tag_name":"v8.18.4","draft":false,"prerelease":false,
              "assets":[{{"name":"vips-8.18.4.tar.xz","digest":"sha256:{V8_18_4_SHA256}"}}]}}
        ]"#
    );
    let status = classify_libvips_pin(&feed, "8.18.4", V8_18_4_SHA256).expect("feed classifies");
    assert_eq!(
        status,
        LibvipsPinStatus::UpToDate,
        "a pre-release must never be selected as the latest stable (would false-flag a bump)"
    );
}

/// SHA-mismatch case: same pinned version, but a recorded digest that no longer
/// matches the upstream tarball — upstream re-published, or the pin is wrong.
#[test]
fn classify_flags_a_sha256_mismatch() {
    let wrong = "0000000000000000000000000000000000000000000000000000000000000000";
    let status =
        classify_libvips_pin(RELEASES_SAMPLE, "8.18.4", wrong).expect("sample payload classifies");
    assert_eq!(
        status,
        LibvipsPinStatus::Sha256Mismatch {
            pinned: wrong.to_string(),
            upstream: V8_18_4_SHA256.to_string(),
        }
    );
}

/// Integrity beats freshness: pinned to `8.18.3` (behind `8.18.4`) AND its
/// recorded digest is wrong — the mismatch is reported ahead of the newer
/// release, so a re-published pinned tarball is never masked by "there is a
/// newer version anyway".
#[test]
fn integrity_mismatch_is_reported_ahead_of_a_newer_release() {
    let wrong = "1111111111111111111111111111111111111111111111111111111111111111";
    let status =
        classify_libvips_pin(RELEASES_SAMPLE, "8.18.3", wrong).expect("sample payload classifies");
    assert_eq!(
        status,
        LibvipsPinStatus::Sha256Mismatch {
            pinned: wrong.to_string(),
            upstream: V8_18_3_SHA256.to_string(),
        }
    );
}

/// A pin newer than anything upstream (or dropped from the feed window): the
/// digest cannot be validated and there is no newer release, so the verdict is
/// the indeterminate [`LibvipsPinStatus::PinnedReleaseNotFound`] — not a pass.
#[test]
fn classify_flags_a_pin_missing_from_the_feed() {
    let status = classify_libvips_pin(RELEASES_SAMPLE, "8.99.0", V8_18_4_SHA256)
        .expect("sample payload classifies");
    assert_eq!(status, LibvipsPinStatus::PinnedReleaseNotFound);
}

/// A feed with no stable release at all yields an error, not a false verdict.
#[test]
fn classify_errors_when_no_stable_release_present() {
    let only_pre = r#"[{"tag_name":"v9.0.0-rc1","prerelease":true,"assets":[]}]"#;
    assert!(matches!(
        classify_libvips_pin(only_pre, "8.18.4", V8_18_4_SHA256),
        Err(LibvipsPinError::NoStableRelease)
    ));
}

/// A malformed payload is a typed parse error, never a panic or a silent pass.
#[test]
fn classify_errors_on_malformed_payload() {
    assert!(matches!(
        classify_libvips_pin("{not json", "8.18.4", V8_18_4_SHA256),
        Err(LibvipsPinError::Parse(_))
    ));
}

/// Commit-time consistency guard: the committed pin (the `provenance`
/// constants) must agree with the committed upstream sample — right version,
/// right digest, latest stable at capture time. Because the pin and the fixture
/// are committed together and agree by construction, this can only ever catch a
/// copy-paste slip at commit time; it does NOT detect a pin aging against *live*
/// upstream — that is the job of [`live_upstream_pin_is_current`] and
/// `tools/check-libvips-pin.sh`. When the pin is bumped, re-capture
/// `tests/fixtures/libvips_releases_sample.json` in the same edit.
#[test]
fn recorded_pin_matches_the_captured_upstream_sample() {
    let status = classify_libvips_pin(
        RELEASES_SAMPLE,
        PINNED_LIBVIPS_VERSION,
        PINNED_LIBVIPS_SHA256,
    )
    .expect("captured sample classifies against the recorded pin");
    assert_eq!(
        status,
        LibvipsPinStatus::UpToDate,
        "recorded libvips pin ({PINNED_LIBVIPS_VERSION}) disagrees with the captured upstream \
         releases sample; if the pin was bumped, re-capture the fixture in the same edit"
    );
}

/// The committed on-demand validator script must fetch the upstream feed and
/// delegate classification to the single-source `check-libvips-pin` binary
/// (which calls [`classify_libvips_pin`]), never reimplement it shell-side. A
/// parallel jq/bash classifier is exactly what diverged from the Rust logic in
/// the #35/#36 review (a null digest read as a false MISMATCH; a mis-flagged
/// pre-release ranked as "latest"). Cheap source-level guard, no network.
#[test]
fn on_demand_validator_script_delegates_to_the_single_classifier() {
    const SCRIPT: &str = include_str!("../tools/check-libvips-pin.sh");
    assert!(
        SCRIPT.contains("api.github.com/repos/libvips/libvips/releases"),
        "validator must query the upstream libvips releases API (#36)"
    );
    assert!(
        SCRIPT.contains("--bin check-libvips-pin"),
        "validator must pipe the feed to the `check-libvips-pin` binary so the \
         single `classify_libvips_pin` is the only classifier (#36)"
    );
    // Guard against a regression to a parallel shell-side classifier: the old
    // jq `sort_by` "latest stable" ranking is precisely what diverged.
    assert!(
        !SCRIPT.contains("sort_by"),
        "the shell must not re-derive 'latest stable' itself; that jq ranking \
         diverged from `classify_libvips_pin` (#35/#36 review)"
    );
}

/// The version compare and the tarball-digest lookup must agree on the input
/// form. A `v`- or `vips-`-prefixed pin (both accepted by
/// [`parse_libvips_version`]) must still locate the
/// `vips-<major.minor.patch>.tar.xz` asset and validate — the pre-review code
/// built the asset name from the raw argument, so a `v8.18.4` pin produced
/// `vips-v8.18.4.tar.xz`, matched nothing, and read as a false
/// `PinnedReleaseNotFound` for a perfectly valid pin.
#[test]
fn classify_validates_prefixed_pin_forms() {
    for pin in ["v8.18.4", "vips-8.18.4", "8.18.4"] {
        let status = classify_libvips_pin(RELEASES_SAMPLE, pin, V8_18_4_SHA256)
            .unwrap_or_else(|e| panic!("sample classifies for pin {pin}: {e}"));
        assert_eq!(
            status,
            LibvipsPinStatus::UpToDate,
            "pin form {pin} must normalize and validate, not read as not-found"
        );
    }
}

/// Only a finished stable release may supply the digest the pin is validated
/// against. Here a `draft:true` `v8.18.4` carrying a bogus digest precedes the
/// real stable `v8.18.4` in document order; the verdict must stay
/// [`LibvipsPinStatus::UpToDate`], not a false `Sha256Mismatch` driven by the
/// draft's digest (the pre-review lookup filtered on version only).
#[test]
fn classify_ignores_a_draft_same_version_digest() {
    let bogus = "dead".repeat(16); // 64-hex, != the real v8.18.4 digest
    let feed = format!(
        r#"[
            {{"tag_name":"v8.18.4","draft":true,"prerelease":false,
              "assets":[{{"name":"vips-8.18.4.tar.xz","digest":"sha256:{bogus}"}}]}},
            {{"tag_name":"v8.18.4","draft":false,"prerelease":false,
              "assets":[{{"name":"vips-8.18.4.tar.xz","digest":"sha256:{V8_18_4_SHA256}"}}]}}
        ]"#
    );
    let status = classify_libvips_pin(&feed, "8.18.4", V8_18_4_SHA256).expect("feed classifies");
    assert_eq!(
        status,
        LibvipsPinStatus::UpToDate,
        "a draft release's digest must never drive the pin verdict"
    );
}

/// On-demand live validation against the real upstream feed. Ignored by
/// default (needs network + `curl`); run explicitly:
///
/// ```text
/// cargo test --test libvips_upstream_check -- --ignored
/// ```
///
/// A failure means the pinned libvips has genuinely aged (a newer release) or
/// its tarball digest drifted — refresh the pin (and the captured fixture).
#[test]
#[ignore = "hits the live GitHub releases API; run explicitly with --ignored"]
fn live_upstream_pin_is_current() {
    let out = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "-H",
            "Accept: application/vnd.github+json",
            "https://api.github.com/repos/libvips/libvips/releases?per_page=20",
        ])
        .output()
        .expect("curl must be available to run the live check");
    assert!(
        out.status.success(),
        "curl fetch of the upstream releases feed failed"
    );
    let json = String::from_utf8(out.stdout).expect("releases feed is UTF-8");
    let status = classify_libvips_pin(&json, PINNED_LIBVIPS_VERSION, PINNED_LIBVIPS_SHA256)
        .expect("live releases feed classifies");
    assert_eq!(
        status,
        LibvipsPinStatus::UpToDate,
        "pinned libvips {PINNED_LIBVIPS_VERSION} is no longer current upstream: {status:?}"
    );
}
