//! Isolation and repetition: the two things the harness this replaces does not
//! have.
//!
//! Every test here names, in a comment, the wrong implementation it goes red
//! against.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use libviprs::planner::TileCoord;
use libviprs::pmtiles::directory::serialize_entries;
use libviprs::pmtiles::{Compression, Entry, Header, RangeReader, Reader, TileType, zxy_to_tileid};

use libviprs_bench::storage::cells::{Backend, Cell, Profile, SEED, Source};
use libviprs_bench::storage::scenarios::reference::{Generate, ReadPass};
use libviprs_bench::storage::scenarios::{
    Coordinates, ReaderFactory, Scenario, ScenarioContext, TileReader,
};
use libviprs_bench::storage::{FileReaderFactory, Scratch, write_pyramid};

// ---------------------------------------------------------------------------
// A fabricated leaf-bearing archive, and a source that remembers every request
// ---------------------------------------------------------------------------
//
// Fabricated rather than generated. A real leaf-bearing archive needs more
// entries than `ROOT_ONLY_MAX_ENTRIES`, which is 16384, and the cheapest cell
// that reaches it is 8192x8192 at 64 pixel tiles: 21851 tiles and a couple of
// seconds per generation. The question here is about which reader answered a
// lookup, not about the pixels behind it, so the archive is four tiles, two
// addressed from the root and two behind a leaf directory, and it costs
// nothing.

const ZOOM: u8 = 10;
const ROOT_OFFSET: u64 = 127;
const METADATA_OFFSET: u64 = 8_192;
const LEAF_OFFSET: u64 = 12_288;
const TILE_DATA_OFFSET: u64 = 65_536;
const TILE_DATA_LENGTH: u64 = 1 << 20;
const METADATA: &[u8] = br#"{"name":"fabricated","vector_layers":[]}"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Request {
    offset: u64,
    len: usize,
}

impl Request {
    fn overlaps(&self, start: u64, length: u64) -> bool {
        let end = self.offset + self.len as u64;
        self.offset < start + length && start < end
    }
}

/// A byte source that serves a fabricated archive and remembers what was asked
/// for.
struct Counting {
    segments: Vec<(u64, Vec<u8>)>,
    size: u64,
    requests: Mutex<Vec<Request>>,
}

impl Counting {
    fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn forget(&self) {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    fn read(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Request { offset, len });
        let end = offset.checked_add(len as u64).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "offset + len overflowed")
        })?;
        if end > self.size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("{offset}..{end} runs past the {} byte archive", self.size),
            ));
        }
        for (start, bytes) in &self.segments {
            let seg_end = start + bytes.len() as u64;
            if offset >= *start && end <= seg_end {
                let from = (offset - start) as usize;
                return Ok(bytes[from..from + len].to_vec());
            }
        }
        if offset >= TILE_DATA_OFFSET {
            return Ok((offset..end).map(synthetic_byte).collect());
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{offset}..{end} is not a section this archive holds"),
        ))
    }
}

/// The byte a synthetic tile at `offset` carries: `splitmix64`'s finaliser, so
/// every bit of the offset reaches the byte and bytes fetched from the wrong
/// place do not happen to match.
fn synthetic_byte(offset: u64) -> u8 {
    let mut z = offset.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31)) as u8
}

struct Handle(Arc<Counting>);

impl RangeReader for Handle {
    fn read_range(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.0.read(offset, len)
    }

    fn size(&self) -> io::Result<Option<u64>> {
        Ok(Some(self.0.size))
    }
}

struct ArchiveReader(Reader<Handle>);

impl TileReader for ArchiveReader {
    fn tile(&self, coord: TileCoord) -> Result<Option<Vec<u8>>, String> {
        self.0
            .get_tile(coord.level as u8, coord.col, coord.row)
            .map_err(|e| e.to_string())
    }
}

/// The seam. Every call opens a reader that has never served a lookup, which
/// re-reads the header and the root and holds no leaf directory.
struct CountingFactory {
    source: Arc<Counting>,
}

impl ReaderFactory for CountingFactory {
    fn fresh(&self) -> Result<Arc<dyn TileReader>, String> {
        let reader = Reader::try_new(Handle(self.source.clone()))
            .map_err(|e| format!("the fabricated archive does not open: {e}"))?;
        Ok(Arc::new(ArchiveReader(reader)))
    }
}

struct Fabricated {
    source: Arc<Counting>,
    from_root: [TileCoord; 2],
    from_leaf: [TileCoord; 2],
    leaf_length: u64,
}

fn coord(x: u32, y: u32) -> TileCoord {
    TileCoord {
        level: u32::from(ZOOM),
        col: x,
        row: y,
    }
}

