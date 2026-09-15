//! Two families, one host, one answer about the filesystem underneath.
//!
//! The final capture of this epic ran both families on the same box two
//! minutes apart, against the same `/dev/vda1`. The `storage` document recorded
//!
//! ```text
//! "filesystem": { "scratchDir": "/scratch/libviprs-storage-provenance",
//!                 "fsType": "unknown", "mountSource": "/dev/vda1", ... }
//! ```
//!
//! and the `engines` document, from the same mount, recorded `"fsType":
//! "ext4"`. One detected and the other did not, and nothing anywhere noticed,
//! because `"unknown"` is a populated field: `archive::admit` reads it with
//! `non_empty_str` and lets it through, and the importer's `!prov.filesystem
//! ?.fsType` check is falsy-only, so a document that says nothing about what it
//! measured on passes two gates written to catch exactly that.
//!
//! `host.fsType` is an era axis. The day `storage` starts detecting `ext4` its
//! version axis splits into two eras and stops drawing a line across runs that
//! are genuinely comparable, so the disagreement is not cosmetic and neither is
//! it fixed by making one side detect: it is fixed by the two sides asking the
//! same question of the same kind of directory, which is what
//! [`Family::scratch_root`] now is and what this file holds them to.
//!
//! Nothing here times anything, so it is safe to run beside a sweep.

use std::path::PathBuf;

use libviprs_bench::family::Family;
use libviprs_bench::provenance::FilesystemInfo;

/// The families whose documents carry a `provenance.filesystem` block.
///
/// `Vips` is left out on purpose rather than forgotten: it shares `engines`'
/// sink root, so including it would compare a value with itself and read as a
/// third independent agreement that is not one.
const DOCUMENT_FAMILIES: [Family; 2] = [Family::Engines, Family::Storage];

/// RED against the producer that made the published capture: `storage`'s
/// provenance probed `$TMPDIR/libviprs-storage-provenance`, a directory nothing
/// in the crate ever creates, so `statfs` answered `ENOENT` and `fs_type_name`
/// reported `"unknown"` while `engines`, which made its sink root first,
/// reported `ext4` for the same mount.
///
/// It is also RED against the shallower fix of teaching `storage` to
/// `create_dir_all` a directory it still never writes into: that would make the
/// two agree today and leave them free to disagree again the next time one of
/// them moves, because there would still be two copies of the rule.
#[test]
fn the_two_document_families_agree_on_the_filesystem_they_measure_on() {
    let roots: Vec<(Family, PathBuf)> = DOCUMENT_FAMILIES
        .into_iter()
        .map(|family| (family, family.scratch_root()))
        .collect();

    // A positive control on the comparison itself. Two families that resolved
    // to one directory would agree for a reason that proves nothing, and that
    // is a live way for this test to rot: `scratch_root` is a `match` and a
    // careless arm merge would collapse them.
    assert_ne!(
        roots[0].1, roots[1].1,
        "the two families must probe two different directories, or their agreement is a \
         tautology rather than a measurement: {roots:?}"
    );

    let observed: Vec<(Family, FilesystemInfo)> = roots
        .iter()
        .map(|(family, root)| (*family, FilesystemInfo::of(root)))
        .collect();

    for (family, fs) in &observed {
        assert!(
            fs.scratch_dir.contains("libviprs"),
            "the {family} family recorded {:?}, which is not a directory this crate writes into",
            fs.scratch_dir
        );
        // The published storage block names `/dev/vda1` as its mount source and
        // `/scratch/libviprs-storage-provenance` as its directory, and that
        // directory has never existed on any host. A provenance block about a
        // directory that is not there describes nothing, so this is the
        // assertion that kills the missing `create_dir_all` on its own.
        assert!(
            std::path::Path::new(&fs.scratch_dir).is_dir(),
            "the {family} family recorded {:?}, which is not a directory that exists",
            fs.scratch_dir
        );
        // `unknown-0x<magic>` is a fact: an unmapped kernel magic is a number
        // somebody can look up. Bare `unknown` is the `statfs` call failing,
        // and it is the exact value the published capture carries.
        assert_ne!(
            fs.fs_type, "unknown",
            "the {family} family could not name the filesystem under {}; `unknown` here means \
             `statfs` failed, and a document carrying it looks populated and says nothing",
            fs.scratch_dir
        );
        assert!(
            !fs.fs_type.is_empty(),
            "the {family} family recorded an empty fsType for {}",
            fs.scratch_dir
        );
    }

    assert_eq!(
        observed[0].1.fs_type, observed[1].1.fs_type,
        "the {} and {} families are looking at the same $TMPDIR on the same host and gave two \
         answers ({:?} against {:?}). fsType is an era axis, so this splits comparable runs \
         into two eras on a difference that is not one",
        observed[0].0, observed[1].0, observed[0].1.fs_type, observed[1].1.fs_type
    );
    assert_eq!(
        observed[0].1.mount_source, observed[1].1.mount_source,
        "the two families disagree about which mount they are on, which is the same defect one \
         level down from fsType"
    );
}

