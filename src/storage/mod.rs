//! The `storage` family: PMTiles against a directory tree, measured properly.
//!
//! This file is a stub that lane K1.4 needed to reach its own modules, and it
//! is deliberately nothing but module declarations. K1.2 (libviprs-bench#65)
//! owns the real `mod.rs` along with `cells.rs`, `document.rs` and `stats.rs`,
//! so when the two branches compose, K1.2's version of this file wins and the
//! two `pub mod` lines below are the whole of my diff to it.

pub mod model;
pub mod scenarios;
