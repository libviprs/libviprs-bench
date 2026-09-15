//! The scenarios and cells the old sweep never had (libviprs-bench#67).
//!
//! Every test here names, in a comment above it, the wrong implementation it is
//! written to go red against. A test that stays green under that mutation is
//! not the test, and the PR body carries the table of what each mutation
//! actually did when it was run.
//!
//! # Nothing here asserts a timing
//!
//! The measured p99 noise floor across a free replicate pair on an *idle* host,
//! at one commit, is 74.5%. Four other lanes are building on this one. So every
//! assertion below is about a shape: a count, an order, an outcome, a flag, a
//! reconciliation. The calibrated sweeps are K2.3's and K2.5's job and they get
//! a quiet host to do it on.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use libviprs::planner::TileCoord;
use libviprs::sink::TileFormat;
use libviprs::sink_pmtiles::PmTilesSink;
use libviprs::{EngineBuilder, FsSink};

use libviprs_bench::storage::cells::{
    self, Backend, Cell, LARGEST_FLAT_ROOT, ROOT_ONLY_MAX_ENTRIES, Regime, SEED, SOURCES, Source,
};
use libviprs_bench::storage::document::Origin;
use libviprs_bench::storage::model::{Modelled, RemoteModel, SyncModel};
use libviprs_bench::storage::scenarios::counting::CountingFactory;
use libviprs_bench::storage::scenarios::{
    ReaderFactory, concurrent_curve, decode_root, first_lookup, open, plan_order, replicate,
    requests, tileid_order,
};
use libviprs_bench::storage::{FileReaderFactory, raster};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A cell small enough to generate inside an ordinary debug test.
///
/// 1024 by 1024 at a 256 pixel tile: 29 planned tiles across eleven levels, a
/// 3 MB raster, and a deepest level of 4 by 4 that divides exactly, so no edge
/// tile is clipped and a flat source really does produce identical payloads.
fn tiny(source: Source) -> Cell {
    cell_at(1024, 1024, 256, source)
}

fn cell_at(width: u32, height: u32, tile_px: u32, source: Source) -> Cell {
    let mut cell = Cell::new(width, height, tile_px, source, 0);
    cell.declared_tiles = cell.planned_tiles().expect("the cell plans") as u32;
    cell
}

/// How many tiles a cell plans, as the planner answers it.
fn planned(cell: &Cell) -> u64 {
    cell.planned_tiles().expect("the cell plans") as u64
}

/// A reader factory over a generated archive, which is where every scenario
/// here gets a reader. Nothing in this file constructs one itself.
fn readers_for(cell: &Cell, archive: &Path) -> FileReaderFactory {
    FileReaderFactory::new(
        Backend::PmTiles,
        archive,
        &cell.plan().expect("the cell plans"),
    )
}

/// The brink cell's tile size at a canvas small enough for an ordinary test.
///
/// 1024 by 1024 at a 46 pixel tile: 728 planned tiles, and a flat fill there
/// collapses to 80 entries while both published sources pay one entry a tile.
/// The engine's gradient does not collapse at 256 either, so this cell is about
/// having a source that deduplicates to compare against rather than about
/// avoiding one that does.
fn distinct_tiny(source: Source) -> Cell {
    cell_at(1024, 1024, 46, source)
}

fn write_archive(dir: &Path, cell: &Cell) -> PathBuf {
    let plan = cell.plan().expect("the cell plans");
    let raster = raster(cell.source, cell.width, cell.height);
    let archive = dir.join(format!("{}.pmtiles", cell.source.as_str()));
    let sink = PmTilesSink::builder(&archive)
        .plan(plan.clone())
        .tile_format(TileFormat::Png)
        .build()
        .expect("the archive sink builds");
    EngineBuilder::new(&raster, plan, sink)
        .run()
        .expect("the archive run succeeds");
    archive
}

fn write_tree(dir: &Path, cell: &Cell) -> PathBuf {
    let plan = cell.plan().expect("the cell plans");
    let raster = raster(cell.source, cell.width, cell.height);
    let root = dir.join(format!("{}-tree", cell.source.as_str()));
    let sink = FsSink::new(&root, plan.clone()).with_format(TileFormat::Png);
    EngineBuilder::new(&raster, plan, sink)
        .run()
        .expect("the directory run succeeds");
    root
}

/// A coordinate the archive really holds: the first one the plan names.
fn first_coord(cell: &Cell) -> TileCoord {
    cells::coordinates(&cell.plan().expect("the cell plans"))[0]
}

// ---------------------------------------------------------------------------
// The cutoff, and the cells around it
// ---------------------------------------------------------------------------

/// The writer's cutoff is still what this crate copied, and the comparison is
/// still strict.
///
/// RED against a crate that hard-codes 16384 as the largest flat root, which is
/// what libviprs#1021 said and what a reader would assume: the writer's test is
/// `<`, so 16384 entries already spill into leaves and 16383 is the biggest
/// flat root there is.
#[test]
fn the_writers_cutoff_is_the_number_this_crate_copied() {
    let writer = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../libviprs/src/pmtiles/writer.rs"
    ))
    .expect("the engine sits alongside as a path dependency, so its writer is readable");

    let declaration = format!("const ROOT_ONLY_MAX_ENTRIES: u64 = {ROOT_ONLY_MAX_ENTRIES};");
    assert!(
        writer.contains(&declaration),
        "the engine no longer declares `{declaration}`, so every cutoff this crate names came \
         from somewhere else"
    );
    assert!(
        writer.contains("if plan.entry_count < ROOT_ONLY_MAX_ENTRIES"),
        "the writer no longer decides a flat root with a strict `<`, so the largest flat root may \
         not be {LARGEST_FLAT_ROOT} any more"
    );
    assert_eq!(
        LARGEST_FLAT_ROOT,
        ROOT_ONLY_MAX_ENTRIES - 1,
        "a strict comparison puts the largest flat root one under the cutoff"
    );
}

/// The brink cell is still the best cell the search space reaches.
///
/// RED against a canvas picked by eye, or against one that used to be the brink
/// and stopped being it when the planner's rounding moved. It is arithmetic,
/// and it says which cell to build; what the archive came out as is
/// `the_brink_cell_sits_under_the_root_cutoff_and_the_leaf_cell_over_it`.
#[test]
fn the_brink_search_still_picks_the_canvas_the_cell_names() {
    let (found, tiles) = cells::brink_search();
    let configured = cells::brink_cell(Source::Gradient);
    assert_eq!(
        (found.width, found.height, found.tile_px),
        (configured.width, configured.height, configured.tile_px),
        "the search space's best cell is now {} planning {tiles} tiles, and the table still names \
         {}",
        found.spec(),
        configured.spec()
    );
    assert!(
        tiles <= LARGEST_FLAT_ROOT,
        "the brink cell plans {tiles} tiles, at or over the {LARGEST_FLAT_ROOT} entries the writer \
         will still keep flat"
    );
    let short_by = LARGEST_FLAT_ROOT - tiles;
    assert!(
        short_by * 100 < LARGEST_FLAT_ROOT,
        "the brink cell plans {tiles} tiles, {short_by} short of the cutoff, which is far enough \
         off it that the cell brackets the peak of the ramp instead of measuring it"
    );
}

