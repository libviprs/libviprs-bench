//! The archive, and the rules that decide what may enter it.
//!
//! The epic's sharpest sentence is that a page which charts unarchived numbers
//! as a time series invents regressions that never happened. This module is the
//! door that keeps such numbers out. It does two things and they are the same
//! thing seen from either side: it derives a run's identity from the run's own
//! evidence, and it refuses a run whose evidence does not support the claim.
//!
//! # Refusing rather than reporting
//!
//! [`admit`] returns every reason a document is not archivable, never the
//! first. A sweep that was measured under emulation on a dirty tree with a
//! debug build has three problems, and finding them one re-run at a time turns
//! a forty-minute sweep into a two-hour afternoon. There is no flag that
//! relaxes a refusal except the two the plan names explicitly: `allowDirty`,
//! which does not make the dirt go away but forces it onto every cell, and a
//! profile that declares tmpfs, which does not make tmpfs comparable to a real
//! filesystem but does stop it being a surprise.
//!
//! # Why this reads JSON and not a struct
//!
//! K1.2 owns the document type. This module deliberately does not use it: it
//! reads the archived file as `serde_json::Value` and looks up documented
//! paths. A struct is the wrong instrument for a refusal, because the moment a
//! field carries `#[serde(default)]` — and a bench history full of older
//! documents guarantees one will — "absent" becomes a value and
//! `a_document_without_commit_dirty_or_invocation_is_refused` becomes a test
//! that cannot fail. The aggregator has to see the absence the file actually
//! has, which means reading the file and not a normalised view of it.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::sha256::sha256_hex;
use crate::storage::integrity::{self, CanonicalError, Digests};

/// Where archived storage runs live, relative to the crate root.
pub const ARCHIVE_DIR: &str = "archive/storage";
/// The index file inside [`ARCHIVE_DIR`].
pub const INDEX_FILE: &str = "index.json";

/// One reason a document may not be archived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// A stable machine-readable code, so a CI job can grep for one refusal
    /// without matching on prose.
    pub code: &'static str,
    /// What was wrong, with the value that was actually found.
    pub detail: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "REFUSED[{}]: {}", self.code, self.detail)
    }
}

fn refuse(code: &'static str, detail: impl Into<String>) -> Refusal {
    Refusal {
        code,
        detail: detail.into(),
    }
}

/// Read a nested path like `provenance.library.commit` out of a document.
fn at<'a>(doc: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cursor = doc;
    for segment in path.split('.') {
        cursor = cursor.get(segment)?;
    }
    Some(cursor)
}

/// A non-empty string at `path`, or `None` for absent, null, or empty.
///
/// Empty is `None` on purpose. A `commit: ""` is the shape a shell command
/// substitution leaves behind when `git` exited 128, and treating it as a
/// present value is how a document with no commit gets archived as though it
/// had one.
fn non_empty_str<'a>(doc: &'a Value, path: &str) -> Option<&'a str> {
    at(doc, path)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Every reason this document may not be archived. Empty means admitted.
pub fn admit(doc: &Value) -> Vec<Refusal> {
    let mut refusals = Vec::new();
    check_emulation(doc, &mut refusals);
    check_source_trees(doc, &mut refusals);
    check_build(doc, &mut refusals);
    check_filesystem(doc, &mut refusals);
    check_invocation_and_reps(doc, &mut refusals);
    check_cells(doc, &mut refusals);
    check_integrity(doc, &mut refusals);
    refusals
}

/// `emulated` must be exactly `false`.
///
/// Both `true` and `"unknown"` are refused, and so is an absent field. That
/// third case is the one the published PMTiles numbers are in: the artefact
/// does not say, and a reader has no way to find out. Refusing only `true`
/// would admit precisely the document this whole lane exists because of.
fn check_emulation(doc: &Value, refusals: &mut Vec<Refusal>) {
    match at(doc, "provenance.emulated") {
        Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => refusals.push(refuse(
            "emulated",
            format!(
                "the run was instruction-translated, and its timings describe the translator \
                 as much as the code ({})",
                emulation_evidence_line(doc)
            ),
        )),
        Some(Value::String(s)) => refusals.push(refuse(
            "emulated",
            format!(
                "the probe answered {s:?} rather than observing either way, so nothing in \
                 this document can settle whether its numbers were translated ({})",
                emulation_evidence_line(doc)
            ),
        )),
        other => refusals.push(refuse(
            "emulated",
            format!(
                "provenance.emulated is {}, so this document records no platform at all, \
                 which is the defect docs/pmtiles-benchmarks.md has",
                describe(other)
            ),
        )),
    }
}

