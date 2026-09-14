//! A [`ReaderFactory`] that serves a real archive and remembers what was asked
//! for.
//!
//! The remote-storage question is "how many round trips and how many bytes",
//! and the crate has no HTTP range reader, so nothing in a normal run can
//! answer it. `pmtiles::Reader::try_new` is public and generic over
//! `RangeReader`, though, so putting a counting source under it is the whole
//! trick. The engine's own suite already has two wrappers of this shape; this
//! is the third and the first that lives where a benchmark can use it.
//!
//! # It is a factory, not a reader a scenario builds
//!
//! K1.2's rule is that no scenario constructs a reader and every scenario asks
//! [`ReaderFactory::fresh`]. That seam is what makes the isolation proof work,
//! and counting fits behind it rather than around it: the counting scenarios
//! are handed one of these instead of the ordinary factory, and `fresh()` is
//! exactly the call they are measuring. A scenario that reached past the seam
//! to open its own archive would also be a scenario that could accidentally
//! reuse a warm one.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use libviprs::planner::TileCoord;
use libviprs::pmtiles::{FileRangeReader, RangeReader, Reader};

use super::{ReaderFactory, TileReader};

/// One range read the reader asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    pub offset: u64,
    pub len: usize,
}

impl Request {
    pub fn end(&self) -> u64 {
        self.offset + self.len as u64
    }
}

#[derive(Debug)]
struct Inner {
    file: FileRangeReader,
    requests: Mutex<Vec<Request>>,
}

/// A file-backed byte source that records every range it serves.
///
/// Cheaply cloneable, because `Reader::try_new` takes its source by value and
/// the caller still needs to read the counts back. Every clone shares one log.
#[derive(Debug, Clone)]
pub struct CountingSource {
    inner: Arc<Inner>,
}

impl CountingSource {
    pub fn try_open(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            inner: Arc::new(Inner {
                file: FileRangeReader::try_open(path)
                    .map_err(|error| io::Error::other(error.to_string()))?,
                requests: Mutex::new(Vec::new()),
            }),
        })
    }

    /// Every request so far, oldest first.
    pub fn requests(&self) -> Vec<Request> {
        self.locked().clone()
    }

    pub fn count(&self) -> u64 {
        self.locked().len() as u64
    }

    pub fn bytes(&self) -> u64 {
        self.locked().iter().map(|r| r.len as u64).sum()
    }

    /// Forget everything recorded so far, so the next operation is counted on
    /// its own.
    pub fn forget(&self) {
        self.locked().clear();
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Vec<Request>> {
        self.inner
            .requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl RangeReader for CountingSource {
    fn read_range(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.locked().push(Request { offset, len });
        self.inner.file.read_range(offset, len)
    }

    fn size(&self) -> io::Result<Option<u64>> {
        // Deliberately not counted. `size()` is a `stat`, not a range read, and
        // a remote store answers it out of the object's metadata rather than
        // with a ranged GET. Counting it would put a request in the column that
        // prices round trips for a call that does not make one.
        self.inner.file.size()
    }
}

/// A tile lookup through a counted archive.
pub struct CountingReader {
    reader: Reader<CountingSource>,
    source: CountingSource,
    /// What building this reader cost, before any lookup.
    open: Vec<Request>,
    tile_data: (u64, u64),
    root_entries: u64,
}

impl CountingReader {
    /// The requests `Reader::try_new` made.
    pub fn open_requests(&self) -> &[Request] {
        &self.open
    }

    pub fn root_entries(&self) -> u64 {
        self.root_entries
    }

    /// Where the archive's tile data section starts, and how long it is.
    pub fn tile_data_range(&self) -> (u64, u64) {
        self.tile_data
    }

    /// The requests that touched the tile data section.
    pub fn in_tile_section(&self, requests: &[Request]) -> Vec<Request> {
        let (start, len) = self.tile_data;
        let end = start + len;
        requests
            .iter()
            .copied()
            .filter(|r| r.offset < end && r.end() > start)
            .collect()
    }

    /// Count one operation on its own: forget, run it, report.
    pub fn counted<T>(&self, body: impl FnOnce(&Self) -> T) -> (T, Vec<Request>) {
        self.source.forget();
        let out = body(self);
        (out, self.source.requests())
    }
}

impl TileReader for CountingReader {
    fn tile(&self, coord: TileCoord) -> Result<Option<Vec<u8>>, String> {
        let z =
            u8::try_from(coord.level).map_err(|_| "a level PMTiles cannot address".to_string())?;
        self.reader
            .get_tile(z, coord.col, coord.row)
            .map_err(|error| error.to_string())
    }
}

/// Hands out readers that count, one archive, one fresh reader per call.
#[derive(Debug, Clone)]
pub struct CountingFactory {
    archive: PathBuf,
}

impl CountingFactory {
    pub fn new(archive: impl Into<PathBuf>) -> Self {
        Self {
            archive: archive.into(),
        }
    }

    /// A counted reader, typed, for the scenarios that need the counts rather
    /// than just the lookups.
    pub fn fresh_counting(&self) -> Result<Arc<CountingReader>, String> {
        let source = CountingSource::try_open(&self.archive).map_err(|e| e.to_string())?;
        let reader = Reader::try_new(source.clone()).map_err(|e| e.to_string())?;
        let header = reader.header();
        let tile_data = (header.tile_data_offset, header.tile_data_length);
        let root_entries = reader.root_entries().len() as u64;
        let open = source.requests();
        source.forget();
        Ok(Arc::new(CountingReader {
            reader,
            source,
            open,
            tile_data,
            root_entries,
        }))
    }
}

impl ReaderFactory for CountingFactory {
    fn fresh(&self) -> Result<Arc<dyn TileReader>, String> {
        Ok(self.fresh_counting()?)
    }
}