/// Ask each archive what it came out as, rather than trusting the arithmetic.
///
/// RED against the wrong canvas: a brink cell whose archive grew leaves is
/// measuring the far side of the cliff, and a leaf cell whose archive stayed
/// flat is measuring the ramp. If the planner disagrees with the arithmetic the
/// cell moves and this test is what says so.
///
/// `#[ignore]`d because it generates a fifty megabyte archive and a two hundred
/// megabyte one. Run it with:
///
/// ```text
/// cargo test --release --test storage_scenarios -- --ignored \
///   the_brink_cell_sits_under_the_root_cutoff_and_the_leaf_cell_over_it --nocapture
/// ```
#[test]
#[ignore = "generates the brink and leaf archives; run with --ignored"]
fn the_brink_cell_sits_under_the_root_cutoff_and_the_leaf_cell_over_it() {
    let dir = tempdir();

    let brink = cells::brink_cell(Source::Gradient);
    let brink_archive = write_archive(dir.path(), &brink);
    let (brink_entries, brink_leaves) =
        cells::root_shape(&brink_archive).expect("the archive opens");
    println!(
        "brink {} planned {} tiles, root holds {brink_entries} entries, {brink_leaves} of them \
         leaf pointers, {} under the {LARGEST_FLAT_ROOT} the writer still keeps flat",
        brink.spec(),
        planned(&brink),
        LARGEST_FLAT_ROOT - brink_entries
    );

    assert_eq!(
        brink_leaves, 0,
        "the brink cell's archive grew leaf directories, so its root is a handful of pointers and \
         it measures the far side of the cliff rather than the top of the ramp"
    );
    assert!(
        brink_entries <= LARGEST_FLAT_ROOT,
        "the brink cell's root holds {brink_entries} entries, at or past the {LARGEST_FLAT_ROOT} \
         the writer will keep flat, which is not a root this writer emits"
    );
    let short_by = LARGEST_FLAT_ROOT - brink_entries;
    assert!(
        short_by * 100 < LARGEST_FLAT_ROOT,
        "the brink cell's root holds {brink_entries} entries, {short_by} under the cutoff, so the \
         sweep brackets the worst case again instead of measuring it"
    );
    assert_eq!(
        brink_entries,
        planned(&brink),
        "the gradient's tiles are all distinct, so every planned tile should cost one root entry; \
         a run collapsed and the root is smaller than the cell's tile count"
    );
    assert_eq!(
        cells::observed_regime(&brink_archive).expect("the archive opens"),
        Regime::Root
    );

    let leaf = cells::leaf_cell(Source::Gradient);
    let leaf_archive = write_archive(dir.path(), &leaf);
    let (leaf_entries, leaf_leaves) = cells::root_shape(&leaf_archive).expect("the archive opens");
    println!(
        "leaf {} planned {} tiles, root holds {leaf_entries} entries, {leaf_leaves} of them leaf \
         pointers",
        leaf.spec(),
        planned(&leaf)
    );
    assert!(
        leaf_leaves >= 1,
        "the leaf cell's archive stayed flat, so it measures the ramp rather than the cliff after \
         it"
    );
    assert!(
        planned(&leaf) > LARGEST_FLAT_ROOT,
        "the leaf cell plans {} tiles, which the writer would still keep in a flat root",
        planned(&leaf)
    );
    assert_eq!(
        cells::observed_regime(&leaf_archive).expect("the archive opens"),
        Regime::Leaves
    );
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// A flat source collapses the root, and no sweep may publish a row from one.
///
/// RED against a `publishes_rows` that says yes to everything, and against a
/// `publishable` that filters nothing. The positive control is in the same
/// test twice: the two real sources must still publish, or a rule that refused
/// everything would pass; and the archive must really collapse, or "flat is
/// special" is a claim about a `match` arm rather than about the writer.
#[test]
fn a_flat_source_never_produces_a_published_row() {
    let dir = tempdir();

    let flat = distinct_tiny(Source::Flat);
    let flat_archive = write_archive(dir.path(), &flat);
    let (flat_entries, _) = cells::root_shape(&flat_archive).expect("the archive opens");

    let gradient = distinct_tiny(Source::Gradient);
    let gradient_archive = write_archive(dir.path(), &gradient);
    let (gradient_entries, _) = cells::root_shape(&gradient_archive).expect("the archive opens");

    println!(
        "{} planned tiles: flat root {flat_entries} entries, gradient root {gradient_entries}",
        planned(&flat)
    );

    // The reason flat is not a cell: its root is not the cell's tile count.
    assert!(
        flat_entries < planned(&flat),
        "the flat source's root holds {flat_entries} entries for {} planned tiles, so nothing \
         deduplicated and this source is not the dedupe guard it is here to be",
        planned(&flat)
    );
    // The positive control for that: a source whose tiles are all distinct pays
    // one entry per tile, so the collapse above is the writer's run-length
    // encoding and not something about this canvas.
    assert_eq!(
        gradient_entries,
        planned(&gradient),
        "the gradient's tiles are meant to be all distinct"
    );

    assert!(!Source::Flat.publishable());
    assert!(Source::Gradient.publishable() && Source::Noise.publishable());

    let sweep: Vec<Cell> = SOURCES
        .iter()
        .map(|source| distinct_tiny(*source))
        .collect();
    let published = cells::publishable(&sweep);
    assert_eq!(
        published.len(),
        2,
        "a sweep over every source publishes the gradient and the noise rows and nothing else, \
         and it published {published:?}"
    );
    assert!(published.iter().all(|cell| cell.source != Source::Flat));
}

/// The noise source really is the incompressible one.
///
/// RED against a `noise` that is another gradient, another flat fill, or a
/// per-tile constant: any of those compresses, and then the source axis this
/// lane added measures nothing.
///
/// The claim is measured against the raw pixels the pyramid holds rather than
/// against the gradient archive, because "incompressible" means "the codec got
/// nothing", not "bigger than the other one". A ratio between the two archives
/// would also be a moving target: the engine's gradient is far less
/// compressible than the ramp I first reconstructed, and a 2x rule that held
/// against one holds by 2.5% against the other. The gradient is still in the
/// test, as the control that the ratio discriminates at all.
#[test]
fn the_noise_source_is_incompressible_and_the_gradient_is_not() {
    let dir = tempdir();

    let noise = tiny(Source::Noise);
    let gradient = tiny(Source::Gradient);
    let raw: u64 = noise
        .plan()
        .expect("the cell plans")
        .levels
        .iter()
        .map(|level| u64::from(level.width) * u64::from(level.height) * 3)
        .sum();
    let noise_bytes = std::fs::metadata(write_archive(dir.path(), &noise))
        .expect("the noise archive exists")
        .len();
    let gradient_bytes = std::fs::metadata(write_archive(dir.path(), &gradient))
        .expect("the gradient archive exists")
        .len();

    let noise_ratio = noise_bytes as f64 / raw as f64;
    let gradient_ratio = gradient_bytes as f64 / raw as f64;
    println!(
        "{raw} raw pyramid bytes: noise archive {noise_bytes} ({noise_ratio:.2}x), gradient \
         archive {gradient_bytes} ({gradient_ratio:.2}x)"
    );
    assert!(
        noise_ratio > 0.9,
        "the noise archive is {noise_ratio:.2} of the {raw} raw pyramid bytes, so the codec got \
         something out of it and it is not the incompressible source"
    );
    assert!(
        gradient_ratio < 0.7,
        "the gradient archive is {gradient_ratio:.2} of the raw pyramid bytes, so this measure \
         cannot tell a compressible source from an incompressible one"
    );

    // And it is deterministic, because a benchmark source that moves between
    // runs is a different amount of work each time.
    let again = raster(Source::Noise, 64, 64);
    let once = raster(Source::Noise, 64, 64);
    assert_eq!(again.data(), once.data(), "the noise source is seeded");
}

// ---------------------------------------------------------------------------
// open
// ---------------------------------------------------------------------------

/// An open reads the header and the root and touches no tile.
///
/// RED against an open that prefetches: one extra range read puts the count
/// over two, and a prefetch of the first tile lands a request inside the tile
/// data section. The lookup in the same test is the positive control, because
/// "no request landed in the tile section" is also what an open whose requests
/// nobody counted would say.
#[test]
fn open_counts_exactly_the_header_and_root_reads_and_no_tile() {
    let dir = tempdir();
    let cell = tiny(Source::Gradient);
    let archive = write_archive(dir.path(), &cell);
    let coord = first_coord(&cell);

    let counting = CountingFactory::new(&archive);
    let (observation, lookup_requests) =
        open::observe_with_lookup(&counting, coord).expect("the archive opens");

    assert_eq!(
        observation.request_count(),
        2,
        "an open is the 127-byte header and the root directory and nothing else; it made {:?}",
        observation.requests
    );
    assert!(
        observation.tile_section_requests().is_empty(),
        "the open reached into the tile data section at {:?}",
        observation.tile_section_requests()
    );

    let end = observation.tile_data_offset + observation.tile_data_length;
    let in_tiles: Vec<_> = lookup_requests
        .iter()
        .filter(|r| r.offset < end && r.end() > observation.tile_data_offset)
        .collect();
    assert!(
        !in_tiles.is_empty(),
        "the lookup after the open fetched no tile bytes, so this test cannot tell an open that \
         prefetches from one whose requests nobody counted; requests were {lookup_requests:?}"
    );

    // The plain observe() path is the scenario, and it agrees with the one that
    // also looks a tile up.
    let plain = open::observe(&counting).expect("the archive opens");
    assert_eq!(plain.request_count(), 2);
    assert_eq!(plain.root_entries, observation.root_entries);
}

/// The reconciliation guard says no to a cell too small to reconcile on.
///
/// RED against a guard that accepts every cell, which is what a split guard
/// becomes the moment somebody widens its allowance until the 93-entry cell
/// passes. That cell drifts -24% on arm64 and -34% on x86_64 with nothing wrong
/// with the split, because the whole open there is a few microseconds and
/// per-iteration overhead is a fifth of it.
#[test]
fn the_cold_split_guard_refuses_a_root_too_small_to_reconcile() {
    let refusal = open::reconciliation_is_meaningful(93)
        .expect_err("a 93-entry root is too small for the phases to reconcile against");
    assert!(
        refusal.contains("93"),
        "the refusal has to name the root it refused: {refusal}"
    );

    // The positive control: the guard is not simply a `no`. The cell the
    // engine's own split guard measures is about 1400 entries and it passes.
    open::reconciliation_is_meaningful(1_373)
        .expect("a 1373-entry root is what the engine's own split guard reconciles on");
    open::reconciliation_is_meaningful(open::MIN_RECONCILABLE_ROOT_ENTRIES)
        .expect("the floor itself is meaningful");
    assert!(open::reconciliation_is_meaningful(open::MIN_RECONCILABLE_ROOT_ENTRIES - 1).is_err());
}

/// The drift arithmetic is signed and the allowance is two-sided.
///
/// RED against a `reconciles` that compares a raw difference, or an unsigned
/// one: the split comes out *under* the combined row on a small cell and over
/// it on a large one, so a one-sided check passes the half of the failures it
/// was not written for.
#[test]
fn the_reconciliation_allowance_is_two_sided() {
    assert!(open::reconciles(100.0, 100.0));
    assert!(open::reconciles(120.0, 100.0));
    assert!(open::reconciles(80.0, 100.0));
    assert!(!open::reconciles(126.0, 100.0));
    assert!(!open::reconciles(74.0, 100.0));
    assert!(open::drift_pct(76.0, 100.0) < 0.0);
    assert!(open::drift_pct(124.0, 100.0) > 0.0);
}

/// The six phases add up to the combined row on a cell big enough to say so.
///
/// RED against a split that measures some other piece of work: a phase timing
/// the wrong call, a walk that skips the inflate, a lookup phase that reuses
/// the reader the decode phase built. `#[ignore]`d because it needs a root
/// around 1400 entries, which is a 200 MB raster.
#[test]
#[ignore = "generates the mid cell; run with --ignored"]
fn the_cold_split_accounts_for_the_whole_combined_row() {
    let dir = tempdir();
    // 2048x2048 at a 64 pixel tile: 1371 planned tiles and a gradient root of
    // 1290 entries, which is the size the engine's own split guard reconciles
    // on and small enough to generate in a minute. The plan's mid cell is
    // 8192x8192 at a 256 pixel tile, and the gradient collapses that one to a
    // root of thirteen entries, which the reconciliation guard rightly refuses.
    let cell = cell_at(2048, 2048, 64, Source::Gradient);
    let archive = write_archive(dir.path(), &cell);
    let (entries, _) = cells::root_shape(&archive).expect("the archive opens");
    open::reconciliation_is_meaningful(entries).expect("the mid cell's root is big enough");

    let coords: Vec<TileCoord> = cells::coordinates(&cell.plan().expect("the cell plans"))
        .into_iter()
        .take(32)
        .collect();
    let pass = open::split_pass(&archive, &readers_for(&cell, &archive), &coords, entries)
        .expect("the archive opens");

    let split_us = median_micros(&pass.totals());
    let combined_us = median_micros(&pass.combined);
    let drift = open::drift_pct(split_us, combined_us);
    println!(
        "root {entries} entries: split {split_us:.2} us, combined {combined_us:.2} us, drift \
         {drift:.1}%"
    );
    assert!(
        open::reconciles(split_us, combined_us),
        "the phases sum to {split_us:.2} us against a combined row of {combined_us:.2}, a drift of \
         {drift:.1}% and the allowance is {}%",
        open::RECONCILIATION_ALLOWANCE_PCT
    );
    assert_eq!(pass.by_phase().len(), open::COLD_PHASES.len());
}

// ---------------------------------------------------------------------------
// first_lookup
// ---------------------------------------------------------------------------

/// The child entry point.
///
/// Does nothing at all unless a parent asked for it, so an ordinary run of this
/// binary passes it in microseconds.
#[test]
fn k14_first_lookup_child() {
    let Ok(spec) = std::env::var(first_lookup::CHILD_VAR) else {
        return;
    };
    println!("{}", first_lookup::child_main(&spec));
}

/// Every repetition of `first_lookup` runs in a process of its own.
///
/// RED against a loop in one process, which is what `read_cold` was: the pids
/// then collapse to one, and to the parent's own. The pid comes out of the
/// child, so an implementation that never spawned one cannot report a pid it
/// does not have.
#[test]
fn first_lookup_runs_in_a_fresh_process_per_rep() {
    let dir = tempdir();
    let cell = tiny(Source::Gradient);
    let archive = write_archive(dir.path(), &cell);
    let coord = first_coord(&cell);
    let exe = std::env::current_exe().expect("a test binary knows where it is");

    let reps = 4;
    let measured = first_lookup::run(reps, |_| {
        let mut command = first_lookup::child_command(&exe, &archive, coord);
        command.args(["--exact", "k14_first_lookup_child", "--nocapture"]);
        command
    })
    .expect("every repetition spawns and answers");

    assert_eq!(measured.len(), reps);
    assert_eq!(
        first_lookup::distinct_pids(&measured),
        reps,
        "{reps} repetitions ran in {} distinct processes: {measured:?}",
        first_lookup::distinct_pids(&measured)
    );
    let mine = std::process::id();
    assert!(
        measured.iter().all(|rep| rep.pid != mine),
        "a repetition reported this process's own pid ({mine}), so it never left it: {measured:?}"
    );
    assert!(
        measured.iter().all(|rep| rep.hit),
        "the coordinate every repetition looked up is the first tile of the plan and the archive \
         holds it: {measured:?}"
    );
}

/// The child spec survives the round trip.
///
/// RED against a spec that loses the level, which would send every child to
/// (0, 0, 0) and make the scenario measure one coordinate under four names.
#[test]
fn the_child_spec_round_trips() {
    let coord = TileCoord {
        level: 7,
        col: 41,
        row: 3,
    };
    let spec = first_lookup::child_spec(Path::new("/tmp/a b/pyramid.pmtiles"), coord);
    let (path, back) = first_lookup::parse_child_spec(&spec).expect("the spec parses");
    assert_eq!(path, PathBuf::from("/tmp/a b/pyramid.pmtiles"));
    assert_eq!(back, coord);
}

// ---------------------------------------------------------------------------
// decode_root
// ---------------------------------------------------------------------------

/// `decode_root` reports the archive's entry count, not the cell's tile count.
///
/// RED against a scenario that reports the planned tile count, or the cell's
/// label, or a constant. The flat source is what makes it a real distinction:
/// its run-length-encoded root is far smaller than its plan, so the two numbers
/// cannot be confused with each other. The gradient archive in the same test is
/// the control that the two *can* coincide, which is why quoting the plan looks
/// right on every cell the sweep used to have.
#[test]
fn decode_root_reports_the_archives_own_entry_count_not_the_plans_tile_count() {
    let dir = tempdir();

    let flat = distinct_tiny(Source::Flat);
    let flat_archive = write_archive(dir.path(), &flat);
    let (flat_entries, _) = cells::root_shape(&flat_archive).expect("the archive opens");
    let decoded = decode_root::observe(&flat_archive, 3).expect("the archive opens");

    assert_eq!(
        decoded.entries, flat_entries,
        "the decode produced {} entries and the archive's root holds {flat_entries}",
        decoded.entries
    );
    assert_ne!(
        decoded.entries,
        planned(&flat),
        "on a deduplicating source the root entry count and the tile count have to differ, or \
         this test cannot tell a decode from a restatement of the plan"
    );
    assert_eq!(decoded.samples.len(), 3);
    assert!(decoded.plain_bytes > 0 && decoded.compressed_bytes > 0);
    assert!(decoded.nanos_per_entry(decoded.samples[0]).is_some());

    let gradient = distinct_tiny(Source::Gradient);
    let gradient_archive = write_archive(dir.path(), &gradient);
    let gradient_decoded = decode_root::observe(&gradient_archive, 1).expect("the archive opens");
    assert_eq!(gradient_decoded.entries, planned(&gradient));
}

// ---------------------------------------------------------------------------
// plan_order and tileid_order
// ---------------------------------------------------------------------------

/// The tile-id walk is monotone in tile id and the plan walk is not.
///
/// RED against a copy-paste of `plan_order` that sorts neither. The trap this
/// test is written around is that a hand-picked pair of coordinates can land on
/// a position the sort leaves alone, and then the assertion is green under
/// exactly the mutation it was written for. So the probe is **computed**:
/// `positions_the_sort_moves` is the set of positions the wrong implementation
/// actually moves, the test asserts that set is not empty before it asserts
/// anything else, and the probe it prints comes out of it.
#[test]
fn tileid_order_is_monotone_in_tile_id_and_plan_order_is_not() {
    let cell = cells::smoke_cell(Source::Gradient);
    let plan = cell.plan().expect("the cell plans");
    let n = plan_order::coordinates(&plan, usize::MAX).len();
    let walked = plan_order::coordinates(&plan, n);
    let sorted = tileid_order::coordinates(&plan, n);

    assert_eq!(walked.len(), sorted.len());
    assert!(
        tileid_order::same_multiset(&walked, &sorted),
        "the two walks have to cover the same tiles, or they are two different amounts of work"
    );

    // The fixed-point check, and it comes first. If the sort moved nothing on
    // this cell, everything below is green against an implementation that sorts
    // nothing, and the test would be a decoration.
    let moved = tileid_order::positions_the_sort_moves(&walked, &sorted);
    assert!(
        !moved.is_empty(),
        "the plan order and the tile-id order coincide on {}, so nothing here can fail against an \
         implementation that sorts neither; the cell has to move, not the test",
        cell.spec()
    );
    let probe = tileid_order::probe(&walked, &sorted).expect("a moved position is a probe");
    println!(
        "{}: {} of {n} positions move; probe at index {} is {:?} (tile id {:?}) in plan order and \
         {:?} (tile id {:?}) in tile-id order",
        cell.spec(),
        moved.len(),
        probe.index,
        probe.in_plan_order,
        probe.in_plan_order_tile_id,
        probe.in_tileid_order,
        probe.in_tileid_order_tile_id
    );
    assert_ne!(
        probe.in_plan_order_tile_id, probe.in_tileid_order_tile_id,
        "a position the sort moves holds two different tiles, so it holds two different tile ids"
    );

    // The claim itself.
    assert!(
        tileid_order::is_monotone(&sorted),
        "the tile-id walk goes backwards at {:?}",
        tileid_order::first_inversion(&sorted)
    );
    let inversion = tileid_order::first_inversion(&walked).expect(
        "the plan walk is row-major within a level and the archive is a Hilbert curve, so the plan \
         walk has to go backwards somewhere",
    );
    println!(
        "plan order first goes backwards at index {}: tile id {} then {}",
        inversion.index, inversion.before, inversion.after
    );
}

/// Both walks truncate to the same `n`, and `n` past the end is the whole plan.
///
/// RED against a tile-id walk that sorts the whole plan and *then* truncates,
/// which would walk a different set of tiles from the plan walk at every `n`
/// short of the pyramid.
#[test]
fn both_walks_take_the_same_n_coordinates() {
    let plan = cells::smoke_cell(Source::Gradient)
        .plan()
        .expect("the cell plans");
    let whole = plan_order::coordinates(&plan, usize::MAX).len();

    for n in [1usize, 7, 16, whole, whole + 100] {
        let walked = plan_order::coordinates(&plan, n);
        let sorted = tileid_order::coordinates(&plan, n);
        assert_eq!(walked.len(), n.min(whole));
        assert_eq!(sorted.len(), n.min(whole));
        assert!(
            tileid_order::same_multiset(&walked, &sorted),
            "at n={n} the two walks cover different tiles"
        );
    }
}

// ---------------------------------------------------------------------------
// concurrent_curve
// ---------------------------------------------------------------------------

/// A rung runs up to twice the core count, and one past that is skipped with a
/// reason and stays in the document.
///
/// RED against omission, and RED against the rule this replaced, which declined
/// every rung above `ncpu`. A dropped row reads as a measurement nobody took,
/// which is a different and worse claim than one this host declined. A row
/// declined for a good reason is still a hole where the finding should be:
/// libviprs#1024 put the x86_64 knee at eight threads and the only host that
/// measures x86_64 natively has six cores, so the old rule declined the rung the
/// whole x86_64 concurrency story lives on, on every cell, forever.
#[test]
fn a_rung_runs_to_twice_the_core_count_and_says_when_it_is_oversubscribed() {
    let ladder = concurrent_curve::ladder(6);
    assert_eq!(
        ladder.len(),
        concurrent_curve::THREAD_LADDER.len(),
        "every rung stays in the document: {ladder:?}"
    );
    assert_eq!(
        ladder.iter().map(|arm| arm.threads).collect::<Vec<_>>(),
        concurrent_curve::THREAD_LADDER.to_vec()
    );
    assert!(
        ladder.iter().all(|arm| arm.is_ok()),
        "six cores reach every rung of a ladder that stops at eight: {ladder:?}"
    );
    let eight = ladder
        .iter()
        .find(|arm| arm.threads == 8)
        .expect("the ladder has an eight-thread rung");
    assert!(
        eight.oversubscribed,
        "eight threads on six cores is oversubscription and the row has to say so"
    );
    assert_eq!(
        eight.outcome(),
        libviprs_bench::storage::scenarios::Outcome::Ok
    );
    for arm in ladder.iter().filter(|arm| arm.threads <= 6) {
        assert!(
            !arm.oversubscribed,
            "{} threads on six cores is not oversubscription",
            arm.threads
        );
    }

    // Past twice the cores the rung is declined, and the reason names both.
    let cramped = concurrent_curve::ladder(2);
    let declined: Vec<_> = cramped.iter().filter(|arm| !arm.is_ok()).collect();
    assert_eq!(
        declined.len(),
        1,
        "on two cores exactly the eight-thread rung is past 2x: {cramped:?}"
    );
    assert_eq!(declined[0].threads, 8);
    assert_eq!(
        declined[0].outcome(),
        libviprs_bench::storage::scenarios::Outcome::Skipped
    );
    let reason = declined[0]
        .reason()
        .expect("a declined rung carries its reason");
    assert!(
        reason.contains('2') && reason.contains('8'),
        "the reason has to name the cores and the rung: {reason}"
    );
    // The four-thread rung on two cores runs, and is marked.
    let four = cramped
        .iter()
        .find(|arm| arm.threads == 4)
        .expect("a four-thread rung");
    assert!(four.is_ok());
    assert!(four.oversubscribed);

    // The positive control: the rule is about the host, not a constant that
    // always refuses eight and not one that always accepts it.
    let roomy = concurrent_curve::ladder(16);
    assert!(
        roomy.iter().all(|arm| arm.is_ok() && !arm.oversubscribed),
        "a sixteen-core host measures every rung and oversubscribes none: {roomy:?}"
    );
    let single = concurrent_curve::ladder(1);
    assert_eq!(
        single.iter().filter(|arm| arm.is_ok()).count(),
        2,
        "a single-core host reaches the control and one rung above it: {single:?}"
    );
}

/// The ladder reaches the rung each architecture's knee sits on, on the hosts
/// this suite really publishes from.
///
/// RED against the rule this replaced. libviprs#1024 measured the bend: on
/// arm64 p99 on the leaf-bearing cell goes 2.04, 2.27, 10.38, 43.06 us at one,
/// two, four and eight threads, so the knee is at four; on native x86_64 the
/// first three points are flat and the whole move is at eight. The arm64 laptop
/// has eight cores and reaches its knee either way. The native x86_64 box has
/// six, and under `threads > ncpu` it declined the eight-thread rung on every
/// cell, so the epic's x86_64 concurrency story could not be reproduced from
/// the one machine that can measure x86_64 natively.
///
/// Asserting that the skip was recorded with a good reason is what the suite
/// did before, and it passed while the finding was unreachable.
#[test]
fn the_ladder_measures_the_rung_each_architectures_knee_sits_on() {
    // libviprs#1024, both of them measured rather than chosen.
    const ARM64_KNEE: usize = 4;
    const X86_64_KNEE: usize = 8;
    // The two hosts this suite publishes from, by their real core counts.
    for (host, ncpu, knee) in [
        ("the arm64 laptop", 8usize, ARM64_KNEE),
        ("the native x86_64 box", 6usize, X86_64_KNEE),
    ] {
        let ladder = concurrent_curve::ladder(ncpu);
        let rung = ladder
            .iter()
            .find(|arm| arm.threads == knee)
            .unwrap_or_else(|| panic!("{host}: the ladder has no rung at {knee} threads"));
        assert!(
            rung.is_ok(),
            "{host} has {ncpu} cores and declines the {knee}-thread rung, which is where its \
             knee is: {:?}",
            rung.reason()
        );
        assert_eq!(
            rung.outcome(),
            libviprs_bench::storage::scenarios::Outcome::Ok,
            "{host}: the rung at the knee has to be a measurement"
        );
        // And the published row says whether the cores were there for it, so a
        // reader never compares the x86_64 box's T=8 with the laptop's.
        assert_eq!(rung.oversubscribed, knee > ncpu, "{host}");
    }
}

/// Every scenario says whether its thread budget is above the host's cores, and
/// only the ladder has one to say.
///
/// RED against a row that carries no such column: an eight-thread rung measured
/// on six cores and one measured on eight are then the same row shape with the
/// same key, and nothing downstream can refuse to grade one against the other.
#[test]
fn only_the_ladder_claims_a_thread_budget_and_it_claims_it_honestly() {
    use libviprs_bench::storage::scenarios::{Scenario, registry};

    let mut ladder_rungs = 0;
    for scenario in registry() {
        let name = scenario.name();
        match name.strip_prefix("read_concurrent@") {
            Some(threads) => {
                ladder_rungs += 1;
                let threads: usize = threads.parse().expect("a rung names its thread count");
                assert_eq!(
                    scenario.oversubscribed(6),
                    Some(threads > 6),
                    "{name} on six cores"
                );
                assert_eq!(
                    scenario.oversubscribed(16),
                    Some(false),
                    "{name} on sixteen cores"
                );
            }
            None => assert_eq!(
                scenario.oversubscribed(1),
                None,
                "{name} names no thread budget, so it must claim none"
            ),
        }
    }
    assert_eq!(
        ladder_rungs,
        concurrent_curve::THREAD_LADDER.len(),
        "the registry lost a rung"
    );
}

/// The T=1 control is the same work as `read_random`, on the calling thread.
///
/// RED against a T=1 path that carries thread overhead, or a different
/// coordinate set. It is written as a structural check rather than as a
/// comparison of two timings on purpose: the replicate spread on p99 is 74.5%
/// on an idle host, so a tie-band assertion between two timed passes is a coin
/// toss and would be one even if nothing else were running. What makes the two
/// agree is that they walk the identical sequence with no spawn and no join,
/// and that is observable.
#[test]
fn the_t1_control_agrees_with_read_random_within_the_tie_band() {
    let dir = tempdir();
    let cell = tiny(Source::Gradient);
    let archive = write_archive(dir.path(), &cell);
    let readers = readers_for(&cell, &archive);
    let reader = readers.fresh().expect("the archive opens for reading");

    let coords = random_order(
        &cells::coordinates(&cell.plan().expect("the cell plans")),
        SEED,
    );

    // The same coordinate set, in the same order.
    let pieces = concurrent_curve::chunks(&coords, 1);
    assert_eq!(pieces.len(), 1);
    assert_eq!(
        pieces[0],
        coords.as_slice(),
        "the T=1 rung walks `read_random`'s sequence, not a reshuffle of it"
    );

    let me = std::thread::current().id();
    let control = concurrent_curve::run_arm(reader.as_ref(), &coords, 1).expect("lookups succeed");
    assert_eq!(control.coordinates_walked, coords.len());
    assert_eq!(control.latencies.len(), coords.len());
    assert!(
        control.ran_on_only(me),
        "the control ran on {:?} and this thread is {me:?}, so it paid for a spawn and a join the \
         pass it is a control for does not",
        control.thread_ids
    );

    // The positive control: a rung that really does spawn is visible here, so
    // `ran_on_only` is an observation rather than a constant.
    let two = concurrent_curve::run_arm(reader.as_ref(), &coords, 2).expect("lookups succeed");
    assert!(
        !two.ran_on_only(me),
        "the two-thread rung reported only this thread, so the thread-id evidence cannot tell a \
         spawn from an inline walk"
    );
    assert_eq!(two.coordinates_walked, coords.len());
    assert_eq!(two.latencies.len(), coords.len());
}

/// Chunks are contiguous, cover everything, and stay balanced.
///
/// RED against a split that drops the remainder, which silently shortens the
/// walk at every thread count that does not divide the coordinate set.
#[test]
fn the_thread_chunks_concatenate_back_to_the_whole_walk() {
    let coords: Vec<TileCoord> = (0..37)
        .map(|index| TileCoord {
            level: 5,
            col: index,
            row: 0,
        })
        .collect();

    for threads in concurrent_curve::THREAD_LADDER {
        let pieces = concurrent_curve::chunks(&coords, threads);
        assert_eq!(pieces.len(), threads);
        let rejoined: Vec<TileCoord> = pieces.iter().flat_map(|p| p.iter().copied()).collect();
        assert_eq!(rejoined, coords, "at T={threads} the chunks lost a tile");
        let longest = pieces.iter().map(|p| p.len()).max().unwrap_or(0);
        let shortest = pieces.iter().map(|p| p.len()).min().unwrap_or(0);
        assert!(
            longest - shortest <= 1,
            "at T={threads} the chunks are {shortest}..{longest} long, so one thread is measuring \
             a different amount of work"
        );
    }
}

/// Scaling efficiency refuses to invent a denominator.
///
/// RED against a derivation that divides by a missing T=1 figure and publishes
/// an infinity or a zero, both of which chart.
#[test]
fn scaling_efficiency_has_no_answer_without_the_control() {
    assert_eq!(
        concurrent_curve::scaling_efficiency(4, Some(400.0), Some(100.0)),
        Some(1.0)
    );
    assert_eq!(
        concurrent_curve::scaling_efficiency(4, Some(200.0), Some(100.0)),
        Some(0.5)
    );
    assert_eq!(
        concurrent_curve::scaling_efficiency(4, Some(200.0), None),
        None
    );
    assert_eq!(
        concurrent_curve::scaling_efficiency(4, None, Some(100.0)),
        None
    );
    assert_eq!(
        concurrent_curve::scaling_efficiency(4, Some(200.0), Some(0.0)),
        None
    );
}

// ---------------------------------------------------------------------------
// requests
// ---------------------------------------------------------------------------

/// The archive's request counts are observed and the tree's are declared.
///
/// RED against publishing both as measured, which is the single most misleading
/// thing this family could put on a page: nothing counts a `std::fs::read`, so
/// the tree's numbers come from the model "one whole object per tile" and have
/// to say so. The test is self-controlling: a hard-coded `Origin` fails one of
/// the two assertions whichever value it is hard-coded to.
#[test]
fn the_directory_request_count_is_declared_and_the_archive_count_is_observed() {
    let dir = tempdir();
    let cell = tiny(Source::Gradient);
    let archive = write_archive(dir.path(), &cell);
    let tree = write_tree(dir.path(), &cell);
    assert!(tree.is_dir(), "the tree backend really wrote a tree");

    let coords = cells::coordinates(&cell.plan().expect("the cell plans"));
    let walk = random_order(&coords, SEED);
    let archive_counts = requests::pmtiles(&CountingFactory::new(&archive), coords[0], None, &walk)
        .expect("the archive opens");
    let tree_counts = requests::directory(planned(&cell), 4_096, walk.len() as u64, false);

    assert!(
        requests::all_observed(&archive_counts),
        "every archive row came out of a counting range reader: {archive_counts:?}"
    );
    assert!(
        requests::all_declared(&tree_counts),
        "every tree row came out of the model: {tree_counts:?}"
    );

    let operations = |rows: &[requests::OperationCount]| {
        rows.iter().map(|row| row.operation).collect::<Vec<_>>()
    };
    assert_eq!(
        operations(&archive_counts),
        operations(&tree_counts),
        "the two backends have to break the same operations down, or the page compares two \
         different lists"
    );

    let open_row = &archive_counts[0];
    assert_eq!(open_row.operation, "open");
    assert_eq!(
        open_row.requests, 2,
        "an open is the header and the root: {open_row:?}"
    );
    assert_eq!(open_row.origin, Origin::Observed);
    assert!(open_row.bytes > 0);

    let walk_row = archive_counts
        .iter()
        .find(|row| row.operation == "random_walk")
        .expect("the walk is counted");
    assert_eq!(
        walk_row.requests,
        walk.len() as u64,
        "a root-only archive is one pread a tile: {walk_row:?}"
    );

    // A root-only archive performs no leaf lookup, so the leaf rows are absent
    // rather than zero: a zero there reads as a leaf lookup that cost nothing.
    assert!(
        !archive_counts
            .iter()
            .any(|row| row.operation.starts_with("leaf_")),
        "a root-only archive published a leaf row: {archive_counts:?}"
    );
    assert!(Origin::Declared.is_declared() && !Origin::Observed.is_declared());
}

// ---------------------------------------------------------------------------
// The models
// ---------------------------------------------------------------------------

/// A modelled cost follows the parameters it was handed and names them.
///
/// RED against a hard-coded round trip. The model is built with an rtt and a
/// bandwidth that are not the declared ones, so an implementation that reaches
/// for `DECLARED_RTT_MS` comes out with the declared answer instead of this
/// one.
#[test]
fn the_remote_model_uses_the_declared_parameters_and_names_them() {
    let model = RemoteModel {
        rtt_ms: 7.5,
        bandwidth_bytes_per_s: 1_048_576.0,
    };
    // Four round trips at 7.5 ms, plus 2 MiB at 1 MiB a second.
    assert_eq!(model.cost_ms(4, 2 * 1_048_576), 30.0 + 2_000.0);

    let modelled = model.modelled("remote_cost_ms", 4, 2 * 1_048_576);
    assert_eq!(modelled.unit, "ms");
    assert_eq!(modelled.value, 2_030.0);
    assert_eq!(
        modelled.parameter_names(),
        vec!["rtt_ms", "bandwidth_bytes_per_s"]
    );
    let rtt = modelled
        .parameter("rtt_ms")
        .expect("the model names its rtt");
    assert_eq!(
        rtt.value, 7.5,
        "the published value came from a declared 30"
    );
    assert_eq!(rtt.unit, "ms");

    // The declared model is a different answer, which is what makes the check
    // above a check rather than a coincidence.
    let declared = RemoteModel::declared().modelled("remote_cost_ms", 4, 2 * 1_048_576);
    assert_ne!(declared.value, modelled.value);
    assert_eq!(
        declared.parameter("rtt_ms").map(|p| p.value),
        Some(libviprs_bench::storage::model::DECLARED_RTT_MS)
    );

    // And a modelled number never shares an axis with a measured one.
    const { assert!(!Modelled::CHARTABLE_BESIDE_MEASURED) };
}

/// The sync model does the same, per filesystem entry.
///
/// RED against a per-file cost baked into the arithmetic.
#[test]
fn the_sync_model_uses_the_declared_parameters_and_names_them() {
    let model = SyncModel { per_file_ms: 0.25 };
    assert_eq!(model.cost_ms(1_000), 250.0);
    let modelled = model.modelled("sync_cost_ms", 1_000);
    assert_eq!(modelled.parameter_names(), vec!["per_file_ms"]);
    assert_eq!(
        modelled.parameter("per_file_ms").map(|p| p.value),
        Some(0.25)
    );
    assert_ne!(
        SyncModel::declared().cost_ms(1_000),
        model.cost_ms(1_000),
        "the declared per-file cost is a different number, so the test above is not a coincidence"
    );
}

// ---------------------------------------------------------------------------
// replicate
// ---------------------------------------------------------------------------

/// The replicate cell is measured through the sweep and publishes its own
/// dispersion.
///
/// RED against a schedule that measures it once, against one that measures it
/// only at the two ends, and against a block computed from a pair: the floor
/// then has no dispersion of its own, which is how two captures of one cell on
/// one host came out at 3.46% and 36.89% with nothing able to say which was the
/// outlier (#84). The estimator's own arithmetic lives in
/// `tests/replicate_estimator.rs`; what this one holds is the scenario's place
/// in the sweep and the fact the whole control exists for, which is that the
/// tail moves further than the median on an idle host running identical code.
#[test]
fn the_replicate_cell_is_measured_throughout_and_publishes_its_dispersion() {
    let control = cells::smoke_cell(Source::Gradient);
    let rest = [
        cells::mid_cell(Source::Gradient),
        cells::brink_cell(Source::Gradient),
        cells::leaf_cell(Source::Gradient),
    ];

    let schedule = replicate::schedule(control, &rest);
    assert_eq!(schedule.len(), 2 * rest.len() + 1);
    assert_eq!(replicate::placements(&schedule, control), rest.len() + 1);
    assert!(!replicate::has_adjacent_placements(&schedule, control));
    assert!(replicate::measured_first_and_last(&schedule, control));
    let measured: Vec<_> = schedule.iter().copied().filter(|c| *c != control).collect();
    assert_eq!(&measured[..], &rest[..]);

    // The positive control: a schedule that measures it only at the front is
    // not one this rule accepts, and neither is the two-ended one this lane
    // replaced.
    let once = {
        let mut cells = vec![control];
        cells.extend_from_slice(&rest);
        cells
    };
    assert!(!replicate::measured_first_and_last(&once, control));
    assert_eq!(replicate::placements(&once, control), 1);
    let two_ended = {
        let mut cells = once.clone();
        cells.push(control);
        cells
    };
    assert!(replicate::measured_first_and_last(&two_ended, control));
    assert!(
        replicate::placements(&two_ended, control) < replicate::MIN_REPLICATE_REPS,
        "the schedule this replaced holds two placements, which is below what a published \
         floor may rest on"
    );

    // Five placements of the p50 and the p99, the second moving as the free
    // replicate pair's p99 did.
    let measurements: Vec<BTreeMap<String, f64>> = [
        (8.0, 5.71),
        (8.4, 9.96),
        (8.1, 6.20),
        (8.3, 9.10),
        (7.9, 5.90),
    ]
    .iter()
    .map(|(p50, p99)| {
        BTreeMap::from([
            ("p50_us".to_string(), *p50),
            ("p99_us".to_string(), *p99),
        ])
    })
    .collect();
    let block = replicate::block(&control, &measurements).expect("five placements make a block");

    assert_eq!(block.reps, replicate::MIN_REPLICATE_REPS);
    assert_eq!(block.estimator.reps, block.reps);
    assert_eq!(block.cell, control.spec());
    assert_eq!(block.spread_pct.len(), 2);
    let p99 = block.spread_pct["p99_us"];
    let p50 = block.spread_pct["p50_us"];
    assert!(
        p99 > 50.0,
        "the tail on this host moves by tens of percent between placements of one cell \
         running identical code, and the floor says {p99}"
    );
    assert!(
        p50 < p99 / 5.0,
        "the median is an order steadier than the tail and the floor should show it: {p50} \
         against {p99}"
    );

    // A delta the floor covers is noise, and one it does not is not.
    assert!(replicate::covered_by_noise(&block, "p99_us", p99 - 0.001));
    assert!(!replicate::covered_by_noise(&block, "p99_us", p99 + 0.001));
    assert!(!replicate::covered_by_noise(&block, "p50_us", p99 - 0.001));

    // Fewer placements than a floor may rest on are refused rather than
    // published as a narrower floor.
    let refusal = replicate::block(&control, &measurements[..2])
        .expect_err("a block over two placements is not a dispersion");
    assert!(refusal.contains(&control.spec()));
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// A scratch directory that cleans up after itself.
///
/// `std::env::temp_dir()` plus the process id, the way every other test in this
/// crate does it, rather than a new dependency for four tests. The archives
/// here run to tens of megabytes on the `--ignored` cells, so the `Drop` is not
/// a nicety.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "libviprs_bench_storage_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn tempdir() -> Scratch {
    Scratch::new()
}

/// A seeded shuffle, the same Fisher-Yates `storage::coordinate_sets` runs, so
/// the T=1 control walks `read_random`'s sequence rather than a lookalike.
fn random_order(coords: &[TileCoord], seed: u64) -> Vec<TileCoord> {
    let mut out = coords.to_vec();
    let mut rng = libviprs_bench::storage::stats::Splitmix::new(seed);
    for i in (1..out.len()).rev() {
        let j = rng.below(i + 1);
        out.swap(i, j);
    }
    out
}

fn median_micros(samples: &[std::time::Duration]) -> f64 {
    let mut micros: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1e6).collect();
    micros.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in a duration"));
    micros[micros.len() / 2]
}