/// A one-line summary of which evidence the probe used, for a refusal message.
fn emulation_evidence_line(doc: &Value) -> String {
    let Some(Value::Array(items)) = at(doc, "provenance.emulationEvidence") else {
        return "no evidence recorded".to_string();
    };
    let named: Vec<String> = items
        .iter()
        .map(|e| {
            format!(
                "{}={}",
                e.get("source").and_then(|v| v.as_str()).unwrap_or("?"),
                e.get("verdict").and_then(|v| v.as_str()).unwrap_or("?")
            )
        })
        .collect();
    if named.is_empty() {
        "no evidence recorded".to_string()
    } else {
        format!("evidence: {}", named.join(", "))
    }
}

/// Both trees must name a commit and a dirty flag, and a dirty tree needs the
/// flag and then the stamp.
fn check_source_trees(doc: &Value, refusals: &mut Vec<Refusal>) {
    for (label, commit_path, dirty_path) in [
        ("the harness", "provenance.commit", "provenance.dirty"),
        (
            "the measured library",
            "provenance.library.commit",
            "provenance.library.dirty",
        ),
    ] {
        if non_empty_str(doc, commit_path).is_none() {
            refusals.push(refuse(
                "commit",
                format!(
                    "{commit_path} is {} for {label}, so this run cannot be tied to a source \
                     tree. A linked git worktree bind-mounted into a container, a tree read \
                     through a share that trips git's safe.directory check, and a git-less \
                     tarball all produce exactly this, and all three are fixed from the \
                     environment rather than by relaxing the rule",
                    describe(at(doc, commit_path))
                ),
            ));
        }
        match at(doc, dirty_path) {
            Some(Value::Bool(_)) => {}
            other => refusals.push(refuse(
                "dirty",
                format!(
                    "{dirty_path} is {} for {label}; a missing dirty flag is not a clean tree",
                    describe(other)
                ),
            )),
        }
    }

    let dirty = at(doc, "provenance.dirty").and_then(Value::as_bool).unwrap_or(false)
        || at(doc, "provenance.library.dirty")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if !dirty {
        return;
    }
    let allowed = at(doc, "provenance.allowDirty")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !allowed {
        refusals.push(refuse(
            "dirty-not-allowed",
            "a tree is dirty and provenance.allowDirty is not true, so these numbers describe \
             a source state that exists on exactly one machine and cannot be recovered",
        ));
        return;
    }
    // Allowed, so the dirt has to travel with every number rather than sitting
    // in a header nobody reads when they quote a single cell.
    let unstamped: Vec<String> = cells(doc)
        .iter()
        .enumerate()
        .filter(|(_, cell)| cell.get("dirty").and_then(Value::as_bool) != Some(true))
        .map(|(i, cell)| cell_name(i, cell))
        .collect();
    if !unstamped.is_empty() {
        refusals.push(refuse(
            "dirty-not-stamped",
            format!(
                "provenance.allowDirty is true but {} of {} cells do not carry dirty: true \
                 ({}), so a reader quoting one cell would not know",
                unstamped.len(),
                cells(doc).len(),
                unstamped.join(", ")
            ),
        ));
    }
}

/// No debug build, and no toolchain flag that changes what is being measured.
fn check_build(doc: &Value, refusals: &mut Vec<Refusal>) {
    match at(doc, "provenance.node.debugAssertions") {
        Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => refusals.push(refuse(
            "debug-build",
            "debug assertions are on, so every bounds check and overflow check in the engine \
             is in the measurement and the numbers are of a build nobody ships",
        )),
        other => refusals.push(refuse(
            "debug-build",
            format!(
                "provenance.node.debugAssertions is {}, so this document does not say whether \
                 it measured a release build",
                describe(other)
            ),
        )),
    }
    match non_empty_str(doc, "provenance.node.buildProfile") {
        Some("release") => {}
        Some(other) => refusals.push(refuse(
            "debug-build",
            format!("the build profile is {other:?} and only release is measurable"),
        )),
        None => refusals.push(refuse(
            "debug-build",
            "provenance.node.buildProfile is absent, so the document does not say what it \
             measured",
        )),
    }
    if let Some(flags) = at(doc, "provenance.node.rustflags").and_then(|v| v.as_str()) {
        for forbidden in ["-C instrument-coverage", "-Cinstrument-coverage", "-Z"] {
            if flags.contains(forbidden) {
                refusals.push(refuse(
                    "perturbing-rustflags",
                    format!(
                        "RUSTFLAGS carries {forbidden:?} ({flags:?}), which changes the code \
                         being timed"
                    ),
                ));
            }
        }
    }
}

