# ---------------------------------------------------------------------------
# Dockerfile — libviprs benchmark environment with libvips + PDFium
#
# Provides a controlled, fully-pinned environment where libvips (C) and
# libviprs (Rust) run side-by-side with identical inputs: both write PNG tiles
# to a real on-disk sink with the same codec, so neither side gets a
# filesystem-I/O or encoding advantage (issue #153).
#
# libvips is compiled from a pinned upstream *source* tarball (not Debian's
# frozen `libvips-dev`), so the C oracle is a recent release matched to the
# `libvips-rs` 8.18 bindings rather than a years-old ~8.14 mismatch (#33).
#
# Build:  docker build -t libviprs-bench .
# Run:    docker run --rm libviprs-bench
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# Pinned inputs. A benchmark is only reproducible if every layer is fixed:
# a floating base image, an unpinned libvips, or a `latest` PDFium would
# silently change the numbers between runs (issue #153). Bump these
# deliberately, never implicitly.
#   PDFIUM_RELEASE  — libviprs-dep release tag (checksum-verified builder)
#   LIBVIPS_VERSION — upstream libvips source release, built from tarball
#   LIBVIPS_SHA256  — SHA-256 of that tarball, verified before it is built
#   DEBIAN_SNAPSHOT — snapshot.debian.org timestamp the apt toolchain resolves
#                     against, so the codec `-dev` libraries (libpng et al. —
#                     the DeepZoom PNG hot path) are fixed too, not floated on
#                     the live bookworm mirror (#35, closing the #33 scope gap)
# The Rust and Debian base images are digest-pinned (`@sha256:`) on the FROM
# lines below, so no layer floats on a mutable tag either (#35).
# ---------------------------------------------------------------------------
ARG PDFIUM_RELEASE=pdfium-7881
# Upstream libvips release compiled from source (issue #33). Kept in lockstep
# with `provenance::PINNED_LIBVIPS_VERSION` and the `libvips-rs` binding in
# Cargo.toml — `tests/libvips_provenance.rs` fails if they drift. Bump all
# three together, refreshing LIBVIPS_SHA256 from the upstream
# `vips-<version>.tar.xz.sha256sum` companion file.
ARG LIBVIPS_VERSION=8.18.4
ARG LIBVIPS_SHA256=2677bad6c422617fd1172d359c16af34e736965d042c214203a87187d26ff037
# Dated snapshot.debian.org mirror the apt toolchain resolves against (#35).
# Matches the Debian base image's own build date (bookworm-20250929) so the
# whole image is one frozen point in time. snapshot.debian.org resolves a
# timestamp to the nearest snapshot at or before it, so a dated value always
# lands on a real snapshot.
ARG DEBIAN_SNAPSHOT=20250929T000000Z

# Stage 1: Download PDFium for the target architecture. Base image digest-pinned
# (not just tag-pinned) so the exact layer cannot shift under a rebuild (#35).
FROM debian:bookworm-20250929-slim@sha256:7e490910eea2861b9664577a96b54ce68ea3e02ce7f51d89cb0103a6f9c386e0 AS pdfium

# Pin apt to the dated Debian snapshot before installing anything, so even this
# stage's `curl` resolves from a frozen mirror (#35). See the builder stage for
# the full rationale; the deb822 source is rewritten wholesale (keeping the
# stock archive keyring) so the pin is independent of the base image's layout.
ARG DEBIAN_SNAPSHOT
RUN printf 'Types: deb\nURIs: http://snapshot.debian.org/archive/debian/%s\nSuites: bookworm bookworm-updates\nComponents: main\nSigned-By: /usr/share/keyrings/debian-archive-keyring.gpg\n\nTypes: deb\nURIs: http://snapshot.debian.org/archive/debian-security/%s\nSuites: bookworm-security\nComponents: main\nSigned-By: /usr/share/keyrings/debian-archive-keyring.gpg\n' \
        "${DEBIAN_SNAPSHOT}" "${DEBIAN_SNAPSHOT}" > /etc/apt/sources.list.d/debian.sources && \
    rm -f /etc/apt/sources.list && \
    printf 'Acquire::Check-Valid-Until "false";\nAcquire::Retries "3";\n' > /etc/apt/apt.conf.d/10snapshot

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
    curl -fL --retry 3 --retry-delay 2 --retry-connrefused -o /tmp/pdfium.tgz \
        "https://github.com/libviprs/libviprs-dep/releases/download/${PDFIUM_RELEASE}/pdfium-${PDFIUM_ARCH}.tgz" && \
    echo "${PDFIUM_SHA256}  /tmp/pdfium.tgz" | sha256sum -c - && \
    mkdir -p /opt/pdfium && \
    tar xzf /tmp/pdfium.tgz -C /opt/pdfium --strip-components=1 && \
    rm /tmp/pdfium.tgz

# Stage 2: Build and run benchmarks
# Pinned Rust (was `rust:latest`) so the compiler and its bundled toolchain
# do not drift between benchmark runs (issue #153); digest-pinned (not just
# tag-pinned) so the exact base layer cannot shift under a rebuild (#35).
FROM rust:1.89-bookworm@sha256:948f9b08a66e7fe01b03a98ef1c7568292e07ec2e4fe90d88c07bb14563c84ff AS builder

ARG LIBVIPS_VERSION
ARG LIBVIPS_SHA256