/// RED against `fs_type_name` answering `"unknown"` for a directory that is
/// simply not there yet.
///
/// That is what turned one missing `create_dir_all` into a published document
/// whose filesystem block names `/dev/vda1` as the mount source and `unknown`
/// as the filesystem on it. The two cannot both be true, and the second one is
/// the one that was wrong: `mount_entry` reads `/proc/self/mountinfo` and never
/// needs the path to exist, `statfs` does.
///
/// A directory that does not exist will be created on whatever its nearest
/// existing ancestor is mounted on, so that is the honest answer and this
/// pins it.
#[test]
fn a_directory_that_is_not_there_yet_names_the_filesystem_it_will_be_created_on() {
    let root = Family::Storage.scratch_root();
    let missing = root.join(format!("not-created-yet-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&missing);
    assert!(
        !missing.exists(),
        "this test is about a path that is not there; {} is",
        missing.display()
    );

    let ancestor = FilesystemInfo::of(&root);
    let absent = FilesystemInfo::of(&missing);

    assert_ne!(
        absent.fs_type, "unknown",
        "a path that is not there yet read as {:?}, which is the value the published storage \
         capture carries and the reason nobody could tell a missing directory from an \
         unnameable filesystem",
        absent.fs_type
    );
    assert_eq!(
        absent.fs_type, ancestor.fs_type,
        "{} will be created under {}, so it can only land on that filesystem",
        missing.display(),
        root.display()
    );
}

/// RED against a producer that goes back to spelling its own probe path.
///
/// The two families disagreed because each carried its own copy of "pick a
/// directory, then ask what it is on", and only one copy made the directory
/// first. Fixing the copies without removing them leaves the next lane free to
/// add a third, and there already was a third: `storage-aggregate
/// --provenance`, the mode you run *before* a sweep to find out what the sweep
/// will record, defaulted to bare `$TMPDIR` while the sweep it previews writes
/// under `$TMPDIR/libviprs-storage`. This test is what found it.
///
/// A source scan and not a behaviour test, because the property is that there
/// is exactly one source for the path, and a behaviour test cannot see a second
/// source that happens to agree on the host it runs on.
#[test]
fn every_provenance_probe_takes_its_path_from_the_family_scratch_root() {
    // Every place in `src/` that asks what filesystem a directory is on, and
    // the line each one has to get its directory from. Spelled out rather than
    // pattern-matched: a fourth prober is a thing to look at, not a thing to
    // wave through because it happens to contain the right substring somewhere.
    const PROBE_SITES: [(&str, &str); 3] = [
        (
            "src/engines/mod.rs",
            "let scratch = crate::family::Family::Engines.scratch_root();",
        ),
        (
            "src/storage/mod.rs",
            "let scratch = crate::family::Family::Storage.scratch_root();",
        ),
        (
            "src/storage_aggregate_main.rs",
            "let mut scratch = Family::Storage.scratch_root();",
        ),
    ];

    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs(&src, &mut files);
    // A positive control on the walk. A walk that found nothing would pass every
    // assertion below by finding nothing, which is not the same as passing.
    assert!(
        !files.is_empty(),
        "the walk over {} found no Rust files at all",
        src.display()
    );

    let mut callers: Vec<String> = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path).expect("a source file in this repository reads");
        // The definition in `provenance.rs` is not a call, and neither is a doc
        // comment mentioning one. Both matched on the first run of this test,
        // which is the ordinary way a source scan turns into a scan of prose.
        let calls = text.lines().any(|l| {
            l.contains("capture_for_document(")
                && !l.contains("fn capture_for_document(")
                && !l.trim_start().starts_with("///")
                && !l.trim_start().starts_with("//")
        });
        if calls {
            let rel = path
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            callers.push(rel);
        }
    }
    callers.sort();

    let mut expected: Vec<String> = PROBE_SITES.iter().map(|(f, _)| (*f).to_string()).collect();
    expected.sort();
    assert_eq!(
        callers, expected,
        "the set of files that probe the scratch filesystem has changed. Every one of them has \
         to take its directory from Family::scratch_root, which is the one place that knows both \
         the path and that the directory has to exist before anything asks what it is on"
    );

    for (file, from) in PROBE_SITES {
        let text = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(file))
            .expect("a probe site reads");
        assert!(
            text.contains(from),
            "{file} no longer takes its probe directory from `{from}`. A path spelled out at the \
             call site is the second copy of the rule, and the copy that forgets the \
             `create_dir_all` is the one that published `fsType: \"unknown\"` against `ext4` for \
             one host"
        );
    }
}

/// Every `.rs` file under `dir`, recursively.
fn collect_rs(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}
