# ---------------------------------------------------------------------------
# Dockerfile — libviprs benchmark environment with libvips + PDFium
#
# Provides a controlled environment where libvips (C) and libviprs (Rust)
# run side-by-side with identical inputs, no filesystem I/O advantage for
# either side. Both use in-memory pipelines for fair comparison.
#
# Build:  docker build -t libviprs-bench .
# Run:    docker run --rm libviprs-bench
# ---------------------------------------------------------------------------

# Stage 1: Download PDFium for the target architecture
FROM debian:bookworm-slim AS pdfium

RUN apt-get update && apt-get install -y curl && rm -rf /var/lib/apt/lists/*

ARG TARGETARCH
RUN case "${TARGETARCH}" in \
        amd64) PDFIUM_ARCH="linux-x64" ;; \
        arm64) PDFIUM_ARCH="linux-arm64" ;; \
        *)     echo "Unsupported arch: ${TARGETARCH}" && exit 1 ;; \
    esac && \
    curl -L -o /tmp/pdfium.tgz \
        "https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-${PDFIUM_ARCH}.tgz" && \
    mkdir -p /opt/pdfium && \
    tar xzf /tmp/pdfium.tgz -C /opt/pdfium && \
    rm /tmp/pdfium.tgz

# Stage 2: Build and run benchmarks
FROM rust:latest AS builder

# Install libvips development headers and runtime (C library for comparison)
# plus pkg-config for the build script to find it
RUN apt-get update && \
    apt-get install -y \
        ca-certificates \
        libvips-dev \
        libvips-tools \
        pkg-config \
        time \
    && rm -rf /var/lib/apt/lists/*

# Install PDFium shared library
COPY --from=pdfium /opt/pdfium/lib/libpdfium.so /usr/local/lib/libpdfium.so
RUN ldconfig

# Verify libvips is installed and accessible
RUN vips --version && pkg-config --libs vips

WORKDIR /src

# Copy crates
COPY libviprs/ libviprs/
COPY libviprs-bench/ libviprs-bench/

# Fetch dependencies
WORKDIR /src/libviprs
RUN cargo fetch

WORKDIR /src/libviprs-bench
RUN cargo fetch

# Build in release mode with libvips FFI feature for in-process comparison
RUN cargo build --release --features libvips --bin scalability --bin report

# Default: run the scalability benchmark
CMD ["cargo", "run", "--release", "--features", "libvips", "--bin", "scalability"]
