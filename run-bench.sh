#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# run-bench.sh — Build and run libviprs benchmarks in Docker
#
# Provides a controlled environment where libvips (C) and libviprs (Rust)
# run side-by-side with identical inputs. Both libraries are linked into
# the same process — no CLI spawning, no filesystem I/O advantage.
#
# Usage:
#   ./run-bench.sh                     # scalability benchmark (default)
#   ./run-bench.sh report              # full comparison report
#   ./run-bench.sh --arch arm          # force arm64 build
#   ./run-bench.sh --memory 4096       # container memory limit in MB
#   ./run-bench.sh --no-build          # run locally (requires libvips-dev)
#
# Output is written to report/ (charts, JSON, text tables).
# ---------------------------------------------------------------------------

ARCH=""
NO_BUILD=false
MEMORY_MB=""
BENCH_CMD="scalability"

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
        report|scalability)
            BENCH_CMD="$1"
            ;;
        *)
            echo "Unknown argument: $1"
            echo "Usage: $0 [scalability|report] [--arch arm|amd64] [--memory MB] [--no-build]"
            exit 1
            ;;
    esac
    shift
done

MEMORY_MB="${MEMORY_MB:-4096}"

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

# ---------------------------------------------------------------------------
# Pre-flight
# ---------------------------------------------------------------------------

if ! docker info >/dev/null 2>&1; then
    echo "Error: Docker is not running."
    exit 1
fi

# ---------------------------------------------------------------------------
# Local mode (--no-build)
# ---------------------------------------------------------------------------

if [ "$NO_BUILD" = true ]; then
    echo "Running benchmark locally (--no-build)..."
    echo ""

    # Check if libvips feature can be used
    FEATURES=""
    if pkg-config --exists vips 2>/dev/null; then
        FEATURES="--features libvips"
        echo "libvips detected via pkg-config — using in-process FFI"
    else
        echo "libvips not found — falling back to CLI comparison"
    fi

    LIBRARY_PATH="$(pkg-config --libs-only-L vips 2>/dev/null | sed 's/-L//g' || true)"
    export LIBRARY_PATH

    cargo run --release $FEATURES --bin "$BENCH_CMD"
    exit 0
fi

# ---------------------------------------------------------------------------
# Docker build
# ---------------------------------------------------------------------------

# Stop previous container
if docker ps -a --format '{{.Names}}' | grep -q "^${CONTAINER_NAME}$"; then
    docker rm -f "$CONTAINER_NAME" >/dev/null
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "=== libviprs benchmark (${ARCH_LABEL}, ${MEMORY_MB} MB) ==="
echo ""
echo "Building Docker image..."
echo "  Platform:  ${PLATFORM}"
echo "  Workspace: ${WORKSPACE_DIR}"
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
    -v "$SCRIPT_DIR/report:/src/libviprs-bench/report" \
    "$IMAGE_NAME" \
    cargo run --release --features libvips --bin "$BENCH_CMD"

echo ""
echo "Results written to ${SCRIPT_DIR}/report/"
echo "  Charts:  report/scalability_*.svg / report/chart_*.svg"
echo "  Data:    report/scalability_results.json / report/benchmark_results.json"
echo "  History: report/benchmark_history.json"
