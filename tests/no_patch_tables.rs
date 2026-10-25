//! Guards that nothing in this crate redirects a dependency through `[patch]`.
//!
//! A `[patch]` table only applies when the manifest holding it is the build
//! root, and it is silent when it fails to apply: cargo prints
//! `warning: patch ... was not used in the crate graph` into a build log that
//! nobody reads and carries on resolving the unpatched crate. That is issue
//! #61, and the same defect libviprs-cli#45 and libviprs-tests#210 fixed in
//! their own manifests. All three tables named the `libviprs/integration`
//! branch of the `libviprs/pdfium-render` fork long after the core crate moved
//! to `pdfium-render 0.9.4` from crates.io, so the fork's `0.9.3` could not
//! satisfy the requirement and the patch did nothing at all.
//!
//! An inert patch is worse than useless here. Cargo has to fetch the named git
//! branch before it can work out that the patch does not apply, so the table
//! made every build of this crate, including the `check (pdfium)` cell and the
//! 05:31 UTC nightly, depend on that branch still existing. Pointing the branch
//! at a name the fork does not have turns `cargo metadata` into a hard
//! `failed to find branch` error rather than a warning, which is what a routine
//! fork cleanup would have caused.
//!
//! # Two checks, because one of them is not enough
//!
//! The first reads the manifest. The second reads `Cargo.lock`, where cargo
//! records an ignored patch as a `[[patch.unused]]` table, which is what
//! libviprs-cli's `tests/pdfium_render_lockstep.rs` keys on.
//!
//! This crate gitignores `Cargo.lock`, and an earlier version of this file said
//! that ruled the lockfile out. It does not. Cargo re-resolves and **writes**
//! `Cargo.lock` into the package root immediately before it runs a test binary,
//! tracked or not, so the lockfile a test reads is the one cargo produced for
//! that very invocation rather than a leftover from some earlier build. It is
//! the same invariant the cli's test already relies on.
//!
//! The two catch different things and neither subsumes the other:
//!
//! * A patch that **does** apply changes the resolved graph and leaves no
//!   `[[patch.unused]]` stanza at all, so only the manifest scan sees it.
//! * A patch that is **inert**, however exotically spelled, is recorded in the
//!   lockfile by cargo itself, so the lockfile check sees it without this file
//!   having to out-parse TOML.
//!
//! That second point is why the lockfile half exists. The manifest scan is
//! line based, and a line-based scan of a format with this many equivalent
//! spellings will always have gaps: `[patch.crates-io] # note`,
//! `[ patch.crates-io ]`, `[patch . crates-io]` and `["patch".crates-io]` are
//! all honoured by cargo, and an earlier version of this scan missed every one
//! of them while reporting a clean pass.

use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()))
}

/// A line with any trailing comment removed, respecting double quotes so a `#`
/// inside `[patch."https://host/p#frag"]` is not mistaken for one.
fn without_trailing_comment(line: &str) -> &str {
    let mut in_quotes = false;
    for (index, ch) in line.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '#' if !in_quotes => return &line[..index],
            _ => {}
        }
    }
    line
}

/// A table header reduced to a comparable key: brackets, whitespace and quotes
/// removed. `[ patch . "crates-io" ]` and `[patch.crates-io]` both give
/// `patch.crates-io`, which is the point, because cargo honours both.
fn normalised_key(inner: &str) -> String {
    inner
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '"' && *c != '\'')
        .collect()
}

/// Does this normalised key name a `[patch]` table? `patch`, `patch.crates-io`
/// and `patch.https://…` all do; `patches` does not.
fn is_patch_key(key: &str) -> bool {
    key == "patch" || key.starts_with("patch.")
}

/// Every TOML table header in `text`, as `(1-based line, normalised key)`.
fn table_headers(text: &str) -> Vec<(usize, String)> {
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let line = without_trailing_comment(line).trim();
            let inner = line.strip_prefix('[')?.strip_suffix(']')?;
            Some((index + 1, normalised_key(inner)))
        })
        .collect()
}

/// Every `[patch…]` table in `text`, and every top-level dotted key that opens
/// one inline (`patch.crates-io = { … }`), which cargo also honours.
///
/// The dotted form only means `patch.*` while no table header has been seen
/// yet, because after `[dependencies]` the same line would mean
/// `dependencies.patch.*`. That is why this walks the file in order rather than
/// grepping it.
fn patch_declarations(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut at_top_level = true;

    for (index, raw) in text.lines().enumerate() {
        let line = without_trailing_comment(raw).trim();
        if line.is_empty() {
            continue;
        }

        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            at_top_level = false;
            let key = normalised_key(inner.trim_start_matches('[').trim_end_matches(']'));
            if is_patch_key(&key) {
                found.push((index + 1, format!("[{key}]")));
            }
            continue;
        }

        if at_top_level && let Some((key, _)) = line.split_once('=') {
            let key = normalised_key(key);
            if is_patch_key(&key) {
                found.push((index + 1, format!("{key} = …")));
            }
        }
    }

    found
}