fn fabricate() -> Fabricated {
    let placed = [(1u32, 1u32, 0u64, 512u32), (2, 2, 4_096, 700)];
    let behind_leaf = [
        (300u32, 300u32, 8_192u64, 1_024u32),
        (301, 301, 16_384, 256),
    ];

    let mut all: Vec<(u32, u32, u64, u32, u64)> = placed
        .iter()
        .chain(behind_leaf.iter())
        .map(|&(x, y, off, len)| {
            (
                x,
                y,
                off,
                len,
                zxy_to_tileid(ZOOM, x, y).expect("inside the zoom's grid"),
            )
        })
        .collect();
    // PMTiles orders a zoom by its Hilbert curve, so which two end up in the
    // root is decided by the tile ids and not by the coordinates.
    all.sort_by_key(|t| t.4);
    let root_tiles = [all[0], all[1]];
    let leaf_tiles = [all[2], all[3]];
    assert!(
        root_tiles[1].4 < leaf_tiles[0].4,
        "the four tile ids should be distinct and ordered"
    );

    let leaf_entries: Vec<Entry> = leaf_tiles
        .iter()
        .map(|t| Entry {
            tile_id: t.4,
            offset: t.2,
            length: t.3,
            run_length: 1,
        })
        .collect();
    let leaf_bytes = Compression::None
        .compress(&serialize_entries(&leaf_entries).expect("the leaf serialises"))
        .expect("no compression is the identity");

    let mut root_entries: Vec<Entry> = root_tiles
        .iter()
        .map(|t| Entry {
            tile_id: t.4,
            offset: t.2,
            length: t.3,
            run_length: 1,
        })
        .collect();
    root_entries.push(Entry {
        tile_id: leaf_tiles[0].4,
        offset: 0,
        length: u32::try_from(leaf_bytes.len()).expect("a leaf this small fits a u32"),
        // Zero run length is what makes an entry a leaf pointer.
        run_length: 0,
    });
    let root_bytes = Compression::None
        .compress(&serialize_entries(&root_entries).expect("the root serialises"))
        .expect("no compression is the identity");

    let header = Header {
        root_offset: ROOT_OFFSET,
        root_length: root_bytes.len() as u64,
        metadata_offset: METADATA_OFFSET,
        metadata_length: METADATA.len() as u64,
        leaf_directories_offset: LEAF_OFFSET,
        leaf_directories_length: leaf_bytes.len() as u64,
        tile_data_offset: TILE_DATA_OFFSET,
        tile_data_length: TILE_DATA_LENGTH,
        addressed_tiles_count: 4,
        tile_entries_count: 4,
        tile_contents_count: 4,
        clustered: true,
        internal_compression: Compression::None,
        tile_compression: Compression::None,
        tile_type: TileType::Png,
        min_zoom: ZOOM,
        max_zoom: ZOOM,
        ..Header::default()
    };

    let leaf_length = leaf_bytes.len() as u64;
    let source = Counting {
        segments: vec![
            (0, header.encode().to_vec()),
            (ROOT_OFFSET, root_bytes),
            (METADATA_OFFSET, METADATA.to_vec()),
            (LEAF_OFFSET, leaf_bytes),
        ],
        size: TILE_DATA_OFFSET + TILE_DATA_LENGTH,
        requests: Mutex::new(Vec::new()),
    };

    Fabricated {
        source: Arc::new(source),
        from_root: [
            coord(root_tiles[0].0, root_tiles[0].1),
            coord(root_tiles[1].0, root_tiles[1].1),
        ],
        from_leaf: [
            coord(leaf_tiles[0].0, leaf_tiles[0].1),
            coord(leaf_tiles[1].0, leaf_tiles[1].1),
        ],
        leaf_length,
    }
}

// ---------------------------------------------------------------------------

