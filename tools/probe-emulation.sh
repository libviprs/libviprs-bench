#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# probe-emulation.sh — the positive control for the emulation probe.
#
# A probe that always answers `false` passes every Rust unit test anyone could
# write for it, which is why the thing the finding actually rests on is this
# shell script and not a `#[test]`. It builds the probe twice on one machine
# and asserts opposite answers:
#
#   --platform linux/amd64 on an arm64 host  ->  emulated: true
#   --platform linux/arm64 on an arm64 host  ->  emulated: false
#
# Nothing about the probe's source differs between those two runs, so a probe
# hard-wired to either answer fails one of them. Run it on an amd64 host and
# the roles swap; the script works out which way round it should be from the
# Docker daemon's own architecture and says so before it asserts anything.
#
# The probe is compiled by bare `rustc` from the single self-contained file
# `src/probe_emulation_main.rs`, which pulls in `src/emulation.rs` by `#[path]`
# and uses nothing outside `std`. That is deliberate: `cargo build` here would
# drag the whole `libviprs` path dependency through an emulated compiler for a
# probe that is two hundred lines of `/proc` reads.
#
# Usage:
#   ./tools/probe-emulation.sh control                   # the two-sided assertion
#   ./tools/probe-emulation.sh run linux/amd64           # one platform, prints JSON
#   ./tools/probe-emulation.sh run linux/arm64 --blind   # ... without BENCH_DAEMON_ARCH,
#                                                        #     so only /proc evidence counts
# ---------------------------------------------------------------------------

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUST_IMAGE="${PROBE_RUST_IMAGE:-rust:1.98-slim-bookworm}"

die() { echo "probe-emulation: $*" >&2; exit 1; }

# The daemon's own architecture, in Docker's spelling (`arm64` / `amd64`). This
# is the one piece of evidence the probe cannot observe from inside its own
# container, so the runner supplies it and the probe records that it did.
daemon_arch() {
    docker version --format '{{.Server.Arch}}' 2>/dev/null || echo "unknown"
}

# Map Docker's arch spelling onto the one `std::env::consts::ARCH` uses, so the
# expectation below is written in the probe's own vocabulary.
rust_arch_for_platform() {
    case "$1" in
        linux/amd64) echo "x86_64" ;;
        linux/arm64|linux/arm64/v8) echo "aarch64" ;;
        *) die "unsupported platform $1" ;;
    esac
}

docker_arch_to_rust() {
    case "$1" in
        amd64) echo "x86_64" ;;
        arm64) echo "aarch64" ;;
        *) echo "unknown" ;;
    esac
}

# Build and run the probe under one platform. Prints the probe's JSON on stdout;
# everything else goes to stderr so a caller can pipe this straight into a parser.
run_probe() {
    local platform="$1" blind="${2:-}"
    local env_args=()
    if [[ "$blind" != "--blind" ]]; then
        env_args+=(-e "BENCH_DAEMON_ARCH=$(daemon_arch)")
    fi
    echo "probe-emulation: building and running under ${platform}${blind:+ (blind: no BENCH_DAEMON_ARCH)}" >&2
    docker run --rm --platform "$platform" \
        -v "${REPO_ROOT}:/src:ro" \
        "${env_args[@]}" \
        "$RUST_IMAGE" \
        sh -c 'rustc --edition 2024 -O -o /tmp/probe-emulation /src/src/probe_emulation_main.rs && exec /tmp/probe-emulation'
}

# Pull `"emulated": <value>` out of the probe's JSON without needing jq in the
# caller's environment. The probe prints one key per line for exactly this reason.
# `sed -E` rather than a BRE with `\|`: the alternation is a GNU extension that
# macOS's BSD sed does not take, and this script's first home is a Mac, where it
# silently matched nothing and reported both sides unparseable.
emulated_field() {
    sed -n -E 's/.*"emulated": (true|false|"unknown").*/\1/p' | head -1
}

cmd_run() {
    local platform="${1:-}" blind="${2:-}"
    [[ -n "$platform" ]] || die "usage: $0 run <linux/amd64|linux/arm64> [--blind]"
    run_probe "$platform" "$blind"
}

cmd_control() {
    local host_arch native_platform foreign_platform
    host_arch="$(docker_arch_to_rust "$(daemon_arch)")"
    case "$host_arch" in
        aarch64) native_platform="linux/arm64"; foreign_platform="linux/amd64" ;;
        x86_64)  native_platform="linux/amd64"; foreign_platform="linux/arm64" ;;
        *) die "cannot tell what this Docker daemon runs on (arch: $(daemon_arch))" ;;
    esac

    echo "probe-emulation: daemon arch $(daemon_arch) ($host_arch)" >&2
    echo "probe-emulation: expecting emulated=false under ${native_platform} and true under ${foreign_platform}" >&2

    local native_json foreign_json native_verdict foreign_verdict blind_json blind_verdict
    native_json="$(run_probe "$native_platform")"
    foreign_json="$(run_probe "$foreign_platform")"
    # The foreign run again with BENCH_DAEMON_ARCH withheld. If the /proc evidence
    # is blind on this host the runner-supplied arch is the only thing carrying the
    # verdict, and I want the control to say which of the two it was.
    blind_json="$(run_probe "$foreign_platform" --blind)"

    native_verdict="$(printf '%s\n' "$native_json" | emulated_field)"
    foreign_verdict="$(printf '%s\n' "$foreign_json" | emulated_field)"
    blind_verdict="$(printf '%s\n' "$blind_json" | emulated_field)"

    echo
    echo "=== native (${native_platform}) ==="
    printf '%s\n' "$native_json"
    echo
    echo "=== foreign (${foreign_platform}) ==="
    printf '%s\n' "$foreign_json"
    echo
    echo "=== foreign, blind (${foreign_platform}, no BENCH_DAEMON_ARCH) ==="
    printf '%s\n' "$blind_json"
    echo

    local failures=0
    if [[ "$native_verdict" != "false" ]]; then
        echo "FAIL: ${native_platform} reported emulated=${native_verdict:-<unparseable>}, expected false" >&2
        failures=$((failures + 1))
    else
        echo "PASS: ${native_platform} reported emulated=false" >&2
    fi
    if [[ "$foreign_verdict" != "true" ]]; then
        echo "FAIL: ${foreign_platform} reported emulated=${foreign_verdict:-<unparseable>}, expected true" >&2
        failures=$((failures + 1))
    else
        echo "PASS: ${foreign_platform} reported emulated=true" >&2
    fi
    if [[ "$blind_verdict" != "true" ]]; then
        echo "NOTE: with BENCH_DAEMON_ARCH withheld the ${foreign_platform} run reported \
emulated=${blind_verdict:-<unparseable>}, so on this host the /proc evidence alone is not \
enough and the runner-supplied daemon arch is load-bearing." >&2
    else
        echo "PASS: ${foreign_platform} reported emulated=true from /proc evidence alone" >&2
    fi

    if (( failures > 0 )); then
        die "$failures of the two required assertions failed"
    fi
    echo "probe-emulation: both required assertions hold on this machine" >&2
}

case "${1:-control}" in
    control) cmd_control ;;
    run) shift; cmd_run "$@" ;;
    -h|--help) sed -n '4,30p' "${BASH_SOURCE[0]}" ;;
    *) die "unknown command ${1}" ;;
esac
