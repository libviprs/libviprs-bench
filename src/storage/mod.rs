//! The `storage` family: PMTiles against a directory tree.
//!
//! One module per concern, and the lanes that own them are called out here
//! because this file is the one place they all meet.
//!
//! K1.3 (this lane) owns [`attest`], [`integrity`] and [`archive`]: what an
//! archived run must prove about itself, how it is digested, and what refuses
//! it. K1.2 owns the document, the cells and the statistics; K1.4 owns the
//! scenarios. Their `pub mod` lines belong next to these three, and a merge
//! that has to reconcile this file is a merge doing its job.

pub mod archive;
pub mod attest;
pub mod integrity;
