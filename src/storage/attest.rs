//! Did the thing I labelled actually run?
//!
//! causl answers this by instantiating the wasm engine and asking it what it
//! is, then stamping `engineAttested` from the answer rather than from the
//! flag that selected it. There is no wasm module here, but the question is the
//! same one and the failure it guards against is the same: a cell labelled
//! `pmtiles / root regime` whose numbers came from something else, published as
//! though the label were a measurement.
//!
//! So the storage analogue, from `SUITE-PLAN.md` §5.4: two observations, both
//! taken in the measuring process, neither of them a read of the cell's own
//! label.
//!
//! 1. **Shape.** The archive's root directory is walked and its entries are
//!    classified. A PMTiles archive small enough to index in one directory has
//!    a root full of tile entries and no leaf directories: the `root` regime,
//!    one directory read per lookup. An archive that spilled has a root full of
//!    pointers to leaf directories: the `leaf` regime, two reads per lookup and
//!    the regime the leaf cache exists for. The regime the cell *declares* is
//!    then compared against the regime that was *seen*, and a disagreement is a
//!    refusal.
//! 2. **Bytes.** A seeded sample of coordinates is read back from both backends
//!    and compared byte for byte. Two backends that disagree about what a tile
//!    contains are not two measurements of one workload, and timing them
//!    against each other is meaningless however clean the numbers look.
//!
//! The one thing this module must never do is decide a cell is attested by
//! reading what the cell says about itself. That is the mistake it exists to
//! prevent, and `storage_attestation_is_observed_not_asserted` in
//! `tests/storage_provenance_k13.rs` is the test that holds it to it.

use serde::{Deserialize, Serialize};

/// How a PMTiles archive indexes its tiles, which decides what a lookup costs.
///
/// K1.2's, re-exported rather than redefined. This module had its own copy with
/// the same two variants under different names, which is the shape of trouble
/// the SHA-256 duplication already turned out to be: two types for one concept,
/// only one of them reachable from the product, and nothing to notice when they
/// drift apart.
pub use super::cells::Regime;

/// The word a document uses for a regime.
///
/// `Regime` is K1.2's and carries no serde spelling of its own, so the mapping
/// from `Leaves` to the document's `"leaf"` lives here, where the document is.
pub fn regime_word(regime: Regime) -> &'static str {
    match regime {
        Regime::Root => "root",
        Regime::Leaves => "leaf",
    }
}

/// What a single root-directory entry points at, as read out of the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RootEntry {
    /// An entry addressing a tile directly.
    Tile,
    /// An entry addressing a leaf directory, which is what PMTiles writes with
    /// `run_length == 0`.
    LeafPointer,
}

/// The archive as it was found on disk, never as it was described.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedArchive {
    /// Which backend produced it: `pmtiles` or `directory`.
    pub backend: String,
    /// Every entry of the root directory, classified. Empty means nothing was
    /// observed, which is a refusal and not a regime.
    pub root_entries: Vec<RootEntry>,
    /// How many leaf directories the archive actually has.
    pub leaf_directories: u64,
    /// How many tiles it holds.
    pub tiles: u64,
}

impl ObservedArchive {
    /// The regime this archive is in, worked out from what was seen.
    ///
    /// `None` when the root directory was empty, because an archive nobody
    /// managed to read is not in a regime, it is unobserved. Returning
    /// `Some(Root)` there would be the same class of mistake as reading the
    /// label: an answer produced by a code path that looked at nothing.
    pub fn observed_regime(&self) -> Option<Regime> {
        if self.root_entries.is_empty() {
            return None;
        }
        if self.root_entries.contains(&RootEntry::LeafPointer) {
            Some(Regime::Leaves)
        } else {
            Some(Regime::Root)
        }
    }
}

/// The seeded byte-equivalence check between the two backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EquivalenceSample {
    /// The seed the coordinates were drawn from, so the sample is reproducible.
    pub seed: u64,
    /// How many coordinates were drawn.
    pub sampled: u32,
    /// How many returned identical bytes from both backends.
    pub matched: u32,
}

