//! PDFium provenance guard (#156, filed in the core repo).
//!
//! The Docker harness must consume the pinned, checksum-verified PDFium
//! binaries published by `libviprs-dep` (the branch-pinned builder that runs
//! real ABI/symbol verification), matching what `libviprs-tests` already
//! does. It must not fetch from the upstream `bblanchon/pdfium-binaries`
//! channel at all: even a tag-pinned upstream download carries no checksum
//! and is a different provenance source from the one the test suite
//! certifies.
//!
//! These are cheap source-level CI checks in the style of
//! `tests/vips_ffi.rs`: they fail the moment the Dockerfile's fetch step
//! drifts off the verified source, without needing Docker in the loop.

const DOCKERFILE: &str = include_str!("../Dockerfile");

/// The pdfium stage must download from libviprs-dep's release, not from
/// bblanchon/pdfium-binaries (neither `latest` nor a pinned tag).
#[test]
fn dockerfile_fetches_pdfium_from_libviprs_dep() {
    assert!(
        !DOCKERFILE.contains("bblanchon"),
        "Dockerfile still references bblanchon/pdfium-binaries; the bench \
         image must consume the verified libviprs-dep release (see \
         libviprs/libviprs#156)"
    );
    assert!(
        DOCKERFILE.contains("github.com/libviprs/libviprs-dep/releases/download"),
        "Dockerfile must download PDFium from the libviprs-dep release \
         (see libviprs/libviprs#156)"
    );
    assert!(
        DOCKERFILE.contains("PDFIUM_RELEASE=pdfium-7881"),
        "Dockerfile must pin the libviprs-dep `pdfium-7881` release tag, in \
         lockstep with libviprs-tests (see libviprs/libviprs#156)"
    );
}

/// The download must be integrity-checked before extraction: a pinned URL
/// without a digest still trusts the remote end forever.
#[test]
fn dockerfile_verifies_pdfium_checksum() {
    assert!(
        DOCKERFILE.contains("sha256sum -c"),
        "Dockerfile must verify the PDFium tarball against a pinned SHA-256 \
         digest with `sha256sum -c` (see libviprs/libviprs#156)"
    );
    // Per-arch digests of the live pdfium-7881 assets; these are the same
    // values libviprs-tests pins. If libviprs-dep republishes the release,
    // update both repos together.
    let digests = [
        // pdfium-linux-x64.tgz
        "653f24f074afe6c868f634ae0cc954a1a89821f33bc7795f16065a14022b662b",
        // pdfium-linux-arm64.tgz
        "3a8940ae414a54601f6bc0b25fb3d589025320ee91fff378e12708259da5702d",
    ];
    for digest in digests {
        assert!(
            DOCKERFILE.contains(digest),
            "Dockerfile is missing the pinned SHA-256 digest {digest} for a \
             pdfium-7881 architecture (see libviprs/libviprs#156)"
        );
    }
}

/// libviprs-dep tarballs nest their contents under a `pdfium-<arch>/` top
/// directory (unlike the flat upstream layout), so extraction must strip one
/// path component for `/opt/pdfium/lib/libpdfium.so` to land where the
/// builder stage copies it from.
#[test]
fn dockerfile_strips_nested_tarball_layout() {
    assert!(
        DOCKERFILE.contains("--strip-components=1"),
        "Dockerfile must extract the libviprs-dep tarball with \
         `--strip-components=1`; its contents are nested one directory deep \
         (see libviprs/libviprs#156)"
    );
    assert!(
        DOCKERFILE.contains("/opt/pdfium/lib/libpdfium.so"),
        "builder stage must copy libpdfium.so from the pdfium stage's \
         /opt/pdfium/lib path"
    );
}
