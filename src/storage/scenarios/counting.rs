//! A [`RangeReader`] that serves a real archive and remembers what was asked
//! for.
//!
//! The remote-storage question is "how many round trips and how many bytes",
//! and the crate has no HTTP range reader, so nothing in a normal run can
//! answer it. `pmtiles::Reader::try_new` is public and generic over
//! `RangeReader`, though, so wrapping the file source and counting is the whole
//! trick. The engine's own suite already has two wrappers of this shape
//! (`tests/pmtiles_index_only_reads.rs` and `tests/pmtiles_lock_probe.rs`);
//! this is the third and the first one that lives where a benchmark can use it.
//!
//! It wraps the real [`FileRangeReader`] rather than fabricating an archive,
//! because the counts a benchmark publishes have to come from the archive the
//! same sweep generated.

use std::io;
use std::path::Path;
use std::sync::Mutex;

use libviprs::pmtiles::{FileRangeReader, RangeReader};

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

/// A file-backed byte source that records every range it serves.
#[derive(Debug)]
pub struct CountingSource {
    inner: FileRangeReader,
    requests: Mutex<Vec<Request>>,
}

impl CountingSource {
    pub fn try_open(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            inner: FileRangeReader::try_open(path)
                .map_err(|error| io::Error::other(error.to_string()))?,
            requests: Mutex::new(Vec::new()),
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
    ///
    /// A reset rather than a fresh reader, because the point of the warm-leaf
    /// operation is that it runs against a reader that has already resolved
    /// that leaf once.
    pub fn forget(&self) {
        self.locked().clear();
    }

    /// The requests that landed inside `[start, start + len)`.
    ///
    /// Used to answer "did the open touch the tile data section", which is the
    /// assertion that an open reads the index and nothing else.
    pub fn requests_within(&self, start: u64, len: u64) -> Vec<Request> {
        let end = start + len;
        self.locked()
            .iter()
            .copied()
            .filter(|r| r.offset < end && r.end() > start)
            .collect()
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Vec<Request>> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl RangeReader for CountingSource {
    fn read_range(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.locked().push(Request { offset, len });
        self.inner.read_range(offset, len)
    }

    fn size(&self) -> io::Result<Option<u64>> {
        // Deliberately not counted. `size()` is a `stat`, not a range read, and
        // a remote store answers it out of the object's metadata rather than
        // with a ranged GET. Counting it would put a request in the column that
        // prices round trips for a call that does not make one.
        self.inner.size()
    }
}
