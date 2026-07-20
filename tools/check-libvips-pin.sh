#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# check-libvips-pin.sh — validate the pinned libvips against upstream (#36)
#
# The benchmark oracle is pinned by version + SHA-256
# (`provenance::PINNED_LIBVIPS_VERSION` / `PINNED_LIBVIPS_SHA256`, built from a
# source tarball by the Dockerfile — issue #33). A pin ages silently: upstream
# ships a newer release, or re-cuts the pinned tarball so the recorded digest no
# longer matches the bytes served. This script flags both against the upstream
# GitHub releases feed.
#
# It is deliberately ON-DEMAND — never a PR gate. This repo gates locally and
# skips GitHub CI on PR commits; run it by hand before a pin bump, or wire it
# into a NON-gating daily cron. It shares the exact classification the
# in-process validator uses (`provenance::classify_libvips_pin`), exercised in
# `tests/libvips_upstream_check.rs`.
#
# Usage:  ./tools/check-libvips-pin.sh
# Needs:  curl, jq (the same tools the Docker path and fixtures already use).
# Exit:   0 up-to-date · 1 newer release or digest mismatch · 2 could not check.
# ---------------------------------------------------------------------------

API="https://api.github.com/repos/libvips/libvips/releases?per_page=20"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROVENANCE="${SCRIPT_DIR}/../src/provenance.rs"

for tool in curl jq; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Error: '${tool}' is required but not installed." >&2
        exit 2
    fi
done

# Read the pin from the single source of truth (provenance.rs) so this check
# and the recorded constants can never drift apart. Tolerates the value sitting
# on the next line (rustfmt wraps the long SHA-256 literal).
extract_const() {
    tr '\n' ' ' <"$PROVENANCE" \
        | grep -oE "pub const $1: &str =[^\"]*\"[0-9A-Za-z.]+\"" \
        | grep -oE '"[0-9A-Za-z.]+"' \
        | tr -d '"' \
        | head -1
}

PINNED_LIBVIPS_VERSION="$(extract_const PINNED_LIBVIPS_VERSION)"
PINNED_LIBVIPS_SHA256="$(extract_const PINNED_LIBVIPS_SHA256)"

if [ -z "$PINNED_LIBVIPS_VERSION" ] || [ -z "$PINNED_LIBVIPS_SHA256" ]; then
    echo "Error: could not read the pin from ${PROVENANCE}." >&2
    exit 2
fi

echo "Pinned libvips: ${PINNED_LIBVIPS_VERSION} (sha256 ${PINNED_LIBVIPS_SHA256})"
echo "Querying ${API} ..."

RELEASES="$(curl -fsSL -H 'Accept: application/vnd.github+json' "$API")" || {
    echo "Error: could not fetch the upstream releases feed." >&2
    exit 2
}

# Latest stable release (skip drafts and pre-releases), tag stripped of `v` and
# ordered by numeric (major, minor, patch).
LATEST="$(printf '%s' "$RELEASES" \
    | jq -r '[.[] | select(.draft == false and .prerelease == false)
                  | .tag_name | ltrimstr("v")]
             | sort_by(split(".") | map(tonumber? // 0)) | last // empty')"

# Upstream digest of the pinned tarball, if the feed still carries it.
UPSTREAM_SHA="$(printf '%s' "$RELEASES" \
    | jq -r --arg v "$PINNED_LIBVIPS_VERSION" \
        '.[] | select((.tag_name | ltrimstr("v")) == $v)
             | .assets[]? | select(.name == "vips-\($v).tar.xz")
             | .digest | ltrimstr("sha256:")' \
    | head -1)"

status=0

# Integrity first: a re-cut pinned tarball outranks a mere newer release.
if [ -n "$UPSTREAM_SHA" ] && [ "$UPSTREAM_SHA" != "$PINNED_LIBVIPS_SHA256" ]; then
    echo "MISMATCH: upstream vips-${PINNED_LIBVIPS_VERSION}.tar.xz is now ${UPSTREAM_SHA}," >&2
    echo "          but the pin records ${PINNED_LIBVIPS_SHA256}." >&2
    status=1
elif [ -z "$UPSTREAM_SHA" ]; then
    echo "WARNING: vips-${PINNED_LIBVIPS_VERSION}.tar.xz not found in the feed; digest unverified." >&2
fi

# Then freshness: a strictly newer latest stable is a bump target.
if [ -n "$LATEST" ] && [ "$LATEST" != "$PINNED_LIBVIPS_VERSION" ]; then
    newest="$(printf '%s\n%s\n' "$PINNED_LIBVIPS_VERSION" "$LATEST" \
        | sort -t. -k1,1n -k2,2n -k3,3n | tail -1)"
    if [ "$newest" = "$LATEST" ]; then
        echo "NEWER RELEASE: upstream latest stable is ${LATEST} (pinned ${PINNED_LIBVIPS_VERSION})."
        status=1
    fi
fi

if [ "$status" -eq 0 ]; then
    echo "OK: pinned libvips ${PINNED_LIBVIPS_VERSION} is the latest stable and its SHA-256 matches upstream."
fi
exit "$status"
