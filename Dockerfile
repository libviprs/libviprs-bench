# ---------------------------------------------------------------------------
# Dockerfile — libviprs benchmark environments, one stage per family
#
# TWO targets, because the families need different machines:
#
#   --target engines   the `engines` family. Rust
#                      and the two crates and nothing else: no libvips headers,
#                      no libvips binary, no cargo features. It builds in a
#                      fraction of the time the comparison image takes, and the
#                      absence is the point — a family that measures three
#                      libviprs engines must not be able to find a fourth.
#   (default)          the `vips` comparison, with libvips compiled from a
#                      pinned upstream source tarball. Unchanged.
#
# Whichever family runs, every engine writes its tiles as PNG files to a real
# on-disk sink under the same DeepZoom layout, so neither side gets an
# in-RAM-sink or tile-codec advantage (issue #153).
#
# libvips is compiled from a pinned upstream *source* tarball (not Debian's
# frozen `libvips-dev`), so the C oracle is a recent release matched to the
# `libvips-rs` 8.18 bindings rather than a years-old ~8.14 mismatch (#33).
#
# Build:  docker build -t libviprs-bench .                        # vips comparison
#         docker build --target engines -t libviprs-bench:engines .  # libviprs only
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
#
# Availability note (#35): pinning apt to snapshot.debian.org routes every
# package fetch through a single archive host that is historically slow and can
# be briefly rate-limited or degraded. That is the deliberate cost of a frozen
# mirror; apt is set to retry and the libvips/PDFium tarball fetches use
# `curl --retry`, so a transient snapshot blip should be re-run rather than
# treated as a real failure.
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

# ---------------------------------------------------------------------------
# The `storage` stage: the libviprs-only families, no libvips at all.
#
# `storage` compares PMTiles against a directory tree, which is libviprs
# against libviprs, so nothing in it links the C oracle. Building libvips from
# source for it would add ten minutes and a hundred apt packages to a job that
# cannot use any of it, so this stage starts at the same digest-pinned Rust
# base the builder does and stops there. Same toolchain, same base image, a
# fraction of the build.
#
# It is its own stage and not a second consumer of the `engines` stage,
# because the two build different binaries: `engines` builds `scalability` and
# `report`, this builds `storage`. They share a base image and a platform, and
# collapsing them into one stage that builds all three would put a longer build
# in front of both jobs for no gain.
#
# It sits ahead of the builder stage on purpose: BuildKit skips a stage nothing
# depends on, so a plain `docker build` with no `--target` still produces the
# builder image and `run-bench.sh`'s default path is untouched.
#
#   docker build --platform linux/arm64 --target storage -t libviprs-bench:storage .
#   docker run --rm --platform linux/arm64 libviprs-bench:storage
# ---------------------------------------------------------------------------
# The Rust pin is 1.97 because libviprs declares `rust-version = "1.97"` and
# cargo refuses to compile it on the old 1.89 pin outright. Every stage in this
# file now carries the same digest, which the families lane moved at the same
# time and for the same reason.
FROM rust:1.97-bookworm@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97 AS storage

WORKDIR /src
COPY libviprs/ libviprs/
COPY libviprs-bench/ libviprs-bench/

WORKDIR /src/libviprs-bench
RUN cargo fetch

# Default features only. The `storage` family must build without `libvips`,
# and this is where that is enforced rather than asserted.
RUN cargo build --release --bin storage

CMD ["cargo", "run", "--release", "--bin", "storage", "--", \
     "--family", "storage", "--profile", "ci"]

# Stage 1: Download PDFium for the target architecture. Base image digest-pinned
# (not just tag-pinned) so the exact layer cannot shift under a rebuild (#35).
FROM debian:bookworm-20250929-slim@sha256:7e490910eea2861b9664577a96b54ce68ea3e02ce7f51d89cb0103a6f9c386e0 AS pdfium

# Pin apt to the dated Debian snapshot before installing anything, so even this
# stage's `curl` resolves from a frozen mirror (#35). See the builder stage for
# the full rationale; the deb822 source is rewritten wholesale (keeping the
# stock archive keyring) so the pin is independent of the base image's layout.
# NOTE: this deb822 snapshot block is duplicated verbatim in the builder stage
# below (the canonical copy). The builder is `rust:*-bookworm` and cannot share
# this Debian base image, so the two are kept intentionally in sync — edit both
# together when either changes.
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

# ---------------------------------------------------------------------------
# Stage 2: the `engines` family.
#
# Rust, the two crates, and deliberately nothing else. No libvips source build,
# no libvips headers, no `vips` binary, and no cargo features — the default
# build. That is what makes an `engines` run mean the same thing wherever it is
# measured: the engine set cannot quietly grow a fourth member because the image
# happened to have libvips in it.
#
# It also costs a fraction of the comparison image: no meson, no ninja, no
# multi-minute libvips compile, no codec `-dev` set, so the cheap CI cell and a
# local iteration loop both get an image in the time cargo takes.
#
# This stage sits BEFORE the builder stage on purpose: the last stage in the
# file is what a bare `docker build .` targets, and that has to stay the
# comparison image the README documents.
# ---------------------------------------------------------------------------
FROM rust:1.97-bookworm@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97 AS engines

ENV CARGO_TERM_COLOR=always
WORKDIR /src

COPY libviprs/ libviprs/
COPY libviprs-bench/ libviprs-bench/

WORKDIR /src/libviprs-bench
RUN cargo fetch
# Default features. If this line ever needs a `--features`, the family split has
# been undone.
RUN cargo build --release --bin scalability --bin report

CMD ["cargo", "run", "--release", "--bin", "scalability", "--", "--family", "engines"]

# ---------------------------------------------------------------------------
# Stage 3: the `vips` comparison family.
#
# Pinned Rust (was `rust:latest`) so the compiler and its bundled toolchain
# do not drift between benchmark runs (issue #153); digest-pinned (not just
# tag-pinned) so the exact base layer cannot shift under a rebuild (#35).
#
# The pin moved 1.89 -> 1.97 because the measured core declares
# `rust-version = "1.97"` as of libviprs 0.4.0, so the old pin could not compile
# the thing it exists to measure — `cargo` refused the build outright before any
# benchmark ran. Bumping a measurement pin does invalidate cross-run comparison,
# which is the deliberate cost of the core moving its floor.
# ---------------------------------------------------------------------------
FROM rust:1.97-bookworm@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97 AS builder

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

# Default: run the scalability benchmark over the comparison family
CMD ["cargo", "run", "--release", "--features", "libvips", "--bin", "scalability", "--", "--family", "vips"]
