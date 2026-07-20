#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# run-bench.sh — Build and run libviprs benchmarks in Docker
#
# Provides a controlled, pinned environment where libvips (C) and libviprs
# (Rust) run side-by-side with identical inputs: every engine writes PNG
# tiles to a real on-disk sink with the same codec, so neither side gets an
# in-RAM-sink or encoding advantage.
#
# Usage:
#   ./run-bench.sh                             # scalability benchmark (default)
#   ./run-bench.sh report                      # full comparison report
#   ./run-bench.sh versions --versions v0.2.0,v0.3.1,HEAD
#                                              # release-history axis (one snapshot per tag)
#   ./run-bench.sh --arch arm                  # force arm64 build
#   ./run-bench.sh --memory 4096               # container memory limit in MB
#   ./run-bench.sh --no-build                  # run locally (requires libvips-dev)
#
# Output is written to report/ (charts, JSON, text tables).
# ---------------------------------------------------------------------------

ARCH=""
NO_BUILD=false
MEMORY_MB=""
BENCH_CMD="scalability"
VERSIONS=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-build) NO_BUILD=true ;;
        --memory)
            shift
            if [[ $# -eq 0 ]] || ! [[ "$1" =~ ^[0-9]+$ ]]; then
                echo "Error: --memory requires a numeric value in MB"
                exit 1
            fi
            MEMORY_MB="$1"
            ;;
        --arch)
            shift
            ARCH="$1"
            ;;
        --versions)
            shift
            VERSIONS="$1"
            ;;
        report|scalability)
            BENCH_CMD="$1"
            ;;
        versions)
            BENCH_CMD="version_matrix"
            ;;
        *)
            echo "Unknown argument: $1"
            echo "Usage: $0 [scalability|report|versions] [--versions v0.2.0,v0.3.1,HEAD] [--arch arm|amd64] [--memory MB] [--no-build]"
            exit 1
            ;;
    esac
    shift
done

MEMORY_MB="${MEMORY_MB:-4096}"

# The version-matrix runner drives its own per-tag git worktrees + cargo builds
# of the sibling core crate, so it must run on the host toolchain rather than
# inside the pinned Docker image (which carries no git worktree topology).
# Force local execution for it.
#
# Consequence: unlike every other axis (measured inside the pinned container),
# the release-history axis is measured on THIS host's toolchain/libvips. Its
# snapshots are a self-contained series — compare them only within themselves,
# not against Docker-measured snapshots in the same history. Each snapshot
# records its environment fingerprint, and cross_version flags cross-environment
# cells with `env≠`, so the two are never silently mixed.
if [ "$BENCH_CMD" = "version_matrix" ]; then
    NO_BUILD=true
    if [ -z "$VERSIONS" ]; then
        echo "Error: 'versions' mode requires --versions <tag,tag,HEAD>"
        exit 1
    fi
fi

# Detect architecture
ARCH="${ARCH:-$(uname -m)}"
case "$ARCH" in
    arm|arm64|aarch64)
        PLATFORM="linux/arm64"
        ARCH_LABEL="arm64"
        ;;
    amd64|x86_64|x64)
        PLATFORM="linux/amd64"
        ARCH_LABEL="amd64"
        ;;
    *)
        echo "Error: unsupported architecture '${ARCH}'. Use 'arm' or 'amd64'."
        exit 1
        ;;
esac

CONTAINER_NAME="libviprs-bench"
IMAGE_NAME="libviprs-bench:local"

# The libvips release the Docker image builds from source (issue #33), read
# from the Dockerfile so this script and the image share a single pin. Shown
# in the banners below; inside the container it is also recorded into each
# snapshot's provenance (the measured-vs-pinned oracle).
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LIBVIPS_PIN="$(sed -n 's/^ARG LIBVIPS_VERSION=//p' "$SCRIPT_DIR/Dockerfile" | head -1)"

# Pinned measurement RUSTFLAGS. Recorded into every snapshot's provenance
# (build.rs stamps $RUSTFLAGS) so the reported numbers carry the exact
# codegen config they were measured under. Kept in lockstep with the
# [profile.release] pin in Cargo.toml (issue #162).
export RUSTFLAGS="${RUSTFLAGS:--C target-cpu=native}"