/// The scratch filesystem has to be recorded, and tmpfs is refused by default.
///
/// tmpfs is RAM with a filesystem interface. A PMTiles read benchmark on it
/// measures a memcpy, a directory-tree benchmark on it measures a memcpy
/// through several hundred thousand inodes, and the ratio between the two is
/// not the ratio anyone will see on a disk. It stays available because a
/// deliberate RAM-bound cell is a legitimate thing to measure, but it has to be
/// declared, so it is never a fact a reader discovers from a filename.
fn check_filesystem(doc: &Value, refusals: &mut Vec<Refusal>) {
    let Some(fs_type) = non_empty_str(doc, "provenance.filesystem.fsType") else {
        refusals.push(refuse(
            "filesystem",
            format!(
                "provenance.filesystem.fsType is {}, so this document does not say what the \
                 scratch directory was on, and overlayfs, a virtiofs bind mount and a real \
                 ext4 are three different benchmarks",
                describe(at(doc, "provenance.filesystem.fsType"))
            ),
        ));
        return;
    };
    if non_empty_str(doc, "provenance.filesystem.scratchDir").is_none() {
        refusals.push(refuse(
            "filesystem",
            "provenance.filesystem.scratchDir is absent, so the fsType above describes a \
             directory nobody can name",
        ));
    }
    if fs_type == "tmpfs"
        && at(doc, "provenance.filesystem.declaredTmpfs").and_then(Value::as_bool) != Some(true)
    {
        refusals.push(refuse(
            "tmpfs",
            "the scratch directory is on tmpfs and the profile does not declare it; tmpfs is \
             RAM with a filesystem interface, so these numbers are a memcpy benchmark wearing \
             a storage benchmark's labels",
        ));
    }
}

/// The invocation has to be recorded, resolved, and consistent with the cells.
fn check_invocation_and_reps(doc: &Value, refusals: &mut Vec<Refusal>) {
    let Some(invocation) = at(doc, "provenance.invocation") else {
        refusals.push(refuse(
            "invocation",
            "provenance.invocation is absent, so nothing records what was asked for and the \
             numbers cannot be reproduced even on this machine",
        ));
        return;
    };
    for field in ["argv", "command", "cwd", "resolved"] {
        if invocation.get(field).map(Value::is_null).unwrap_or(true) {
            refusals.push(refuse(
                "invocation",
                format!("provenance.invocation.{field} is absent"),
            ));
        }
    }

    match at(doc, "provenance.invocation.resolved.scenarios") {
        Some(Value::Array(items)) if !items.is_empty() => {
            if items.iter().any(|i| i.as_str() == Some("all")) {
                refusals.push(refuse(
                    "scenarios-unresolved",
                    "resolved.scenarios still says \"all\"; \"all\" means a different set of \
                     scenarios on every day the suite grows, so two runs that both say it are \
                     not comparable",
                ));
            }
        }
        other => refusals.push(refuse(
            "scenarios-unresolved",
            format!(
                "resolved.scenarios is {}, so the document does not record which scenarios \
                 the defaults expanded to",
                describe(other)
            ),
        )),
    }

    // Resolved reps against every cell's reps. This is the cheapest possible
    // check on the most expensive possible mistake: a sweep that says it took
    // seven repetitions per cell and has a cell that took one.
    let Some(resolved_reps) = at(doc, "provenance.invocation.resolved.reps").and_then(Value::as_u64)
    else {
        refusals.push(refuse(
            "reps-disagree",
            format!(
                "resolved.reps is {}, so there is nothing to check the cells against",
                describe(at(doc, "provenance.invocation.resolved.reps"))
            ),
        ));
        return;
    };
    let disagreeing: Vec<String> = cells(doc)
        .iter()
        .enumerate()
        .filter_map(|(i, cell)| {
            let cell_reps = cell.get("reps").and_then(Value::as_u64);
            (cell_reps != Some(resolved_reps)).then(|| {
                format!(
                    "{} has reps {}",
                    cell_name(i, cell),
                    describe(cell.get("reps"))
                )
            })
        })
        .collect();
    if !disagreeing.is_empty() {
        refusals.push(refuse(
            "reps-disagree",
            format!(
                "the invocation resolved to {resolved_reps} repetitions per cell but {}",
                disagreeing.join("; ")
            ),
        ));
    }
}