/// RED against reusing the reader `read_plan_order` warmed, which is what the
/// harness in the engine repository does: one reader per cell, shared by every
/// read scenario, so `read_random` walks a reader `read_sequential` has just
/// warmed and the row is published as a cold-ish random read.
///
/// The witness is a range read of the leaf directory. On a leaf-bearing
/// archive a leaf-addressed lookup costs a directory fetch on a reader that
/// has not seen that leaf and costs nothing on one that has, which is most of
/// the difference between the two readings.
#[test]
fn every_read_scenario_runs_in_its_own_process_on_a_fresh_reader() {
    let fab = fabricate();
    let factory = CountingFactory {
        source: fab.source.clone(),
    };
    let leaf = fab.from_leaf[0];
    let root = fab.from_root[0];

    // The control for the observation itself. A fresh reader fetches the leaf
    // directory; the same reader asked again does not. Without this, an
    // implementation that never fetched a leaf at all would look the same as
    // one that always did, and the assertion below would prove nothing.
    {
        let reader = factory.fresh().expect("a reader opens");
        fab.source.forget();
        reader.tile(leaf).expect("a leaf lookup succeeds");
        assert!(
            fab.source
                .requests()
                .iter()
                .any(|r| r.overlaps(LEAF_OFFSET, fab.leaf_length)),
            "a reader that has never seen this leaf has to fetch the directory"
        );
        fab.source.forget();
        reader.tile(leaf).expect("a second leaf lookup succeeds");
        assert!(
            !fab.source
                .requests()
                .iter()
                .any(|r| r.overlaps(LEAF_OFFSET, fab.leaf_length)),
            "the same reader caches the leaf, so the observation discriminates"
        );
    }

    let coords = Coordinates {
        plan_order: vec![root, leaf, fab.from_root[1], fab.from_leaf[1]],
        tileid_order: Vec::new(),
        random: vec![leaf, root, fab.from_leaf[1]],
        root_addressed: Some(root),
        leaf_addressed: Some(leaf),
    };
    let cell = Cell::new(2048, 2048, 256, Source::Gradient, 93);
    let ctx = ScenarioContext {
        backend: Backend::PmTiles,
        cell,
        profile: Profile::Ci,
        seed: SEED,
        scratch_root: None,
        artefact: None,
        coords: &coords,
        readers: &factory,
    };

    // The warmed scenario first, exactly as a sweep runs them.
    ReadPass::plan_order()
        .run(&ctx, 1)
        .expect("read_plan_order runs");
    assert!(
        fab.source
            .requests()
            .iter()
            .any(|r| r.overlaps(LEAF_OFFSET, fab.leaf_length)),
        "read_plan_order should itself have gone through the leaf directory"
    );

    fab.source.forget();
    ReadPass::random().run(&ctx, 1).expect("read_random runs");
    let requests = fab.source.requests();

    assert!(
        requests
            .iter()
            .any(|r| r.overlaps(LEAF_OFFSET, fab.leaf_length)),
        "read_random's first leaf-addressed lookup issued no directory read, so \
         it ran on the reader read_plan_order warmed"
    );
    assert!(
        requests.iter().any(|r| r.offset == 0),
        "and a reader that has never been used re-reads the header"
    );
}

// ---------------------------------------------------------------------------

/// A factory that counts what a scenario asked it for.
///
/// Two numbers, and both of them matter: how many readers the scenario opened,
/// and how many lookups it made. The second is what proves a warm-up pass
/// happened, because a pass is a fixed number of lookups and the arithmetic is
/// exact.
struct CountingFile {
    inner: FileReaderFactory,
    readers: Arc<AtomicU64>,
    lookups: Arc<AtomicU64>,
}

struct CountingReader {
    inner: Arc<dyn TileReader>,
    lookups: Arc<AtomicU64>,
}

impl TileReader for CountingReader {
    fn tile(&self, coord: TileCoord) -> Result<Option<Vec<u8>>, String> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        self.inner.tile(coord)
    }
}

impl ReaderFactory for CountingFile {
    fn fresh(&self) -> Result<Arc<dyn TileReader>, String> {
        self.readers.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(CountingReader {
            inner: self.inner.fresh()?,
            lookups: self.lookups.clone(),
        }))
    }
}

/// A cell small enough that three whole regenerations are a unit test.
fn tiny() -> Cell {
    let mut cell = Cell::new(512, 512, 256, Source::Gradient, 0);
    cell.declared_tiles = cell.planned_tiles().expect("a plan") as u32;
    cell
}

