//! `check-libvips-pin` — classify the recorded libvips pin against an upstream
//! GitHub releases payload read from stdin, exiting with a status code the
//! `tools/check-libvips-pin.sh` wrapper (or a cron) can act on.
//!
//! This is the single on-demand classification path: it calls
//! [`classify_libvips_pin`] over the same [`PINNED_LIBVIPS_VERSION`] /
//! [`PINNED_LIBVIPS_SHA256`] the
//! benchmark records, so the operator check and the in-process tests can never
//! classify the same feed differently. The shell wrapper only fetches the feed
//! (`curl`) and pipes it here — there is no parallel shell-side classifier.
//!
//! Exit codes: `0` up-to-date · `1` an actionable drift (a newer release or a
//! digest mismatch) · `2` could not check (pin/asset absent from the feed
//! window, no stable release, or a malformed/unreadable payload).

use std::io::Read;
use std::process::ExitCode;

use libviprs_bench::pin_check::{
    LibvipsPinStatus, PINNED_LIBVIPS_SHA256, PINNED_LIBVIPS_VERSION, classify_libvips_pin,
};

fn main() -> ExitCode {
    let mut releases_json = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut releases_json) {
        eprintln!("check-libvips-pin: could not read the releases payload from stdin: {e}");
        return ExitCode::from(2);
    }

    match classify_libvips_pin(
        &releases_json,
        PINNED_LIBVIPS_VERSION,
        PINNED_LIBVIPS_SHA256,
    ) {
        Ok(LibvipsPinStatus::UpToDate) => {
            println!(
                "OK: pinned libvips {PINNED_LIBVIPS_VERSION} is the latest stable upstream and its \
                 tarball SHA-256 matches."
            );
            ExitCode::SUCCESS
        }
        Ok(LibvipsPinStatus::NewerReleaseAvailable { latest }) => {
            println!(
                "NEWER RELEASE: upstream latest stable is {latest} (pinned \
                 {PINNED_LIBVIPS_VERSION}) — bump the pin."
            );
            ExitCode::from(1)
        }
        Ok(LibvipsPinStatus::Sha256Mismatch { pinned, upstream }) => {
            eprintln!(
                "MISMATCH: upstream vips-{PINNED_LIBVIPS_VERSION}.tar.xz now advertises {upstream}, \
                 but the pin records {pinned}. Upstream re-cut the tarball, or the pin is wrong."
            );
            ExitCode::from(1)
        }
        Ok(LibvipsPinStatus::PinnedReleaseNotFound) => {
            eprintln!(
                "COULD NOT VERIFY: vips-{PINNED_LIBVIPS_VERSION}.tar.xz (or its digest) was not \
                 found in the upstream feed window, so the SHA-256 could not be checked."
            );
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("check-libvips-pin: {e}");
            ExitCode::from(2)
        }
    }
}