/// Every cell has to be attested if it claims to be ok, and explained if it is
/// not.
fn check_cells(doc: &Value, refusals: &mut Vec<Refusal>) {
    let cells = cells(doc);
    if cells.is_empty() {
        refusals.push(refuse(
            "no-cells",
            "the document has no cells; an empty reading is a refusal, not a result",
        ));
        return;
    }
    for (i, cell) in cells.iter().enumerate() {
        let outcome = cell.get("outcome").and_then(|v| v.as_str()).unwrap_or("");
        if outcome == "ok" {
            if cell.get("storageAttested").and_then(Value::as_bool) != Some(true) {
                refusals.push(refuse(
                    "unattested-cell",
                    format!(
                        "{} claims outcome ok and storageAttested is {}; an ok cell that was \
                         never observed to have measured the backend it names is a label, not \
                         a measurement",
                        cell_name(i, cell),
                        describe(cell.get("storageAttested"))
                    ),
                ));
            }
        } else if non_empty_str(cell, "reason").is_none() {
            refusals.push(refuse(
                "outcome-without-reason",
                format!(
                    "{} has outcome {outcome:?} and no reason",
                    cell_name(i, cell)
                ),
            ));
        }
    }
}

/// If the document carries digests, they have to still hold.
fn check_integrity(doc: &Value, refusals: &mut Vec<Refusal>) {
    if doc.get("integrity").is_none() {
        // An unsealed document is the ordinary input to `--archive`: sealing is
        // what archiving does. It is only a refusal under `--verify`, which is
        // the aggregator's other mode.
        return;
    }
    match integrity::verify(doc) {
        Ok(report) if report.ok() => {}
        Ok(report) => {
            for line in report.lines() {
                refusals.push(refuse("integrity", line));
            }
        }
        Err(err) => refusals.push(refuse(
            "integrity",
            format!("the document cannot be canonicalised, so it cannot be digested: {err}"),
        )),
    }
}

/// The document's cells, or an empty slice.
fn cells(doc: &Value) -> &[Value] {
    match doc.get("cells") {
        Some(Value::Array(items)) => items,
        _ => &[],
    }
}

/// How a refusal names a cell: its own labels when it has them, its index when
/// it does not.
fn cell_name(index: usize, cell: &Value) -> String {
    let backend = cell.get("backend").and_then(|v| v.as_str());
    let scenario = cell.get("scenario").and_then(|v| v.as_str());
    let scale = cell.get("scale").and_then(Value::as_u64);
    match (backend, scenario, scale) {
        (Some(b), Some(s), Some(n)) => format!("cell[{index}] {b}/{s}@{n}"),
        (Some(b), Some(s), None) => format!("cell[{index}] {b}/{s}"),
        _ => format!("cell[{index}]"),
    }
}

/// How a refusal message prints a value that was not what it should have been.
///
/// The distinction between `absent` and `null` is kept because the whole
/// canonicalisation contract rests on them being different, and a message that
/// called both "missing" would teach a reader the opposite.
fn describe(value: Option<&Value>) -> String {
    match value {
        None => "absent".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(v) => v.to_string(),
    }
}

/// Why a run id could not be derived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunIdError {
    /// A field the id is built from is absent or empty.
    Missing(&'static str),
}

impl std::fmt::Display for RunIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunIdError::Missing(path) => write!(
                f,
                "{path} is absent, and a run id derived around a missing field would collide \
                 with every other run missing it"
            ),
        }
    }
}

impl std::error::Error for RunIdError {}