// ---------------------------------------------------------------------------
// A source's period
// ---------------------------------------------------------------------------

/// The ported gradient does not collapse at any tile size the sweep uses.
///
/// RED against a gradient reconstructed with power-of-two moduli, which is what
/// I wrote before I read the engine's. The engine's is
/// `(x % 251, y % 241, (x * 7 + y * 13) % 239)`: three primes, so the smallest
/// tile that could make two neighbouring tiles byte-identical is 251 * 241 * 239
/// pixels wide and no sweep comes near it. A `% 256` version repeats every 256
/// pixels, the whole level becomes one contiguous run of identical payloads, and
/// the writer's run-length encoding merges it into a single entry.
///
/// The positive control is in the same test: the periodic ramp *does* collapse
/// at 256, so the guard is shown firing rather than merely not firing.
#[test]
fn the_ported_gradient_does_not_collapse_at_any_tile_size_the_sweep_uses() {
    // The arithmetic, first, because it is what the generated archives below
    // are supposed to confirm.
    for tile_px in [256u32, 128, 64, 46] {
        assert!(
            !cells::collapses_at(Source::Gradient, tile_px),
            "the ported gradient would collapse at a {tile_px} pixel tile, which means its \
             moduli are not the engine's"
        );
        assert!(!cells::collapses_at(Source::Noise, tile_px));
        assert!(
            cells::collapses_at(Source::Flat, tile_px),
            "a solid fill repeats every pixel, so it collapses at every tile size"
        );
    }
    assert!(
        cells::collapses_at(Source::PeriodicGradient, 256),
        "the control has to collapse at 256, or this test cannot tell a guard that fires from one \
         that cannot"
    );
    assert!(!cells::collapses_at(Source::PeriodicGradient, 46));
    assert_eq!(cells::period_px(Source::Gradient), Some(251 * 241 * 239));

    // And the archives agree with the arithmetic.
    let dir = tempdir();
    let at_256 = tiny(Source::Gradient);
    let levels = at_256.plan().expect("the cell plans").levels.len() as u64;
    let (entries, _) =
        cells::root_shape(&write_archive(dir.path(), &at_256)).expect("the archive opens");
    println!(
        "{} plans {} tiles over {levels} levels and its root holds {entries} entries",
        at_256.spec(),
        planned(&at_256)
    );
    assert_eq!(
        entries,
        planned(&at_256),
        "the ported gradient at a 256 pixel tile paid {entries} entries for {} tiles, so something \
         deduplicated and the moduli are not prime any more",
        planned(&at_256)
    );

    // The control, at the same cell, really does collapse to one entry a level.
    let periodic = tiny(Source::PeriodicGradient);
    let (collapsed, _) =
        cells::root_shape(&write_archive(dir.path(), &periodic)).expect("the archive opens");
    println!(
        "{} plans {} tiles over {levels} levels and its root holds {collapsed} entries",
        periodic.spec(),
        planned(&periodic)
    );
    assert_eq!(
        collapsed, levels,
        "the periodic ramp pays one entry a level, and this archive paid {collapsed} over {levels} \
         levels; without that the test above cannot fail"
    );
}

