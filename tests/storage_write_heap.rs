//! What the two write phases hold, measured rather than reasoned about
//! (libviprs#1136).
//!
//! A binary of its own, holding nothing but tests that measure the heap. The
//! counters in [`libviprs_bench::storage::heap`] are one per process and
//! `libtest` runs a binary's tests on parallel threads, so a measuring test
//! sharing a binary with an ordinary one is measuring the ordinary one too.
//! This is not hypothetical: with these tests next to the rest of the scenario
//! suite, ingestion's retained heap read 3,388,861 bytes for the archive and
//! 2,097,519 for the tree, against 22,817 and 766 measured on their own, and
//! the difference is a neighbouring test's raster.
//!
//! `heap::arm` takes one lock so two windows cannot be open at once, and every
//! test here goes through it. That only helps if the threads not holding it
//! are not allocating either, which is what keeping the binary to measuring
//! tests buys.
//!
//! # Which quantity these numbers are
//!
//! Live heap, whole process, over what was live when the phase started. That
//! is the basis `libviprs/tests/pmtiles_bounded_memory.rs` measures 72 bytes a
//! distinct payload on, and it is **not** the basis
//! `libviprs/src/pmtiles/writer.rs`'s table implies 105.8 bytes a payload on,
//! which is RSS. The two figures in that repository have never been
//! reconcilable with each other and this does not reconcile them; what it does
//! is say which of the two it is on, and prove it by asserting the thing RSS
//! cannot do, which is fall.

use std::collections::BTreeMap;

use libviprs_bench::storage::cells::{Backend, Cell, Source};
use libviprs_bench::storage::heap;
use libviprs_bench::storage::scenarios::write_split;

/// The counting allocator, which is the whole point of this binary.
#[global_allocator]
static HEAP: heap::Counting = heap::Counting;

/// A cell small enough to generate inside an ordinary debug test: 1024x1024 at
/// a 256 pixel tile is 29 planned tiles over eleven levels and a 3 MB raster.
fn tiny(source: Source) -> Cell {
    let mut cell = Cell::new(1024, 1024, 256, source, 0);
    cell.declared_tiles = cell.planned_tiles().expect("the cell plans") as u32;
    cell
}

/// A scratch directory that removes itself.
struct Scratch {
    root: std::path::PathBuf,
}