# Directory containing this script (the crate root). Defined up-front so both
# the local and Docker paths can locate report/ and the JS chart renderer.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Regenerate the history + scalability SVGs from the JSON the Rust harness
# just wrote, using the causl-bench JS chart library (tools/charts/render.mjs).
# The Rust binaries only emit JSON now; charts are rendered here so a single
# run always refreshes report/*.svg. Skipped with a hint if node is missing —
# the JSON is the source of truth and can be re-rendered later.
regenerate_charts() {
    local report_dir="$1"
    if command -v node >/dev/null 2>&1; then
        echo ""
        echo "Regenerating SVG charts from JSON (tools/charts/render.mjs)..."
        node "$SCRIPT_DIR/tools/charts/render.mjs" --report-dir "$report_dir"
    else
        echo ""
        echo "node not found — skipping SVG chart regeneration."
        echo "  JSON written; run: node tools/charts/render.mjs --report-dir $report_dir"
    fi
}

# ---------------------------------------------------------------------------
# Local mode (--no-build) — runs on the host, no Docker required
# ---------------------------------------------------------------------------

if [ "$NO_BUILD" = true ]; then
    echo "Running benchmark locally (--no-build, no Docker)..."
    echo "RUSTFLAGS=${RUSTFLAGS}"
    echo ""

    # Check if libvips feature can be used
    FEATURES=""
    if pkg-config --exists vips 2>/dev/null; then
        FEATURES="--features libvips"
        echo "libvips detected via pkg-config ($(pkg-config --modversion vips)) — using in-process FFI"
        echo "  (Docker path pins libvips ${LIBVIPS_PIN:-unknown}, built from source)"
    else
        echo "libvips not found — falling back to CLI comparison"
    fi

    # Point the linker at the libvips (and its glib) library directories so
    # the FFI links against the system libvips.
    LIBVIPS_LIBS="$(pkg-config --libs-only-L vips glib-2.0 2>/dev/null | sed 's/-L//g' | tr ' ' ':' || true)"
    if [ -n "$LIBVIPS_LIBS" ]; then
        export LIBRARY_PATH="${LIBVIPS_LIBS}${LIBRARY_PATH:+:$LIBRARY_PATH}"
    fi

    # Version-matrix runner: one tagged, fingerprinted snapshot appended per
    # ref. It manages its own per-tag rebuilds internally, so we only launch
    # the driver here.
    if [ "$BENCH_CMD" = "version_matrix" ]; then
        echo "Running version-matrix over: ${VERSIONS}"
        cargo run --release $FEATURES --bin version_matrix -- --versions "$VERSIONS"
        exit 0
    fi

    cargo run --release $FEATURES --bin "$BENCH_CMD"

    regenerate_charts "$SCRIPT_DIR/report"
    exit 0
fi

# ---------------------------------------------------------------------------
# Pre-flight (Docker path only)
# ---------------------------------------------------------------------------

if ! docker info >/dev/null 2>&1; then
    echo "Error: Docker is not running (needed unless --no-build)."
    exit 1
fi

# ---------------------------------------------------------------------------
# Docker build
# ---------------------------------------------------------------------------

# Stop previous container
if docker ps -a --format '{{.Names}}' | grep -q "^${CONTAINER_NAME}$"; then
    docker rm -f "$CONTAINER_NAME" >/dev/null
fi

WORKSPACE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "=== libviprs benchmark (${ARCH_LABEL}, ${MEMORY_MB} MB) ==="
echo ""
echo "Building Docker image..."
echo "  Platform:  ${PLATFORM}"
echo "  Workspace: ${WORKSPACE_DIR}"
echo "  libvips:   ${LIBVIPS_PIN:-unknown} (built from source, issue #33)"
echo "  Command:   ${BENCH_CMD}"
echo ""

DOCKER_BUILDKIT=1 docker build \
    --platform "$PLATFORM" \
    -f "$SCRIPT_DIR/Dockerfile" \
    -t "$IMAGE_NAME" \
    "$WORKSPACE_DIR"

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------

echo ""
echo "Running benchmark in container (${MEMORY_MB} MB memory limit)..."
echo ""

# Mount report/ so charts persist after the container exits
mkdir -p "$SCRIPT_DIR/report"

docker run --rm \
    --platform "$PLATFORM" \
    --name "$CONTAINER_NAME" \
    --memory="${MEMORY_MB}m" \
    -e RUSTFLAGS="$RUSTFLAGS" \
    -v "$SCRIPT_DIR/report:/src/libviprs-bench/report" \
    "$IMAGE_NAME" \
    cargo run --release --features libvips --bin "$BENCH_CMD"

# Charts are rendered on the HOST (node lives here, not in the Rust image)
# from the JSON the container wrote into the mounted report/ volume.
regenerate_charts "$SCRIPT_DIR/report"

echo ""
echo "Results written to ${SCRIPT_DIR}/report/"
echo "  Charts:  report/scalability_*.svg / report/chart_*.svg"
echo "  Data:    report/scalability_results.json / report/benchmark_results.json"
echo "  History: report/benchmark_history.json"
