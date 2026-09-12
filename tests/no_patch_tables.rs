//! Guards that this crate's `Cargo.toml` carries no `[patch]` table.
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
//! libviprs-cli guards the same property through its `Cargo.lock`, where cargo
//! records an ignored patch as a `[[patch.unused]]` table. This crate does not
//! track a lockfile (`.gitignore` lists `Cargo.lock`), so there is no committed
//! artefact to read and the manifest itself is what gets held.
//!
//! The scan is line based and recognises a table only as a line that is nothing
//! but the header, which is the shape libviprs-tests uses in
//! `tests/cargo_metadata_pins.rs`. A substring search would match the paragraph
//! above, and a guard that reads a comment as configuration is the failure this
//! file exists to prevent.

/// Every TOML table header in `manifest`, as `(1-based line number, header)`.
///
/// A header is a line whose trimmed form is bracketed at both ends and nothing
/// else, so `# [patch.crates-io]` in a comment is not one, and neither is
/// `pdfium-render = { ... }` inside a table.
fn table_headers(manifest: &str) -> Vec<(usize, String)> {
    manifest
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line.trim()))
        .filter(|(_, trimmed)| trimmed.starts_with('[') && trimmed.ends_with(']'))
        .map(|(number, trimmed)| (number, trimmed.to_string()))
        .collect()
}

/// The headers from [`table_headers`] that open a `[patch]` table, in any of
/// its spellings: `[patch]`, `[patch.crates-io]`, or a `[patch."<url>"]`
/// pointed at some other registry or source.
fn patch_table_headers(manifest: &str) -> Vec<(usize, String)> {
    table_headers(manifest)
        .into_iter()
        .filter(|(_, header)| header == "[patch]" || header.starts_with("[patch."))
        .collect()
}

fn manifest_text() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()))
}

#[test]
fn the_manifest_declares_no_patch_table() {
    let manifest = manifest_text();

    // The positive control. An empty result has two explanations, and "the
    // manifest is clean" is only one of them: a scan that stopped recognising
    // headers at all would report the same nothing. `[package]` is the first
    // table in every Cargo manifest, so seeing it proves the recogniser fired
    // on this file before the assertion below reads anything into its silence.
    let headers = table_headers(&manifest);
    assert!(
        headers.iter().any(|(_, header)| header == "[package]"),
        "the scan found no [package] table in Cargo.toml, so it is not reading \
         table headers and the patch check below cannot fail. Headers seen: {headers:#?}"
    );

    let patches = patch_table_headers(&manifest);
    assert!(
        patches.is_empty(),
        "Cargo.toml declares {} [patch] table(s):\n{}\n\
         A patch here applies only when this crate is the build root, it is \
         reported as a warning rather than an error when it does not apply, and \
         cargo has to fetch whatever source it names on every single build. \
         That is issue #61. Mirror the core crate's dependency versions instead.",
        patches.len(),
        patches
            .iter()
            .map(|(number, header)| format!("  Cargo.toml:{number}: {header}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[test]
fn the_scan_reads_tables_and_not_comments_about_them() {
    // The table this crate used to carry, verbatim enough to be the real case.
    let with_table = "[package]\n\
                      name = \"libviprs-bench\"\n\
                      \n\
                      [patch.crates-io]\n\
                      pdfium-render = { git = \"https://example.invalid/f.git\", branch = \"b\" }\n";
    assert_eq!(
        patch_table_headers(with_table),
        vec![(4, "[patch.crates-io]".to_string())],
        "a real [patch.crates-io] table must be caught, at its line number"
    );

    // Prose mentioning the table, which is what this very file is full of.
    let only_a_comment = "[package]\n\
                          name = \"libviprs-bench\"\n\
                          # There is no [patch.crates-io] table here, and the\n\
                          # word [patch.foo] in a sentence is not one either.\n";
    assert!(
        patch_table_headers(only_a_comment).is_empty(),
        "a comment mentioning [patch.crates-io] must not count as declaring one"
    );

    // The other spellings, so the guard is not pinned to one registry.
    for header in [
        "[patch]",
        "[patch.crates-io]",
        "[patch.\"https://example.invalid\"]",
    ] {
        let manifest = format!("[package]\nname = \"x\"\n\n{header}\nfoo = {{ path = \"f\" }}\n");
        assert_eq!(
            patch_table_headers(&manifest).len(),
            1,
            "{header} must be recognised as opening a patch table"
        );
    }
}