impl Scratch {
    fn new() -> Scratch {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!("write-heap-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).expect("a scratch directory");
        Scratch { root }
    }

    fn path(&self) -> &std::path::Path {
        &self.root
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// ---------------------------------------------------------------------------
// The gauge
// ---------------------------------------------------------------------------

/// The gauge falls when memory is given back, which is what makes it live heap.
///
/// RED against a high-water counter that only ever climbs, which is what RSS
/// is and which is why the engine repository's two memory figures cannot be
/// reconciled with each other: `tests/pmtiles_bounded_memory.rs` measures live
/// heap and gets 72 bytes a distinct payload, and `src/pmtiles/writer.rs`'s
/// table measures RSS and implies 105.8 across its five rows. This is the
/// assertion that says which of the two quantities the heap numbers this file
/// publishes are.
#[test]
fn the_heap_gauge_counts_live_bytes_and_not_a_high_water_mark() {
    /// The block this test watches go up and come back down.
    const BLOCK: u64 = 4 << 20;
    /// Slack on the block's own size. A window measures the change in live
    /// heap, not this test's own allocations, so memory that was live when the
    /// window opened and is freed inside it moves the reading by a few hundred
    /// bytes in the other direction.
    const SLACK: u64 = 64 << 10;

    let armed = heap::arm();
    assert!(
        armed.installed(),
        "this test binary did not install the counting allocator, so every heap number in it \
         would be a zero wearing a measurement's clothes"
    );

    let block = vec![0u8; BLOCK as usize];
    let held = armed.live_bytes().expect("an armed gauge answers");
    let peak = armed.peak_bytes().expect("an armed gauge answers");
    assert!(
        held + SLACK >= BLOCK,
        "four mebibytes went live and the gauge reads {held}"
    );

    drop(block);
    let after = armed.live_bytes().expect("an armed gauge answers");
    assert!(
        held.saturating_sub(after) + SLACK >= BLOCK,
        "the four mebibytes were freed and live heap only fell from {held} to {after}, so this is \
         a high-water mark and not a live gauge"
    );
    assert!(
        armed.peak_bytes().expect("an armed gauge answers") >= peak,
        "the peak is the one number here that must not fall"
    );
    assert!(
        armed.peak_bytes().expect("an armed gauge answers") >= after,
        "the peak is below what is still live"
    );
}

/// Nesting a window leaves the outer one counting.
///
/// The shape the reconciliation needs: it arms around three scenarios and two
/// of them arm again inside it, so a window that clears the flag on its way
/// out rather than restoring it would turn the measurement off halfway
/// through and the combined row would be the only one not instrumented, which
/// is the asymmetry the arming is there to remove.
///
/// The other half is that a window answers `Some` from the allocator's own
/// flag rather than from a counter. There is no way to exercise the
/// uninstalled case from a binary that installs it, so what is asserted is
/// that the two answers come from one fact: `installed` and a number being
/// there never disagree. The failure that matters is a `peak_bytes` that
/// returns `Some(0)` off counters nothing has written, and zero is the best
/// possible number on a lower-is-better column.
#[test]
fn nesting_a_window_leaves_the_outer_one_counting() {
    {
        let armed = heap::arm();
        assert_eq!(armed.installed(), armed.peak_bytes().is_some());
        assert_eq!(armed.installed(), armed.live_bytes().is_some());
        assert!(armed.installed(), "this binary installs the allocator");
    }
    let outer = heap::arm();
    {
        let inner = heap::arm();
        assert!(
            inner.installed(),
            "a nested window could not find the allocator the window around it is using"
        );
    }
    assert!(
        outer.installed(),
        "an inner window closing disarmed the outer one, so a phase inside a measurement turns \
         the measurement off on its way out"
    );
    let block = vec![0u8; 1 << 20];
    assert!(
        outer.live_bytes().expect("an armed gauge answers") > 0,
        "the outer window stopped counting once the inner one closed"
    );
    drop(block);
}

// ---------------------------------------------------------------------------
// The phases
// ---------------------------------------------------------------------------

/// The finalize peak is the ingest peak it grew from, and the heap numbers are
/// the writer's rather than the raster's.
///
/// Two claims, because the first one on its own is satisfied by a pair of
/// zeros. The peaks are nested by construction: one window spans both phases,
/// so the finalize peak is the high-water mark over the whole pass and the
/// ingest peak is the high-water mark up to the last tile. The writer's module
/// documentation claims "the two numbers above and below `finish` are now the
/// same", and this is that claim with a number behind it.
///
/// The second claim is the positive control. Peak live heap over a generation
/// is mostly the engine's raster, which both backends pay equally, so a peak
/// alone cannot say the gauge is seeing the writer at all. What ingestion
/// *retains* can: the archive keeps a content-hash table, a payload table and
/// a final-offset lookup, one entry a distinct payload, and a tree of loose
/// files keeps nothing. So the two backends' retained heap differ by a factor
/// nobody has to squint at.
#[test]
fn the_write_phases_peak_where_the_writer_holds_its_tables() {
    let cell = tiny(Source::Gradient);
    let plan = cell.plan().expect("the cell plans");
    let mut retained = BTreeMap::new();

    for backend in Backend::ALL {
        let dir = Scratch::new();
        let walked =
            write_split::hand_walk(backend, cell, &plan, dir.path()).expect("the hand walk writes");
        let ingest = walked
            .ingest_peak_bytes
            .expect("this test binary installed the counting allocator");
        let finalize = walked
            .finalize_peak_bytes
            .expect("this test binary installed the counting allocator");
        let live = walked
            .ingest_live_bytes
            .expect("this test binary installed the counting allocator");
        println!(
            "{}: ingest peak {ingest} bytes, finalize peak {finalize} bytes, retained after \
             ingest {live} bytes over {} tiles",
            backend.as_str(),
            walked.tiles_produced
        );

        assert!(
            ingest > 0,
            "{} peaked at nothing, so the gauge saw no allocation at all",
            backend.as_str()
        );
        assert!(
            finalize >= ingest,
            "{} finalized at {finalize} bytes under an ingest peak of {ingest}, and one window \
             spans both, so a peak cannot fall",
            backend.as_str()
        );
        retained.insert(backend.as_str(), live);
    }

    let archive = retained["pmtiles"];
    let tree = retained["directory"];
    assert!(
        archive > tree * 4,
        "ingestion retained {archive} bytes into the archive and {tree} into the tree. The \
         archive keeps a table entry a distinct payload and the tree keeps nothing, so a gauge \
         that cannot tell them apart is measuring the raster and not the writer"
    );
}