/// Every source's root entry count, measured rather than assumed.
///
/// RED against a cell table that reads a root-entry count off a tile count. It
/// prints the table the module documents, so the numbers in the docs came out of
/// a run rather than out of a head.
#[test]
fn the_measured_root_entries_of_every_source() {
    let dir = tempdir();
    println!("cell\tplanned\tgradient\tnoise\tperiodic\tflat");
    for (width, height, tile_px) in [
        (1024u32, 1024u32, 256u32),
        (1024, 1024, 128),
        (1024, 1024, 64),
        (1024, 1024, 46),
        (2048, 2048, 256),
    ] {
        let base = cell_at(width, height, tile_px, Source::Gradient);
        let mut counts = Vec::new();
        for source in SOURCES {
            let cell = cell_at(width, height, tile_px, source);
            let (entries, leaves) =
                cells::root_shape(&write_archive(dir.path(), &cell)).expect("the archive opens");
            assert_eq!(leaves, 0, "{} grew leaves at this scale", cell.spec());
            counts.push((source, entries));
        }
        println!(
            "{}x{}@{}\t{}\t{}\t{}\t{}\t{}",
            width,
            height,
            tile_px,
            planned(&base),
            counts[0].1,
            counts[1].1,
            counts[3].1,
            counts[2].1
        );

        let planned = planned(&base);
        for (source, entries) in &counts {
            match source {
                // The two sources the sweep publishes pay one entry a tile at
                // every tile size, which is the claim the ramp's x axis rests
                // on.
                Source::Gradient | Source::Noise => assert_eq!(
                    *entries,
                    planned,
                    "{}x{}@{} from {} paid {entries} entries for {planned} tiles",
                    width,
                    height,
                    tile_px,
                    source.as_str()
                ),
                // The controls collapse, and they have to, or nothing above is
                // a distinction.
                Source::Flat => assert!(*entries < planned),
                Source::PeriodicGradient => {
                    if cells::collapses_at(Source::PeriodicGradient, tile_px) {
                        assert!(*entries < planned);
                    }
                }
            }
        }
    }
}