/// RED against a harness that generates once and times nothing seven times.
///
/// Seven samples of one event is not a dispersion figure, it is a measurement
/// of the clock. The witness is the scratch directory: every repetition gets
/// its own, the name is never reused, and a repetition that reports a path an
/// earlier one already reported did not regenerate anything.
#[test]
fn generate_reps_regenerate_and_the_artefact_digest_does_not_move() {
    let scratch = Scratch::new(None).expect("a scratch root");
    let cell = tiny();
    let coords = Coordinates::default();

    for backend in Backend::ALL {
        let ctx = ScenarioContext {
            backend,
            cell,
            profile: Profile::Ci,
            seed: SEED,
            scratch_root: Some(scratch.path()),
            artefact: None,
            coords: &coords,
            readers: &libviprs_bench::storage::NoReaders,
        };
        let run = Generate
            .run(&ctx, 3)
            .unwrap_or_else(|e| panic!("{} generates: {}", backend.as_str(), e.reason));

        assert_eq!(run.reps.len(), 3, "three repetitions, three sets of facts");
        assert_eq!(
            run.series[0].samples.len(),
            3,
            "and three timed samples, one per repetition"
        );

        let paths: Vec<PathBuf> = run
            .reps
            .iter()
            .map(|r| {
                r.scratch
                    .clone()
                    .expect("a repetition records where it built")
            })
            .collect();
        for (i, path) in paths.iter().enumerate() {
            for other in &paths[i + 1..] {
                assert_ne!(
                    path,
                    other,
                    "{}: two repetitions built into the same directory, so one of \
                     them did not regenerate",
                    backend.as_str()
                );
            }
        }

        let first = &run.reps[0].invariants;
        assert!(
            first.filesystem_entries.is_some(),
            "{}: the entry count is the column this comparison is about and it \
             was not measured",
            backend.as_str()
        );
        assert!(first.artefact_digest.is_some());
        for rep in &run.reps {
            assert_eq!(
                rep.invariants.filesystem_entries,
                first.filesystem_entries,
                "{}: the entry count moved between two repetitions of one commit, \
                 which is a defect and never noise",
                backend.as_str()
            );
            assert_eq!(
                rep.invariants.artefact_digest,
                first.artefact_digest,
                "{}: the artefact digest moved between repetitions",
                backend.as_str()
            );
            assert_eq!(rep.invariants.tiles_produced, first.tiles_produced);
        }
    }
}

/// RED against measuring the first pass.
///
/// The first pass over a coordinate set on a fresh reader pays for every
/// directory the archive holds and for every page the kernel has not faulted
/// in, and folding it into `samples` puts a different event in with the rest.
/// It is run, thrown away, and recorded as thrown away.
#[test]
fn pass_scenarios_discard_one_warmup_and_record_it() {
    let scratch = Scratch::new(None).expect("a scratch root");
    let cell = tiny();
    let plan = cell.plan().expect("a plan");
    let written = write_pyramid(Backend::PmTiles, cell, &plan, scratch.path())
        .unwrap_or_else(|e| panic!("the fixture archive writes: {e}"));

    let coords = libviprs_bench::storage::coordinate_sets(&plan, Profile::Ci, SEED);
    let readers = Arc::new(AtomicU64::new(0));
    let lookups = Arc::new(AtomicU64::new(0));
    let factory = CountingFile {
        inner: FileReaderFactory::new(Backend::PmTiles, &written.output, &plan),
        readers: readers.clone(),
        lookups: lookups.clone(),
    };
    let ctx = ScenarioContext {
        backend: Backend::PmTiles,
        cell,
        profile: Profile::Ci,
        seed: SEED,
        scratch_root: Some(scratch.path()),
        artefact: Some(&written.output),
        coords: &coords,
        readers: &factory,
    };

    let scenario = ReadPass::plan_order();
    let warmup = scenario
        .warmup()
        .expect("a pass scenario declares what it discards");
    assert_eq!(warmup.policy, "one-discarded-pass");
    assert_eq!(warmup.passes, 1);

    let run = scenario
        .run(&ctx, 4)
        .unwrap_or_else(|e| panic!("read_plan_order runs: {}", e.reason));

    assert_eq!(
        run.discarded_warmup.len(),
        1,
        "the warm-up pass was not run, or was not recorded as discarded"
    );
    assert_eq!(
        run.series[0].samples.len(),
        4,
        "four timed repetitions, and the warm-up is not one of them"
    );

    // The witness that the warm-up pass really ran and really is not one of
    // the four. A pass is exactly `coords.len()` lookups, so five passes'
    // worth of lookups behind four samples is a discarded pass, and four is
    // not.
    //
    // This is a count and not a comparison of the discarded value against the
    // samples, which is what the issue asks for in words. On this cell a pass
    // is five lookups and the p50 of five is one of them, the clock ticks at
    // about forty nanoseconds, and two passes land on the same p50 often
    // enough that the comparison failed on its second run. A coincidence test
    // is not a property test.
    let per_pass = coords.plan_order.len() as u64;
    assert_eq!(
        lookups.load(Ordering::SeqCst),
        per_pass * 5,
        "five passes' worth of lookups: one discarded, four measured"
    );
    assert_eq!(
        readers.load(Ordering::SeqCst),
        1,
        "and all five happened on one reader the scenario opened itself"
    );

    // A scenario whose repetitions each own a process has nothing for a
    // discarded pass to warm, and says so rather than declaring a policy it
    // does not follow.
    assert!(Generate.warmup().is_none());
}