/// The one patch this crate carries on purpose (issue #126): `pdfium-render`
/// resolved to the libviprs fork tag, in whitespace-free form. Delete this and
/// `unexpected_patches` once a released pdfium-render carries the fixes
/// (libviprs/pdfium-render#15), and the manifest goes back to no patch at all.
const ALLOWED_FORK_VALUES: [&str; 2] = [
    "{git=\"https://github.com/libviprs/pdfium-render\",tag=\"v0.9.4-viprs.1\"}",
    "{tag=\"v0.9.4-viprs.1\",git=\"https://github.com/libviprs/pdfium-render\"}",
];

/// Patch declarations that are not the allowlisted fork entry (issue #134).
///
/// The allowance is deliberately narrow: a `[patch.crates-io]` table holding
/// exactly one line, `pdfium-render` pinned to the fork tag. A second crate, a
/// path or branch spelling, another registry, or a dotted top-level key all
/// stay reported.
fn unexpected_patches(text: &str) -> Vec<(usize, String)> {
    let declarations = patch_declarations(text);

    // Collect the body of every `[patch.crates-io]` table: (header line, entries).
    let mut tables: Vec<(usize, Vec<String>)> = Vec::new();
    let mut in_crates_io = false;
    for (index, raw) in text.lines().enumerate() {
        let line = without_trailing_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_crates_io = normalised_key(inner) == "patch.crates-io" && !line.starts_with("[[");
            if in_crates_io {
                tables.push((index + 1, Vec::new()));
            }
        } else if in_crates_io {
            tables.last_mut().expect("pushed with the header").1.push(line.to_string());
        }
    }

    let allowed_header = |number: usize| {
        tables.iter().any(|(header, entries)| {
            *header == number
                && entries.len() == 1
                && entries[0].split_once('=').is_some_and(|(name, value)| {
                    let value: String = value.chars().filter(|c| !c.is_whitespace()).collect();
                    name.trim() == "pdfium-render" && ALLOWED_FORK_VALUES.contains(&value.as_str())
                })
        })
    };

    declarations
        .into_iter()
        .filter(|(number, _)| !allowed_header(*number))
        .collect()
}

/// `[[patch.unused]]` stanzas cargo recorded in the lockfile.
fn unused_patches(lock: &str) -> Vec<String> {
    let mut out = Vec::new();
    for stanza in lock.split("[[patch.unused]]").skip(1) {
        let body = stanza.find("\n[").map_or(stanza, |at| &stanza[..at]);
        let field = |key: &str| {
            body.lines()
                .map(str::trim)
                .find_map(|line| {
                    let rest = line
                        .strip_prefix(key)?
                        .trim_start()
                        .strip_prefix('=')?
                        .trim();
                    let inner = rest.strip_prefix('"')?;
                    Some(inner[..inner.find('"')?].to_string())
                })
                .unwrap_or_else(|| "<none>".to_string())
        };
        out.push(format!("{} <- {}", field("name"), field("source")));
    }
    out
}

