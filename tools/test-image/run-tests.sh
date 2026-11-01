#!/usr/bin/env bash
set -euo pipefail

# Runs the Rust test suite in an image that has both cargo and node (#135).
# Extra arguments go to `cargo test`. Uses whatever docker host is current.
#
#   tools/test-image/run-tests.sh
#   tools/test-image/run-tests.sh --test engines_family_end_to_end

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
tag="libviprs-bench-test:local"
# Cargo.toml has `libviprs = { path = "../libviprs" }`, so the core has to sit
# next to the bench tree inside the container too.
core="${LIBVIPRS_DIR:-$repo/../libviprs}"

docker build -q -t "$tag" "$here"
docker run --rm \
  -v "$repo":/work/libviprs-bench -v "$core":/work/libviprs -w /work/libviprs-bench \
  -e CARGO_TARGET_DIR=/tmp/target \
  "$tag" sh -c 'npm ci --no-audit --no-fund && exec cargo test "$@"' sh "$@"
