//! What the benchmark actually does with a tile, and whether the prose agrees
//! (issue #64).
//!
//! The README said every engine writes PNG tiles to a real on-disk sink. The
//! benchmark article on libviprs.org said libviprs writes to a `MemorySink` and
//! "Neither side encodes to PNG or JPEG". Both cannot be true, nothing checked
//! either, and the two have been contradicting each other since issue #153 put
//! both sides on the same codec and left the article behind.
//!
//! These tests settle it from the code and then hold every document this
//! repository ships to the answer.

use std::path::{Path, PathBuf};

use libviprs::{EngineKind, Layout, PyramidPlanner};
use libviprs_bench::{
    BENCH_TILE_FORMAT, BENCH_TILE_SUFFIX, TILE_ENCODING_CLAIM, TILE_ENCODING_CONTRADICTIONS,
    gradient_raster, write_libviprs_pyramid,
};

/// The first eight bytes of every PNG file, by the format's own spec. Reading
/// the magic rather than trusting the `.png` name is the difference between
/// "a file called a tile" and "an encoded tile".
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("libviprs-bench-encoding-claim")
        .join(format!("{}_{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

fn tile_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(levels) = std::fs::read_dir(root) else {
        return out;
    };
    for level in levels.flatten() {
        if !level.path().is_dir() {
            continue;
        }
        if let Ok(tiles) = std::fs::read_dir(level.path()) {
            for tile in tiles.flatten() {
                if tile.path().is_file() {
                    out.push(tile.path());
                }
            }
        }
    }
    out
}

/// RED against the story the site article tells: swap the engines' [`FsSink`]
/// for a `MemorySink`, or move [`BENCH_TILE_FORMAT`] off PNG, and this fails —
/// there are no tile files to find, or the files that are there do not start
/// with the PNG signature. It is the executable half of the claim; the prose
/// half is below.
///
/// [`FsSink`]: libviprs::FsSink
#[test]
fn every_engine_writes_encoded_png_tiles_to_a_real_on_disk_sink() {
    assert_eq!(
        BENCH_TILE_SUFFIX, ".png",
        "the codec constant and the claim have to agree before anything else can"
    );
    assert_eq!(format!("{BENCH_TILE_FORMAT:?}").to_lowercase(), "png");

    let src = gradient_raster(512, 384);
    let planner = PyramidPlanner::new(512, 384, 256, 0, Layout::DeepZoom).expect("plan");
    let plan = planner.plan();

    for (kind, name) in [
        (EngineKind::Monolithic, "monolithic"),
        (EngineKind::Streaming, "streaming"),
        (EngineKind::MapReduce, "mapreduce"),
    ] {
        let dir = scratch(name);
        let tiles_root = write_libviprs_pyramid(&src, &plan, kind, 0, 1_000_000, &dir)
            .unwrap_or_else(|e| panic!("{name} engine could not write its pyramid: {e}"));
        let tiles = tile_files(&tiles_root);
        assert!(
            !tiles.is_empty(),
            "the {name} engine produced no tile FILES under {}: it is not writing to a real \
             on-disk sink, which is what the site article claims and the README denies",
            tiles_root.display()
        );
        for tile in &tiles {
            assert_eq!(
                tile.extension().and_then(|e| e.to_str()),
                Some("png"),
                "{tile:?} is not a .png"
            );
            let bytes = std::fs::read(tile).expect("read a tile");
            assert!(
                bytes.len() > PNG_MAGIC.len() && bytes[..PNG_MAGIC.len()] == PNG_MAGIC,
                "{tile:?} does not start with the PNG signature, so the {name} engine wrote \
                 something other than an encoded PNG"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Every document this repository ships that describes the measurement path.
/// The claim has one source ([`TILE_ENCODING_CLAIM`]) and these repeat it
/// verbatim, so there is no second place for the prose to drift to.
const DOCUMENTS: &[&str] = &["README.md", "run-bench.sh", "Dockerfile"];

fn read_doc(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    normalize(&text)
}

/// Flatten a document to one line of words so a claim can be matched across a
/// line wrap, a comment marker, or a Rust string continuation. The claim is a
/// sentence, and a sentence that only matches when nobody reflows the paragraph
/// is a guard that fails for the wrong reason.
fn normalize(text: &str) -> String {
    let mut joined = String::new();
    for line in text.lines() {
        let mut line = line.trim();
        for marker in ["//!", "///", "//", "#"] {
            if let Some(rest) = line.strip_prefix(marker) {
                line = rest.trim();
                break;
            }
        }
        // Trailing `\` is a Rust string continuation and a shell line
        // continuation; in both the next line runs straight on from this one.
        let line = line.strip_suffix('\\').unwrap_or(line).trim();
        joined.push(' ');
        joined.push_str(line);
    }
    joined.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// RED today, whichever way the code turned out to say it, because nothing
/// asserted the prose against the code at all: the README said one thing, the
/// site article said the opposite, and both were free to keep saying it.
///
/// The code says tiles ARE encoded to PNG and written to a real sink (proved by
/// the test above), so this pins that sentence into every document here and
/// refuses the phrases the stale in-memory-sink story is written in.
///
/// What this test can reach is this repository. The libviprs.org article lives
/// in libviprs-org and still carries "Neither side encodes to PNG or JPEG" and
/// "libviprs writes to a `MemorySink`"; correcting it is the site lane's edit,
/// and [`TILE_ENCODING_CLAIM`] is the sentence it should carry.
#[test]
fn readme_and_the_article_agree_on_whether_tiles_are_encoded() {
    for rel in DOCUMENTS {
        let text = read_doc(rel);
        assert!(
            text.contains(TILE_ENCODING_CLAIM),
            "{rel} must state the tile-encoding claim verbatim, so the prose cannot drift from \
             the code that proves it. Expected to find:\n  {TILE_ENCODING_CLAIM}"
        );
        for phrase in TILE_ENCODING_CONTRADICTIONS {
            assert!(
                !text.contains(phrase),
                "{rel} contains {phrase:?}, which is the stale in-memory-sink story. Every \
                 timed cell writes encoded PNG tiles to a real FsSink; see \
                 tests/encoding_claim.rs::every_engine_writes_encoded_png_tiles_to_a_real_on_disk_sink"
            );
        }
    }
}

/// RED against the claim being re-typed in a second place rather than quoted
/// from the constant: a sentence that exists twice in source is a sentence that
/// will be edited once.
#[test]
fn the_tile_encoding_claim_has_exactly_one_source() {
    let lib = read_doc("src/lib.rs");
    let occurrences = lib.matches(TILE_ENCODING_CLAIM).count();
    assert_eq!(
        occurrences, 1,
        "src/lib.rs must hold the claim exactly once (the TILE_ENCODING_CLAIM constant); \
         found it {occurrences} times"
    );
    assert!(
        TILE_ENCODING_CLAIM.len() > 60,
        "a claim short enough to appear by accident is not a guard"
    );
}