/// The smallest sample that counts, from `SUITE-PLAN.md` §5.4.
///
/// Not a statistical threshold: it is an agreement check, so a single mismatch
/// is fatal and the only question is how many coordinates were looked at before
/// declaring agreement. Sixty-four is what the plan names, and a smaller sample
/// is refused rather than scaled down, because a cell that sampled four
/// coordinates and a cell that sampled sixty-four would otherwise carry the
/// same `attested: true` and mean different things.
pub const MIN_EQUIVALENCE_SAMPLE: u32 = 64;

/// The verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attestation {
    /// Both observations agree with the cell's declaration.
    Attested {
        /// The regime that was seen, which by construction equals the one
        /// declared.
        observed_regime: Regime,
        /// The sample that was compared.
        equivalence: EquivalenceSample,
    },
    /// At least one observation contradicts the declaration, or could not be
    /// made at all. Every reason is reported, not the first.
    Refused {
        /// Why, in words, one entry per failed observation.
        reasons: Vec<String>,
    },
}

impl Attestation {
    /// Whether the cell may carry `attested: true`.
    pub fn is_attested(&self) -> bool {
        matches!(self, Attestation::Attested { .. })
    }

    /// The refusal reasons, empty when attested.
    pub fn reasons(&self) -> &[String] {
        match self {
            Attestation::Attested { .. } => &[],
            Attestation::Refused { reasons } => reasons,
        }
    }
}

/// Attest a cell against what was observed.
///
/// `declared` is the regime the cell claims. It is used for exactly one thing:
/// as the right-hand side of a comparison against the regime read out of
/// `archive`. It never reaches the verdict any other way, which is the whole
/// point of the function.
pub fn attest(
    declared: Regime,
    archive: &ObservedArchive,
    equivalence: &EquivalenceSample,
) -> Attestation {
    let mut reasons = Vec::new();

    let observed = archive.observed_regime();
    match observed {
        None => reasons.push(format!(
            "the {} archive's root directory has no entries, so no regime was observed and \
             the cell's declared regime '{}' rests on nothing",
            archive.backend,
            regime_word(declared)
        )),
        Some(seen) if seen != declared => reasons.push(format!(
            "the cell declares the '{}' regime and the {} archive is in the '{}' one: its \
             root directory holds {} entries of which {} point at leaf directories, and it \
             has {} leaf directories",
            regime_word(declared),
            archive.backend,
            regime_word(seen),
            archive.root_entries.len(),
            archive
                .root_entries
                .iter()
                .filter(|e| **e == RootEntry::LeafPointer)
                .count(),
            archive.leaf_directories,
        )),
        Some(_) => {}
    }

    // A regime read out of the root directory and a leaf-directory count that
    // contradicts it means the walk itself is wrong, and an attestation built on
    // a broken walk is worth no more than one built on the label.
    if let Some(seen) = observed {
        let consistent = match seen {
            Regime::Root => archive.leaf_directories == 0,
            Regime::Leaves => archive.leaf_directories > 0,
        };
        if !consistent {
            reasons.push(format!(
                "the {} archive's root directory reads as the '{}' regime while it reports {} \
                 leaf directories, so the two observations of one archive disagree",
                archive.backend,
                regime_word(seen),
                archive.leaf_directories
            ));
        }
    }

    if equivalence.sampled < MIN_EQUIVALENCE_SAMPLE {
        reasons.push(format!(
            "byte equivalence was checked over {} coordinates and the floor is {}",
            equivalence.sampled, MIN_EQUIVALENCE_SAMPLE
        ));
    }
    if equivalence.matched != equivalence.sampled {
        reasons.push(format!(
            "{} of {} sampled coordinates returned different bytes from the two backends \
             (seed {:#x}), so they are not two measurements of one workload",
            equivalence.sampled - equivalence.matched,
            equivalence.sampled,
            equivalence.seed
        ));
    }

    if reasons.is_empty() {
        Attestation::Attested {
            observed_regime: observed.expect("an empty root directory is a reason above"),
            equivalence: *equivalence,
        }
    } else {
        Attestation::Refused { reasons }
    }
}