/// The run's identity, derived from the run's own evidence.
///
/// `<startedAt>-<library commit>-<host8>`, where `host8` is the first eight hex
/// characters of a sha256 over the environment fields that decide whether two
/// runs are comparable: os, arch, cpu model, rustc, scratch filesystem type and
/// the emulation verdict.
///
/// Nothing here reads the clock. That is not a style preference: an id with
/// `SystemTime::now()` in it makes archiving the same document twice produce
/// two entries, so an idempotent re-archive silently doubles a series and the
/// page draws a flat line as two points. The `startedAt` in the id is the one
/// the document recorded, which is a fact about the run rather than about when
/// somebody got round to filing it.
pub fn run_id(doc: &Value) -> Result<String, RunIdError> {
    let started_at =
        non_empty_str(doc, "startedAt").ok_or(RunIdError::Missing("startedAt"))?;
    let commit = non_empty_str(doc, "provenance.library.commit")
        .ok_or(RunIdError::Missing("provenance.library.commit"))?;
    Ok(format!(
        "{}-{}-{}",
        compact_timestamp(started_at),
        commit,
        host8(doc)
    ))
}

/// `2026-09-13T21:45:00.123Z` into `20260913T214500Z`.
///
/// Colons are a path separator on one of the platforms this archive gets copied
/// to and a shell quoting hazard on the rest, and the fractional part is
/// discarded so that an id is stable against a producer that starts or stops
/// printing milliseconds.
fn compact_timestamp(iso: &str) -> String {
    let without_fraction = match iso.split_once('.') {
        Some((head, tail)) => {
            let suffix = if tail.ends_with('Z') { "Z" } else { "" };
            format!("{head}{suffix}")
        }
        None => iso.to_string(),
    };
    without_fraction
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// The environment bucket: eight hex characters over the fields that decide
/// comparability.
///
/// NUL-separated rather than concatenated, so that an os of `linu` with an arch
/// of `xaarch64` cannot hash to the same bucket as `linux` with `aarch64`.
fn host8(doc: &Value) -> String {
    let emulated = match at(doc, "provenance.emulated") {
        Some(v) => v.to_string(),
        None => "absent".to_string(),
    };
    let parts = [
        non_empty_str(doc, "provenance.os").unwrap_or("unknown"),
        non_empty_str(doc, "provenance.arch").unwrap_or("unknown"),
        non_empty_str(doc, "provenance.cpuModel").unwrap_or("unknown"),
        non_empty_str(doc, "provenance.node.rustc").unwrap_or("unknown"),
        non_empty_str(doc, "provenance.filesystem.fsType").unwrap_or("unknown"),
        &emulated,
    ];
    let joined = parts.join("\0");
    sha256_hex(joined.as_bytes())[..8].to_string()
}

/// A sealed copy of `doc`: the same evidence, plus the four digests and the
/// time it was combined.
///
/// Sealing is idempotent in the only sense that matters. `combinedAt` moves on
/// every call and `integrity` is written fresh, but neither is covered by the
/// `document` digest, so the digest, and therefore the run id and the archive
/// path, are the same on the second call as on the first.
pub fn seal(doc: &Value) -> Result<(Value, Digests), CanonicalError> {
    let digests = integrity::compute_digests(doc)?;
    let mut map: Map<String, Value> = match doc {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    map.insert(
        "combinedAt".to_string(),
        json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
    );
    map.insert("integrity".to_string(), serde_json::to_value(&digests)
        .expect("Digests is four strings and always serialises"));
    Ok((Value::Object(map), digests))
}

/// What happened when a document was archived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    /// The run's derived id.
    pub run_id: String,
    /// Where the sealed document was written, or already was.
    pub path: PathBuf,
    /// The `document` digest.
    pub document_digest: String,
    /// `false` when an identical run was already archived, so this call changed
    /// nothing.
    pub written: bool,
}

/// Why an archive attempt failed.
#[derive(Debug)]
pub enum ArchiveError {
    /// The document was refused. Every reason, not the first.
    Refused(Vec<Refusal>),
    /// The id could not be derived.
    RunId(RunIdError),
    /// The document could not be canonicalised.
    Canonical(CanonicalError),
    /// A different run is already archived under this id.
    Collision {
        /// The id both runs derive to.
        run_id: String,
        /// The digest already on disk.
        existing: String,
        /// The digest of the document being archived.
        incoming: String,
    },
    /// The filesystem said no.
    Io(std::io::Error),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveError::Refused(refusals) => {
                writeln!(f, "the document was refused for {} reasons:", refusals.len())?;
                for r in refusals {
                    writeln!(f, "  {r}")?;
                }
                Ok(())
            }
            ArchiveError::RunId(e) => write!(f, "the run id could not be derived: {e}"),
            ArchiveError::Canonical(e) => write!(f, "{e}"),
            ArchiveError::Collision {
                run_id,
                existing,
                incoming,
            } => write!(
                f,
                "{run_id} is already archived with document digest {existing} and this \
                 document digests to {incoming}; two different runs derived the same id, which \
                 means the id is missing a field that distinguishes them"
            ),
            ArchiveError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ArchiveError {}

impl From<std::io::Error> for ArchiveError {
    fn from(e: std::io::Error) -> Self {
        ArchiveError::Io(e)
    }
}

/// Admit, seal and file a document under `root`.
///
/// Archiving the same document twice files it once. The second call recomputes
/// the id from the same evidence, finds the file, checks that the digest on
/// disk matches, and returns `written: false` having touched nothing.
pub fn archive(doc: &Value, root: &Path) -> Result<ArchiveEntry, ArchiveError> {
    let refusals = admit(doc);
    if !refusals.is_empty() {
        return Err(ArchiveError::Refused(refusals));
    }
    let run_id = run_id(doc).map_err(ArchiveError::RunId)?;
    let (sealed, digests) = seal(doc).map_err(ArchiveError::Canonical)?;

    std::fs::create_dir_all(root)?;
    let path = root.join(format!("{run_id}.json"));

    if path.exists() {
        let existing: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)
            .map_err(|e| ArchiveError::Io(std::io::Error::other(e)))?;
        let existing_digest = existing
            .get("integrity")
            .and_then(|i| i.get("document"))
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .to_string();
        if existing_digest == digests.document {
            return Ok(ArchiveEntry {
                run_id,
                path,
                document_digest: digests.document,
                written: false,
            });
        }
        return Err(ArchiveError::Collision {
            run_id,
            existing: existing_digest,
            incoming: digests.document,
        });
    }

    let body = serde_json::to_string_pretty(&sealed)
        .map_err(|e| ArchiveError::Io(std::io::Error::other(e)))?;
    std::fs::write(&path, format!("{body}\n"))?;
    update_index(root, &run_id, &digests.document, doc)?;

    Ok(ArchiveEntry {
        run_id,
        path,
        document_digest: digests.document,
        written: true,
    })
}

/// Add one row to the archive index, or leave it alone if the row is there.
fn update_index(
    root: &Path,
    run_id: &str,
    document_digest: &str,
    doc: &Value,
) -> Result<(), ArchiveError> {
    let index_path = root.join(INDEX_FILE);
    let mut rows: Vec<Value> = if index_path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&index_path)?)
            .map_err(|e| ArchiveError::Io(std::io::Error::other(e)))?
    } else {
        Vec::new()
    };
    if rows
        .iter()
        .any(|row| row.get("runId").and_then(|v| v.as_str()) == Some(run_id))
    {
        return Ok(());
    }
    rows.push(json!({
        "runId": run_id,
        "documentDigest": document_digest,
        "startedAt": at(doc, "startedAt").cloned().unwrap_or(Value::Null),
        "libraryCommit": at(doc, "provenance.library.commit").cloned().unwrap_or(Value::Null),
        "emulated": at(doc, "provenance.emulated").cloned().unwrap_or(Value::Null),
        "fsType": at(doc, "provenance.filesystem.fsType").cloned().unwrap_or(Value::Null),
        "file": format!("{run_id}.json"),
    }));
    rows.sort_by(|a, b| {
        a.get("runId")
            .and_then(|v| v.as_str())
            .cmp(&b.get("runId").and_then(|v| v.as_str()))
    });
    let body = serde_json::to_string_pretty(&rows)
        .map_err(|e| ArchiveError::Io(std::io::Error::other(e)))?;
    std::fs::write(&index_path, format!("{body}\n"))?;
    Ok(())
}