#[test]
fn the_manifest_declares_no_patch_table() {
    let manifest = read(&crate_root().join("Cargo.toml"));

    // The positive control. An empty result has two explanations, and "the
    // manifest is clean" is only one of them: a scan that stopped recognising
    // headers would report the same nothing. `[package]` opens every Cargo
    // manifest, so seeing it proves the recogniser fired on this file.
    let headers = table_headers(&manifest);
    assert!(
        headers.iter().any(|(_, key)| key == "package"),
        "the scan found no [package] table in Cargo.toml, so it is not reading \
         table headers and the patch check below cannot fail. Headers seen: {headers:#?}"
    );

    let patches = unexpected_patches(&manifest);
    assert!(
        patches.is_empty(),
        "Cargo.toml declares {} patch redirect(s):\n{}\n\
         A patch here applies only when this crate is the build root, it is \
         reported as a warning rather than an error when it does not apply, and \
         cargo has to fetch whatever source it names on every single build. \
         That is issue #61. Mirror the core crate's dependency versions instead.",
        patches.len(),
        patches
            .iter()
            .map(|(number, what)| format!("  Cargo.toml:{number}: {what}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[test]
fn cargo_config_declares_no_patch_table() {
    // `.cargo/config.toml` carries `[patch]` tables too, and they apply to
    // every build run from this directory. The file is absent today, which is
    // the passing state; if one appears it has to stay patch-free.
    let path = crate_root().join(".cargo/config.toml");
    if !path.exists() {
        return;
    }

    let patches = patch_declarations(&read(&path));
    assert!(
        patches.is_empty(),
        ".cargo/config.toml declares {} patch redirect(s):\n{}",
        patches.len(),
        patches
            .iter()
            .map(|(number, what)| format!("  .cargo/config.toml:{number}: {what}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[test]
fn cargo_recorded_no_ignored_patch() {
    // Cargo writes this file immediately before running this binary, gitignored
    // or not, so it describes this very resolution. It catches an inert patch
    // in any spelling, including the ones the line-based scan above cannot see.
    let path = crate_root().join("Cargo.lock");
    let lock = read(&path);

    // Positive control, same reasoning as above: a lockfile with no packages in
    // it would make the assertion below pass over nothing.
    assert!(
        lock.contains("[[package]]"),
        "Cargo.lock has no [[package]] stanzas, so it is not a resolved \
         lockfile and the check below cannot fail"
    );

    let unused = unused_patches(&lock);
    assert!(
        unused.is_empty(),
        "cargo recorded {} ignored patch(es) in Cargo.lock:\n{}\n\
         An ignored patch is one cargo fetched, failed to apply, and warned \
         about into a log nobody reads. That is issue #61.",
        unused.len(),
        unused
            .iter()
            .map(|entry| format!("  {entry}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[test]
fn the_scan_survives_every_spelling_cargo_honours() {
    // Each of these was verified against cargo itself: build a crate carrying
    // the spelling, point it at a path that does not exist, and cargo errors,
    // which means it honoured the redirect. An earlier version of this scan
    // missed the last four while reporting a clean pass.
    for header in [
        "[patch.crates-io]",
        "[patch]",
        "[patch.\"https://example.invalid\"]",
        "[patch.crates-io] # keep in lockstep with ../libviprs/Cargo.toml",
        "[ patch.crates-io ]",
        "[patch . crates-io]",
        "[\"patch\".crates-io]",
    ] {
        let manifest = format!("[package]\nname = \"x\"\n\n{header}\nfoo = {{ path = \"f\" }}\n");
        assert_eq!(
            patch_declarations(&manifest).len(),
            1,
            "cargo honours {header}, so the scan must catch it"
        );
    }

    // The inline dotted form. It has to sit before any table header to mean
    // `patch.*`: after `[package]` the identical line means
    // `package.patch.crates-io`, which is why the scan tracks position rather
    // than grepping.
    let dotted = "patch.crates-io = { foo = { path = \"f\" } }\n\n[package]\nname = \"x\"\n";
    assert_eq!(
        patch_declarations(dotted).len(),
        1,
        "a top-level dotted patch key is honoured by cargo and must be caught"
    );

    // The same spelling under a table means `<table>.patch.*` and is not a
    // redirect, so it must not be reported.
    let scoped = "[package]\nname = \"x\"\n\n[dependencies]\npatch.crates-io = { path = \"f\" }\n";
    assert!(
        patch_declarations(scoped).is_empty(),
        "a dotted key under [dependencies] is not a patch table"
    );

    // Prose mentioning the table, which is what this very file is full of.
    let only_a_comment = "[package]\nname = \"x\"\n# no [patch.crates-io] here\n";
    assert!(
        patch_declarations(only_a_comment).is_empty(),
        "a comment mentioning [patch.crates-io] must not count as declaring one"
    );

    // A table whose name merely starts with the same letters.
    let patches_table = "[package]\nname = \"x\"\n\n[patches]\nfoo = 1\n";
    assert!(
        patch_declarations(patches_table).is_empty(),
        "[patches] is not a [patch] table"
    );
}

const FORK_PATCH: &str = "[package]\nname = \"x\"\n\n[patch.crates-io]\npdfium-render = { git = \"https://github.com/libviprs/pdfium-render\", tag = \"v0.9.4-viprs.1\" }\n";

#[test]
fn issue_134_the_pdfium_render_fork_patch_is_allowed() {
    assert!(
        unexpected_patches(FORK_PATCH).is_empty(),
        "the one pdfium-render fork entry is allowlisted until libviprs/pdfium-render#15's fixes are released"
    );
}

#[test]
fn issue_134_any_other_crate_in_the_patch_table_still_fails() {
    let text = format!("{FORK_PATCH}serde = {{ path = \"f\" }}\n");
    assert_eq!(unexpected_patches(&text).len(), 1, "only serde is unexpected");
}

#[test]
fn issue_134_pdfium_render_from_another_source_still_fails() {
    let text = "[package]\nname = \"x\"\n\n[patch.crates-io]\npdfium-render = { path = \"../pdfium-render\" }\n";
    assert_eq!(unexpected_patches(text).len(), 1, "a path redirect is not the fork tag");
    let text = FORK_PATCH.replace("v0.9.4-viprs.1", "integration");
    assert_eq!(unexpected_patches(&text).len(), 1, "a branch is not the pinned tag");
}

#[test]
fn issue_134_another_patch_registry_still_fails() {
    let text = FORK_PATCH.replace("patch.crates-io", "patch.\"https://example.invalid\"");
    assert_eq!(unexpected_patches(&text).len(), 1, "the allowlist is for crates-io only");
}

#[test]
fn issue_134_unused_patch_in_the_lockfile_still_fails() {
    let lock = "[[package]]\nname = \"x\"\n\n[[patch.unused]]\nname = \"pdfium-render\"\nsource = \"git+https://github.com/libviprs/pdfium-render?tag=v0.9.4-viprs.1\"\n";
    assert_eq!(unused_patches(lock).len(), 1, "an allowlisted entry that cargo ignored is still inert");
}
