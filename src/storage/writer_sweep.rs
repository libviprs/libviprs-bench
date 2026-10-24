//! The PMTiles writer sweep (libviprs-bench #132).
//!
//! Skeleton: the contract in `tests/pmtiles_writer_sweep.rs` is written first
//! and is red against this. Nothing here measures anything yet.

use super::cells::Profile;

/// The file a writer sweep writes, inside the storage family's report directory.
pub const DOCUMENT_NAME: &str = "writer-results.json";

/// The backend label go-pmtiles rows carry.
pub const GO_PMTILES: &str = "go-pmtiles";

/// The scenarios a writer sweep publishes, in sweep order.
pub const SCENARIOS: &[&str] = &[];

/// One point in the writer's configuration space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Variant;

impl Variant {
    pub fn label(&self) -> String {
        String::new()
    }
    pub fn parse(_label: &str) -> Option<Variant> {
        None
    }
}

/// Every variant the measured core can express.
pub fn matrix() -> Vec<Variant> {
    Vec::new()
}

/// The cells a profile walks, as the specs the document names them by.
pub fn sweep_cells(_profile: Profile) -> Vec<String> {
    Vec::new()
}
