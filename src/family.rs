//! Benchmark families: the unit the runner, the report and the charts key on.
//!
//! This crate used to be one benchmark with libvips wired through the middle of
//! it, so the libviprs-only question (how do monolithic, streaming and mapreduce
//! compare to each other, and how does that move across releases) could only be
//! answered as a by-product of a comparison it does not need. A family names the
//! question a run is asking, and the engine set, the output directory and the
//! recorded snapshot all follow from it (issue #64).
//!
//! Three members:
//!
//! * [`Family::Engines`] — monolithic against streaming against mapreduce. No
//!   `libvips` feature, no FFI, no libvips in the container. This is the
//!   default, and it builds and runs with **no cargo features at all**.
//! * [`Family::Storage`] — PMTiles against a directory tree. The member and its
//!   dispatch live here; the scenarios land in K1.2 (issue #65), so asking for
//!   it today is refused loudly rather than producing an empty run.
//! * [`Family::Vips`] — the libvips comparison, gated on `feature = "libvips"`,
//!   keeping its pinned Dockerfile stage, its `libvips-rs` FFI, its provenance
//!   pin check and its upstream pin validator exactly as they were.
//!
//! The engine set is a pure function of the family and nothing else. It is not
//! conditioned on `cfg(feature = "libvips")` and not on whether a `vips` binary
//! happens to be on `PATH`: an `engines` run on a machine that has libvips
//! installed measures the same three engines as one on a machine that does not,
//! which is the whole point of keying on the family instead of on the
//! environment.

use crate::harness::Engine;

/// The three libviprs pyramid engines, in pipeline order. Every libviprs-only
/// family measures exactly these, and the `vips` family measures these plus the
/// libvips oracle.
pub const LIBVIPRS_ENGINES: [Engine; 3] = [Engine::Monolithic, Engine::Streaming, Engine::MapReduce];

/// The cargo feature the `vips` family needs. Named in the refusal message so a
/// reader is told the fix, not just the problem.
pub const VIPS_FEATURE: &str = "libvips";

/// A benchmark family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Family {
    /// Monolithic against streaming against mapreduce. The default.
    Engines,
    /// PMTiles against a directory tree. Skeleton only until K1.2 (issue #65).
    Storage,
    /// The libvips comparison, behind `feature = "libvips"`.
    Vips,
}

/// Every family, in the order `--help` and the refusal messages list them.
pub const ALL_FAMILIES: [Family; 3] = [Family::Engines, Family::Storage, Family::Vips];

/// The family a run gets when nobody asks for one. libviprs-only, by design.
pub const DEFAULT_FAMILY: Family = Family::Engines;