/// A cell table may not claim a root the source at that tile size cannot give.
///
/// RED against a table that pairs any source with any tile size. With the
/// engine's gradient the rule never fires on a cell in this family, which is why
/// the positive control is the periodic ramp: the guard is shown refusing
/// something, so a green run is evidence rather than silence.
#[test]
fn the_cell_table_refuses_a_source_at_a_tile_size_that_collapses_it() {
    let refusal = cells::source_suits_the_cell(&cells::smoke_cell(Source::PeriodicGradient))
        .expect_err("a 256 pixel tile collapses a ramp whose period is 256");
    assert!(
        refusal.contains("256") && refusal.contains("run-length"),
        "the refusal has to name the period and the mechanism: {refusal}"
    );
    cells::source_suits_the_cell(&cells::mid_cell(Source::Flat))
        .expect_err("a solid fill collapses at every tile size");

    // The engine's gradient suits every cell in the family, which is the point
    // of porting it rather than reconstructing it.
    for cell in [
        cells::smoke_cell(Source::Gradient),
        cells::mid_cell(Source::Gradient),
        cells::brink_cell(Source::Gradient),
        cells::leaf_cell(Source::Gradient),
    ] {
        cells::source_suits_the_cell(&cell)
            .unwrap_or_else(|why| panic!("the ported gradient suits every cell: {why}"));
    }
    for cell in [
        cells::smoke_cell(Source::Noise),
        cells::leaf_cell(Source::Noise),
    ] {
        cells::source_suits_the_cell(&cell).expect("noise never repeats");
    }
}

