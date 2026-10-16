//! #124: the tile codec is a parameter of the pyramid writer, and PNG stays the
//! default that the engines and vips families share (#153, `encoding_claim.rs`).
//!
//! Every test names the wrong implementation it goes red against.

use std::path::{Path, PathBuf};

use libviprs::{EngineKind, Layout, PyramidPlanner, TileFormat};
use libviprs_bench::{
    BENCH_TILE_FORMAT, BENCH_TILE_SUFFIX, TileCodec, gradient_raster, write_libviprs_pyramid,
    write_libviprs_pyramid_as,
};

fn tiles(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for level in std::fs::read_dir(root).unwrap().flatten() {
        if level.path().is_dir() {
            for t in std::fs::read_dir(level.path()).unwrap().flatten() {
                out.push(t.path());
            }
        }
    }
    out.sort();
    out
}

fn build(codec: Option<TileCodec>, tag: &str) -> PathBuf {
    let raster = gradient_raster(512, 512);
    let plan = PyramidPlanner::new(512, 512, 256, 0, Layout::DeepZoom)
        .unwrap()
        .plan();
    let dir = std::env::temp_dir().join(format!("tile_codec_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    match codec {
        Some(c) => write_libviprs_pyramid_as(
            &raster,
            &plan,
            EngineKind::Monolithic,
            0,
            1_000_000,
            &dir,
            c,
        ),
        None => write_libviprs_pyramid(&raster, &plan, EngineKind::Monolithic, 0, 1_000_000, &dir),
    }
    .unwrap()
}

// Goes red against: a writer that ignores the codec and always writes PNG.
#[test]
fn webp_writes_webp_files() {
    let root = build(Some(TileCodec::WEBP_LOSSLESS), "webp");
    let files = tiles(&root);
    assert!(!files.is_empty());
    for f in &files {
        assert_eq!(
            f.extension().and_then(|e| e.to_str()),
            Some("webp"),
            "{f:?}"
        );
        let bytes = std::fs::read(f).unwrap();
        assert_eq!(&bytes[..4], b"RIFF", "{f:?} is not a RIFF container");
        assert_eq!(&bytes[8..12], b"WEBP", "{f:?}");
    }
}

// Goes red against: a codec whose suffix and format can disagree, which is how
// a file called .jpg ends up holding PNG bytes.
#[test]
fn jpeg_writes_jpeg_files() {
    let root = build(Some(TileCodec::jpeg(85)), "jpeg");
    for f in tiles(&root) {
        assert_eq!(
            f.extension().and_then(|e| e.to_str()),
            Some("jpeg"),
            "{f:?}"
        );
        assert_eq!(&std::fs::read(&f).unwrap()[..2], &[0xFF, 0xD8], "{f:?}");
    }
}

// Goes red against: a default that drifted. The engines and vips families still
// write PNG through the unparameterised function.
#[test]
fn the_default_writer_is_still_png() {
    let root = build(None, "default");
    for f in tiles(&root) {
        assert_eq!(f.extension().and_then(|e| e.to_str()), Some("png"));
    }
    assert_eq!(TileCodec::BENCH.format, BENCH_TILE_FORMAT);
    assert_eq!(TileCodec::BENCH.suffix, BENCH_TILE_SUFFIX);
}

// Goes red against: PNG passed explicitly writing something else than the
// default does, i.e. the parameterised path diverging from the shared one.
#[test]
fn explicit_png_matches_the_default_byte_for_byte() {
    let a = build(None, "a");
    let b = build(Some(TileCodec::PNG), "b");
    let (fa, fb) = (tiles(&a), tiles(&b));
    assert_eq!(fa.len(), fb.len());
    for (x, y) in fa.iter().zip(&fb) {
        assert_eq!(std::fs::read(x).unwrap(), std::fs::read(y).unwrap());
    }
}

// Goes red against: a suffix typed by hand next to the format. The suffix is
// derived from the format's own extension, so the two cannot drift.
#[test]
fn suffix_is_the_formats_own_extension() {
    for f in [
        TileFormat::Png,
        TileFormat::Jpeg { quality: 85 },
        TileFormat::Webp,
    ] {
        assert_eq!(TileCodec::new(f).suffix, format!(".{}", f.extension()));
    }
}
