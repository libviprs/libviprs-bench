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

/// Cross-engine fairness guard (#153): the shared bench functions must not
/// give the libviprs engines an in-RAM sink advantage, and both sides must
/// encode the same tile codec.
///
/// This is a cheap source-level CI check that fails the moment either half of
/// the fairness fix regresses — without needing libvips linked in.
#[test]
fn cross_engine_comparison_is_structurally_fair() {
    let source = include_str!("../src/lib.rs");

    // 1. Sink medium: the libviprs engine bench functions must write real
    //    tiles through `FsSink`, not collect them in a `MemorySink`. The old
    //    code let libviprs skip all filesystem I/O that libvips paid.
    assert!(
        source.contains("FsSink::new") || source.contains("engine_fs_sink"),
        "libviprs engine benches must write to an on-disk FsSink (see #153)"
    );
    assert!(
        !source.contains("MemorySink"),
        "src/lib.rs still uses a MemorySink for the cross-engine benches — \
         that hands the libviprs engines an in-RAM sink advantage libvips \
         never gets (see #153)"
    );

    // 2. Codec: both the in-process and CLI libvips paths, and the libviprs
    //    FsSink, must encode the same format. The in-process path used to
    //    write `.raw` tiles while the CLI wrote `.png`, both reported as
    //    engine "libvips".
    assert!(
        !source.contains("\".raw\""),
        "src/lib.rs still writes `.raw` tiles somewhere — the libvips \
         in-process path must use the same codec as everyone else (see #153)"
    );
    assert!(
        source.contains("BENCH_TILE_SUFFIX") && source.contains("BENCH_TILE_FORMAT"),
        "both engines must route through the shared BENCH_TILE_* codec \
         constants so the tile format is not a hidden variable (see #153)"
    );
}

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

/// End-to-end fairness (#153): a libviprs engine and libvips run on the same
/// raster and DeepZoom plan must both produce tiles (both now writing PNG
/// tiles to a real filesystem sink), and the two memory bases must be
/// reported in their own labelled fields — never conflated.
#[cfg(feature = "libvips")]
#[test]
fn engines_share_sink_and_report_separate_memory_columns() {
    let raster = libviprs_bench::gradient_raster(512, 512);
    let planner =
        libviprs::PyramidPlanner::new(512, 512, 256, 0, libviprs::Layout::DeepZoom).unwrap();
    let plan = planner.plan();

    let mono = libviprs_bench::bench_monolithic(&raster, &plan, 1, "fair_mono");
    let vips = libviprs_bench::bench_libvips_inprocess(&raster, 256, 1, "fair_vips")
        .expect("libvips in-process bench returned None");

    // Both engines actually produce a pyramid on disk under identical input.
    assert!(mono.tiles_produced > 0, "monolithic produced no tiles");
    assert!(vips.tiles_produced > 0, "libvips produced no tiles");

    // Memory columns are separate and correctly attributed:
    //  - the libviprs engine exposes an engine-tracked working set,
    //  - libvips exposes none (tracked == 0) and only an RSS figure.
    assert!(
        mono.tracked_memory_bytes > 0,
        "libviprs engine should report a non-zero tracked working set"
    );
    assert_eq!(
        vips.tracked_memory_bytes, 0,
        "libvips has no engine-tracked counter; its tracked column must be 0 \
         rather than borrowing an unrelated basis (see #153)"
    );
    assert!(
        vips.peak_rss_bytes > 0,
        "libvips must report a peak RSS figure"
    );

    // The codec constant both sides encode is PNG.
    assert_eq!(libviprs_bench::BENCH_TILE_FORMAT, libviprs::TileFormat::Png);
    assert_eq!(libviprs_bench::BENCH_TILE_SUFFIX, ".png");
}
