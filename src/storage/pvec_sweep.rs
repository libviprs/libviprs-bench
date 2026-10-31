//! Red stub (libviprs/libviprs#1331): the PVec row is not written yet.

use std::path::Path;

use super::writer_sweep::RepOut;

/// Stub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contender {
    /// Stub.
    Pmtiles,
    /// Stub.
    Pvec,
    /// Stub.
    GdalCog,
    /// Stub.
    VipsZip,
}

impl Contender {
    /// Stub.
    pub const ALL: [Contender; 4] = [
        Contender::Pmtiles,
        Contender::Pvec,
        Contender::GdalCog,
        Contender::VipsZip,
    ];
    /// Stub.
    pub fn label(self) -> &'static str {
        "stub"
    }
    /// Stub.
    pub fn parse(_s: &str) -> Option<Contender> {
        None
    }
}

/// Stub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanCell {
    /// Stub.
    pub width: u32,
    /// Stub.
    pub height: u32,
}

impl ScanCell {
    /// Stub.
    pub fn ci() -> ScanCell {
        ScanCell { width: 0, height: 0 }
    }
    /// Stub.
    pub fn a0() -> ScanCell {
        ScanCell { width: 0, height: 0 }
    }
    /// Stub.
    pub fn spec(&self) -> String {
        String::new()
    }
    /// Stub.
    pub fn parse(_s: &str) -> Option<ScanCell> {
        None
    }
    /// Stub.
    pub fn tiles(&self) -> u64 {
        0
    }
}

/// Stub.
pub fn one_rep(
    _c: Contender,
    _cell: ScanCell,
    _out_dir: &Path,
    _extra: &[String],
) -> Result<RepOut, String> {
    Err("stub".into())
}

/// Stub.
pub fn attest(_c: Contender, _cell: ScanCell, _out_dir: &Path, _out: &RepOut) -> Result<(), String> {
    Err("stub".into())
}
