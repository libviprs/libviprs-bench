#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# check-libvips-pin.sh — validate the pinned libvips against upstream (#36)
#
# The benchmark oracle is pinned by version + SHA-256
# (`provenance::PINNED_LIBVIPS_VERSION` / `PINNED_LIBVIPS_SHA256`, built from a
# source tarball by the Dockerfile — issue #33). A pin ages silently: upstream
# ships a newer release, or re-cuts the pinned tarball so the recorded digest no
# longer matches the bytes served. This check flags both against the upstream
# GitHub releases feed.
#
# It is a THIN wrapper. It only fetches the feed with `curl` and pipes it to the
# `check-libvips-pin` binary, which runs the one and only classifier
# (`pin_check::classify_libvips_pin`) over the recorded pin and maps the verdict
# to an exit code. There is NO second, shell-side reimplementation to drift from
# the Rust logic: the binary reads the pin constants at compile time, and the
# unit tests exercise that same `classify_libvips_pin`.
#
# It is deliberately ON-DEMAND — never a PR gate. This repo gates locally and
# skips GitHub CI on PR commits; run it by hand before a pin bump, or wire it
# into a NON-gating daily cron that alerts on a non-zero exit.
#
# Usage:  ./tools/check-libvips-pin.sh
# Needs:  curl (fetch) and cargo (runs the classifier binary).
# Exit:   0 up-to-date · 1 newer release or digest mismatch · 2 could not check.
# ---------------------------------------------------------------------------

API="https://api.github.com/repos/libvips/libvips/releases?per_page=20"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MANIFEST="${SCRIPT_DIR}/../Cargo.toml"

for tool in curl cargo; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Error: '${tool}' is required but not installed." >&2
        exit 2
    fi
done

echo "Querying ${API} ..." >&2

RELEASES="$(curl -fsSL -H 'Accept: application/vnd.github+json' "$API")" || {
    echo "Error: could not fetch the upstream releases feed." >&2
    exit 2
}

# Hand the payload to the single classifier. The binary prints the verdict and
# sets the exit code (0 up-to-date, 1 drift, 2 could-not-check); propagate it
# rather than re-deriving anything here.
set +e
printf '%s' "$RELEASES" \
    | cargo run --quiet --manifest-path "$MANIFEST" --bin check-libvips-pin
code=$?
set -e
exit "$code"