impl Family {
    /// The family's name on the command line, in the report directory, and in
    /// the snapshot.
    pub fn as_str(self) -> &'static str {
        match self {
            Family::Engines => "engines",
            Family::Storage => "storage",
            Family::Vips => "vips",
        }
    }

    /// One line describing what the family measures, for `--help`.
    pub fn summary(self) -> &'static str {
        match self {
            Family::Engines => "monolithic vs streaming vs mapreduce (libviprs only, no features)",
            Family::Storage => "PMTiles vs a directory tree (libviprs only) — lands in K1.2 (#65)",
            Family::Vips => "the libvips dzsave comparison (needs --features libvips)",
        }
    }

    /// Parse a family name. Unknown names are `None`; use [`Family::resolve`]
    /// to get a refusal that says what the known names are.
    pub fn parse(name: &str) -> Option<Family> {
        ALL_FAMILIES.into_iter().find(|f| f.as_str() == name)
    }

    /// The cargo feature this family needs, if any.
    pub fn required_feature(self) -> Option<&'static str> {
        match self {
            Family::Vips => Some(VIPS_FEATURE),
            Family::Engines | Family::Storage => None,
        }
    }

    /// Whether the binary that is asking was built with what this family needs.
    ///
    /// The two libviprs-only families are always compiled in — that is the
    /// property the whole lane exists to establish — so only `vips` can answer
    /// `false`, and it does so on a default build.
    pub fn is_compiled_in(self) -> bool {
        match self {
            Family::Engines | Family::Storage => true,
            Family::Vips => cfg!(feature = "libvips"),
        }
    }

    /// Whether the family's scenarios exist yet. `storage` is a skeleton until
    /// K1.2 fills it in; asking for it before then is refused rather than
    /// silently measured as something else.
    pub fn is_implemented(self) -> bool {
        !matches!(self, Family::Storage)
    }

    /// The engines this family measures.
    ///
    /// A pure function of the family. `engines` and `storage` never include
    /// [`Engine::Libvips`], whether or not the `libvips` feature is on and
    /// whether or not a `vips` binary is on `PATH`.
    pub fn engines(self) -> Vec<Engine> {
        let mut engines = LIBVIPRS_ENGINES.to_vec();
        if self == Family::Vips {
            engines.push(Engine::Libvips);
        }
        engines
    }

    /// Whether a run of this family is allowed to record a libvips row at all.
    /// The report and scalability binaries gate every libvips code path on this
    /// rather than on `vips_available()`.
    pub fn measures_libvips(self) -> bool {
        self == Family::Vips
    }

    /// Resolve a family name into a family this binary can actually run, or a
    /// refusal that names the reason and the fix.
    pub fn resolve(name: &str) -> Result<Family, FamilyRefusal> {
        let Some(family) = Family::parse(name) else {
            return Err(FamilyRefusal::Unknown {
                name: name.to_string(),
            });
        };
        if let Some(feature) = family.required_feature() {
            if !family.is_compiled_in() {
                return Err(FamilyRefusal::FeatureOff { family, feature });
            }
        }
        if !family.is_implemented() {
            return Err(FamilyRefusal::NotYet { family });
        }
        Ok(family)
    }

    /// The subdirectory of `report/` this family's artifacts live in.
    ///
    /// One directory per family, so two families can never append to each
    /// other's history or overwrite each other's charts, and so the JS chart
    /// renderer needs nothing but a `--report-dir` to draw either of them.
    pub fn report_dir(self, report_root: &std::path::Path) -> std::path::PathBuf {
        report_root.join(self.as_str())
    }
}

impl std::fmt::Display for Family {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a requested family cannot be run here.
///
/// Every variant is a hard refusal with a non-zero exit code. The case this
/// exists for is asking for `vips` on a build that has no libvips in it: the
/// old code answered that by quietly running whatever it could, which produced
/// a libvips-shaped report with no libvips in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FamilyRefusal {
    /// No family goes by that name.
    Unknown { name: String },
    /// The family exists but this binary was built without its cargo feature.
    FeatureOff {
        family: Family,
        feature: &'static str,
    },
    /// The family exists and is compiled in, but its scenarios have not landed.
    NotYet { family: Family },
}

impl FamilyRefusal {
    /// The process exit code a binary should die with. Never zero: a refused
    /// run that exits 0 is the silent empty run this type exists to prevent.
    pub fn exit_code(&self) -> i32 {
        2
    }
}

impl std::fmt::Display for FamilyRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FamilyRefusal::Unknown { name } => {
                write!(f, "unknown benchmark family {name:?}. Known families: ")?;
                for (i, family) in ALL_FAMILIES.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    f.write_str(family.as_str())?;
                }
                Ok(())
            }
            FamilyRefusal::FeatureOff { family, feature } => write!(
                f,
                "the {family} family needs the `{feature}` cargo feature and this binary was \
                 built without it. Rebuild with `--features {feature}` (and run it somewhere \
                 libvips is installed), or ask for the `{default}` family, which needs no \
                 features at all.",
                default = DEFAULT_FAMILY.as_str(),
            ),
            FamilyRefusal::NotYet { family } => write!(
                f,
                "the {family} family has no scenarios yet: its skeleton is here and the cells \
                 land in K1.2 (libviprs-bench#65). Ask for the `{default}` family meanwhile.",
                default = DEFAULT_FAMILY.as_str(),
            ),
        }
    }
}

impl std::error::Error for FamilyRefusal {}