# Build libvips from a pinned upstream source tarball rather than installing
# Debian's frozen `libvips-dev` (issue #33): bookworm ships ~8.14, years
# behind the `libvips-rs` 8.18 bindings, so the apt package made the C oracle
# an unfair, mismatched baseline. A source build gives a recent release
# matched to the bindings, fixed by version + SHA-256.
#
# Two dependency sets: the meson/ninja toolchain that compiles libvips, and
# the image-format `-dev` libraries it links against. Only PNG is on the
# benchmark's hot path (DeepZoom writes PNG tiles), but jpeg/tiff/webp are
# included so the oracle is a realistic, full-featured libvips build.
#
# Reproducibility scope (#35, closing the #33 gap): the apt toolchain is pinned
# to the dated snapshot.debian.org mirror set up just below, so these `-dev`
# packages resolve to fixed versions instead of bookworm's live mirror. Without
# it an intra-bookworm point release (e.g. a libpng security update) could shift
# the encode hot path under a rebuild — libpng directly shapes the measured
# DeepZoom PNG-tile numbers. The meson force-enable further hard-fails the build
# if a codec ever disappears from the snapshot entirely.
#
# The deb822 source is rewritten wholesale to the snapshot (keeping the stock
# `debian-archive-keyring` so releases stay GPG-verified), and
# `Check-Valid-Until false` is required because a dated snapshot's Release file
# carries an expired `Valid-Until`.
ARG DEBIAN_SNAPSHOT
RUN printf 'Types: deb\nURIs: http://snapshot.debian.org/archive/debian/%s\nSuites: bookworm bookworm-updates\nComponents: main\nSigned-By: /usr/share/keyrings/debian-archive-keyring.gpg\n\nTypes: deb\nURIs: http://snapshot.debian.org/archive/debian-security/%s\nSuites: bookworm-security\nComponents: main\nSigned-By: /usr/share/keyrings/debian-archive-keyring.gpg\n' \
        "${DEBIAN_SNAPSHOT}" "${DEBIAN_SNAPSHOT}" > /etc/apt/sources.list.d/debian.sources && \
    rm -f /etc/apt/sources.list && \
    printf 'Acquire::Check-Valid-Until "false";\nAcquire::Retries "3";\n' > /etc/apt/apt.conf.d/10snapshot

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        xz-utils \
        build-essential \
        meson \
        ninja-build \
        pkg-config \
        libglib2.0-dev \
        libexpat1-dev \
        libpng-dev \
        libjpeg62-turbo-dev \
        libtiff-dev \
        libwebp-dev \
        time \
    && rm -rf /var/lib/apt/lists/*

# Download, checksum-verify, and compile the pinned libvips release. The
# tarball is verified against LIBVIPS_SHA256 before it is unpacked (a pinned
# URL without a digest still trusts the remote end forever — the same rule the
# PDFium stage follows), then built release-mode into /usr/local. `--libdir=lib`
# keeps `vips.pc` under /usr/local/lib/pkgconfig where pkg-config finds it
# without a multiarch subdir. The codec `-dev` libraries are force-enabled
# (`-Dpng/jpeg/tiff/webp=enabled`) rather than left to meson's `auto`
# detection, so a missing or broken codec lib hard-fails the build instead of
# silently producing a libvips without it — a PNG-less oracle would quietly
# invalidate the DeepZoom PNG-tile hot path (issue #33). `curl --retry`
# absorbs a transient network blip on the now-multi-minute build.
RUN curl -fL --retry 3 --retry-delay 2 --retry-connrefused -o /tmp/vips.tar.xz \
        "https://github.com/libvips/libvips/releases/download/v${LIBVIPS_VERSION}/vips-${LIBVIPS_VERSION}.tar.xz" && \
    echo "${LIBVIPS_SHA256}  /tmp/vips.tar.xz" | sha256sum -c - && \
    mkdir -p /tmp/vips-src && \
    tar xJf /tmp/vips.tar.xz -C /tmp/vips-src --strip-components=1 && \
    cd /tmp/vips-src && \
    meson setup build --buildtype=release --prefix=/usr/local --libdir=lib \
        -Dpng=enabled -Djpeg=enabled -Dtiff=enabled -Dwebp=enabled && \
    ninja -C build && \
    ninja -C build install && \
    ldconfig && \
    rm -rf /tmp/vips.tar.xz /tmp/vips-src

# Let the build script's pkg-config probe find the freshly built libvips.
ENV PKG_CONFIG_PATH=/usr/local/lib/pkgconfig

# Install PDFium shared library
COPY --from=pdfium /opt/pdfium/lib/libpdfium.so /usr/local/lib/libpdfium.so
RUN ldconfig

# Verify the built libvips is *exactly* the pinned version and is discoverable
# by pkg-config. Comparing the modversion against ${LIBVIPS_VERSION} (not just
# printing it) fails the build if a stray or wrong-version libvips is ahead on
# PATH / in the pkg-config path, rather than silently benchmarking it (#33).
RUN vips --version && \
    modversion="$(pkg-config --modversion vips)" && \
    if [ "$modversion" != "${LIBVIPS_VERSION}" ]; then \
        echo "built libvips modversion ${modversion} != pinned ${LIBVIPS_VERSION}" >&2; \
        exit 1; \
    fi

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