/// The gradient compiled into this binary is the engine's, pixel for pixel.
///
/// RED against a stale build. Every entry count this lane publishes is a
/// property of the source the running binary actually contains, and
/// `Compiling libviprs-bench` in a build log is a weaker witness than the
/// pixels: a revert that lands inside one filesystem timestamp leaves cargo
/// calling the target fresh, and then a clean-looking run reports the previous
/// source's numbers. This asks the bytes.
///
/// The probes are computed for the same reason the tile-id probe is. `x % 251`
/// and `x % 256` agree for every `x` under 251, so a coordinate picked by eye
/// lands on a value both formulas produce and the test cannot fail against the
/// reconstruction it exists to catch. So it builds both rasters, takes the set
/// of offsets where they disagree, asserts that set is not empty, and checks a
/// member of it.
#[test]
fn the_gradient_in_this_binary_has_the_engines_prime_moduli() {
    const SIDE: u32 = 300;
    let engine = raster(Source::Gradient, SIDE, SIDE);
    let rounded = raster(Source::PeriodicGradient, SIDE, SIDE);
    let engine_bytes = engine.data();
    let rounded_bytes = rounded.data();
    assert_eq!(engine_bytes.len(), rounded_bytes.len());

    let disagree: Vec<usize> = engine_bytes
        .iter()
        .zip(rounded_bytes)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(index, _)| index)
        .collect();
    assert!(
        !disagree.is_empty(),
        "the prime-moduli gradient and the 256 reconstruction produce identical pixels over \
         {SIDE}x{SIDE}, so nothing below can tell them apart"
    );

    let probe = disagree[0];
    let pixel = probe / 3;
    let channel = probe % 3;
    let (x, y) = (
        (pixel % SIDE as usize) as u32,
        (pixel / SIDE as usize) as u32,
    );
    println!(
        "{} of {} bytes differ; first at pixel ({x}, {y}) channel {channel}: engine {} against a \
         256 reconstruction's {}",
        disagree.len(),
        engine_bytes.len(),
        engine_bytes[probe],
        rounded_bytes[probe]
    );

    let expected = [
        (x % 251) as u8,
        (y % 241) as u8,
        ((x * 7 + y * 13) % 239) as u8,
    ];
    assert_eq!(
        engine_bytes[probe], expected[channel],
        "at the one probe where the two formulas disagree, this binary's gradient is not the \
         engine's"
    );
    assert_ne!(
        engine_bytes[probe], rounded_bytes[probe],
        "the probe has to be a byte the reconstruction gets wrong"
    );

    // And the whole raster follows the engine's formula, not just the probe.
    for y in 0..SIDE {
        for x in 0..SIDE {
            let off = (y as usize * SIDE as usize + x as usize) * 3;
            assert_eq!(engine_bytes[off], (x % 251) as u8);
            assert_eq!(engine_bytes[off + 1], (y % 241) as u8);
            assert_eq!(engine_bytes[off + 2], ((x * 7 + y * 13) % 239) as u8);
        }
    }
}
