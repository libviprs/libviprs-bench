//! Tests for the libvips FFI benchmark path (#152).
//!
//! Two concerns are covered:
//!
//! 1. The libvips application handle must not be stored in an aliased
//!    mutable global (`static mut`). That pattern is UB-prone (the
//!    borrow checker cannot see the aliasing) and trips `static_mut_refs`
//!    under stricter toolchains, silently rotting the `libvips` feature
//!    because the crate has no CI. It must be a `std::sync::OnceLock`.
//!
//! 2. The no-copy `VipsImage` created from a `&Raster` buffer must not be
//!    able to outlive the borrow it aliases. The FFI wrapper must tie the
//!    image handle's lifetime to the borrowed raster.

/// The libvips app global must not be declared `static mut`.
///
/// Fails on the pre-fix code (`static mut APP: Option<VipsApp>`), passes
/// once it is replaced by a `OnceLock`.
#[test]
fn vips_app_global_is_not_static_mut() {
    let source = include_str!("../src/lib.rs");
    let declares_static_mut = source
        .lines()
        .any(|line| line.trim_start().starts_with("static mut "));
    assert!(
        !declares_static_mut,
        "src/lib.rs still declares a `static mut` global; use \
         `std::sync::OnceLock` for the libvips app handle (see #152)"
    );
}

/// End-to-end exercise of the in-process libvips FFI path: build a small
/// raster, wrap it (no-copy) through the RAII image guard, run `dzsave`,
/// and confirm tiles come out. Validates that the `OnceLock` init and the
/// lifetime-bound image handle keep the path working after the refactor.
#[cfg(feature = "libvips")]
#[test]
fn vips_inprocess_produces_tiles() {
    let raster = libviprs_bench::gradient_raster(512, 512);
    let metrics = libviprs_bench::bench_libvips_inprocess(&raster, 256, 1, "test")
        .expect("libvips in-process bench returned None");
    assert_eq!(metrics.engine, "libvips");
    assert!(
        metrics.tiles_produced > 0,
        "expected dzsave to produce tiles, got {}",
        metrics.tiles_produced
    );
}
