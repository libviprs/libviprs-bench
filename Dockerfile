# ---------------------------------------------------------------------------
# Dockerfile — libviprs benchmark environment with libvips + PDFium
#
# Provides a controlled, fully-pinned environment where libvips (C) and
# libviprs (Rust) run side-by-side with identical inputs: both write PNG tiles
# to a real on-disk sink with the same codec, so neither side gets a
# filesystem-I/O or encoding advantage (issue #153).
#
# Build:  docker build -t libviprs-bench .
# Run:    docker run --rm libviprs-bench
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# Pinned inputs. A benchmark is only reproducible if every layer is fixed:
# a floating base image, an unpinned `libvips-dev`, or a `latest` PDFium
# would silently change the numbers between runs (issue #153). Bump these
# deliberately, never implicitly.
#   PDFIUM_RELEASE  : libviprs-dep release tag (checksum-verified builder)
#   LIBVIPS_VERSION — exact Debian bookworm `libvips*` package version
# The Rust and Debian base images are pinned by tag on the FROM lines below.
# ---------------------------------------------------------------------------
ARG PDFIUM_RELEASE=pdfium-7881
# Exact `libvips*` version currently in Debian bookworm. Debian only keeps the
# newest security build of a package on the live mirror, so this must name the
# current `8.14.1-3+deb12uN` — bump `N` when a new bookworm security update
# lands (otherwise `apt-get install` cannot resolve the pinned version).
ARG LIBVIPS_VERSION=8.14.1-3+deb12u3

# Stage 1: Download PDFium for the target architecture
FROM debian:bookworm-20250929-slim AS pdfium

RUN apt-get update && apt-get install -y curl && rm -rf /var/lib/apt/lists/*

ARG TARGETARCH
ARG PDFIUM_RELEASE
# PDFium provenance (libviprs/libviprs#156): consume the pinned,
# checksum-verified binaries published by libviprs-dep (the branch-pinned
# builder that runs real ABI/symbol verification), the same source
# libviprs-tests consumes. Keep PDFIUM_RELEASE and the per-arch SHA-256
# digests in lockstep with libviprs-tests. The libviprs-dep tarball nests
# its contents under a `pdfium-<arch>/` top directory, hence
# `--strip-components=1`.
RUN case "${TARGETARCH}" in \
        amd64) PDFIUM_ARCH="linux-x64";   PDFIUM_SHA256="653f24f074afe6c868f634ae0cc954a1a89821f33bc7795f16065a14022b662b" ;; \
        arm64) PDFIUM_ARCH="linux-arm64"; PDFIUM_SHA256="3a8940ae414a54601f6bc0b25fb3d589025320ee91fff378e12708259da5702d" ;; \
        *)     echo "Unsupported arch: ${TARGETARCH}" && exit 1 ;; \
    esac && \
    curl -fL -o /tmp/pdfium.tgz \
        "https://github.com/libviprs/libviprs-dep/releases/download/${PDFIUM_RELEASE}/pdfium-${PDFIUM_ARCH}.tgz" && \
    echo "${PDFIUM_SHA256}  /tmp/pdfium.tgz" | sha256sum -c - && \
    mkdir -p /opt/pdfium && \
    tar xzf /tmp/pdfium.tgz -C /opt/pdfium --strip-components=1 && \
    rm /tmp/pdfium.tgz

# Stage 2: Build and run benchmarks
# Pinned Rust (was `rust:latest`) so the compiler and its bundled toolchain
# do not drift between benchmark runs (issue #153).
FROM rust:1.89-bookworm AS builder

ARG LIBVIPS_VERSION

# Install libvips development headers and runtime (C library for comparison)
# plus pkg-config for the build script to find it. `libvips*` are pinned to an
# exact Debian version so the C baseline is fixed run-to-run; bump
# LIBVIPS_VERSION deliberately when refreshing the environment.
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        "libvips-dev=${LIBVIPS_VERSION}" \
        "libvips-tools=${LIBVIPS_VERSION}" \
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
