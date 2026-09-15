//! The peak RSS the `engines` document publishes is this cell's, not the
//! largest one the parent has ever measured.
//!
//! # Why "the three engines differ" is not the test
//!
//! K2.1 ran the shortcut as a mutation: keep the shared process-wide watermark
//! and reorder the engines so the monolithic one runs last. A cross-engine test
//! went green on three plausible, distinct numbers (37.75, 39.72, 70.57 MB),
//! every one of which was the same watermark read at a different moment.
//! `ru_maxrss` is monotonic, so numbers taken as a process grows differ from one
//! another while all being the same contaminated quantity.
//!
//! What a monotonic watermark cannot do is go DOWN. So the measurement here is
//! a large cell and then a small one, in that order, in one parent process: on a
//! per-child figure the small cell reports the small number, and on a
//! process-wide one it reports the large cell's.
//!
//! # What this covers that K2.1's suite does not
//!
//! The isolation itself is `harness::spawn_single_cell` and it is K2.1's, with
//! its own cross-size guard. This is the same question asked one layer up, of
//! the path the `engines` DOCUMENT takes: `EngineCell::spec_for` builds the
//! child spec, `RepColumns::of` turns the child's metrics into the
//! `peak_rss_mb` column, and either of those could quietly publish something
//! else while the isolation underneath stayed correct.

use libviprs_bench::engines::RepColumns;
use libviprs_bench::engines::cells::EngineCell;
use libviprs_bench::harness::{self, Engine};

/// A canvas whose monolithic peak is large enough that a contaminated small
/// cell is unmistakable, and small enough to measure in a test.
const LARGE: (u32, u32) = (4096, 2880);
/// Two hundred and twenty times less area.
const SMALL: (u32, u32) = (512, 360);

/// RED against a `peak_rss_mb` that comes from a process-wide watermark, and
/// against a column that reports something other than the child it names.
///
/// The order is the assertion. Large first, small second, one parent: a
/// monotonic high-water mark cannot come back down, so a small figure after a
/// large one can only have come from a fresh address space.
#[test]
fn a_small_cell_measured_after_a_large_one_reports_the_small_figure() {
    let exe = std::path::PathBuf::from(env!("CARGO_BIN_EXE_engines"));

    let measure = |cell: EngineCell| -> RepColumns {
        let metrics = harness::spawn_single_cell(&exe, cell.spec_for(Engine::Monolithic))
            .unwrap_or_else(|| panic!("the child measured {}", cell.spec()));
        RepColumns::of(&metrics)
    };

    let large = measure(EngineCell::new(LARGE.0, LARGE.1, 1));
    let small = measure(EngineCell::new(SMALL.0, SMALL.1, 1));

    // The control: the large cell really is large, or the comparison below is
    // between two small numbers and proves nothing.
    assert!(
        large.peak_rss_mb > 40.0,
        "the large cell has to be large enough for contamination to show: {} MB",
        large.peak_rss_mb
    );
    assert!(
        small.peak_rss_mb > 0.0,
        "the small cell reported no RSS at all, so nothing was measured"
    );

    assert!(
        small.peak_rss_mb * 2.0 < large.peak_rss_mb,
        "a {}x{} cell measured AFTER a {}x{} one reports {:.2} MB against {:.2} MB. A \
         monotonic process-wide watermark cannot come back down, so this is the shape that \
         says the figure is the child's own rather than the parent's high-water mark.",
        SMALL.0,
        SMALL.1,
        LARGE.0,
        LARGE.1,
        small.peak_rss_mb,
        large.peak_rss_mb
    );
}

/// RED against a document column wired to the wrong field.
///
/// `peak_rss_mb` and `tracked_memory_mb` are different bases and the document
/// keeps them in separate columns for that reason: one is what the operating
/// system charged the child, the other is what the engine thinks it is holding.
/// A column that published the engine's own accounting under the OS figure's
/// name would be wrong in a way no amount of dispersion would show.
#[test]
fn the_two_memory_columns_are_two_different_bases() {
    let exe = std::path::PathBuf::from(env!("CARGO_BIN_EXE_engines"));
    let cell = EngineCell::new(LARGE.0, LARGE.1, 1);
    let metrics = harness::spawn_single_cell(&exe, cell.spec_for(Engine::Monolithic))
        .expect("the child measured the cell");
    let columns = RepColumns::of(&metrics);

    assert_eq!(columns.peak_rss_mb, metrics.peak_rss_mb());
    assert_eq!(columns.tracked_memory_mb, metrics.tracked_memory_mb());
    assert!(
        columns.peak_rss_mb > columns.tracked_memory_mb,
        "the process's resident set includes the engine's buffers and everything else it \
         needed to get there, so it cannot be the smaller of the two: rss {:.2} MB, tracked \
         {:.2} MB",
        columns.peak_rss_mb,
        columns.tracked_memory_mb
    );
    assert!(
        columns.tracked_memory_mb > 0.0,
        "a libviprs engine accounts for its own raster buffers, so zero here means the column \
         is reading nothing"
    );
}
