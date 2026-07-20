//! Pure version-identity and ordering logic for the release-history axis.
//!
//! These helpers are side-effect-free domain logic: they turn a snapshot's
//! `(version, git_sha, timestamp)` into a stable column key and a release
//! ordering. They live apart from [`crate::version_matrix`] — which shells out
//! to `git`/`cargo`, manages worktrees, and spawns benchmark suites — so the
//! reporting binary (`cross_version`) can depend on identity alone without
//! being compile-coupled to the process-orchestration runner (issue #19).
//!
//! Version identity is keyed by `version@short_sha` ([`version_key`]) so two
//! builds of the same version at different commits stay distinct, and releases
//! order by (semver, timestamp) ([`ordered_version_keys`]) so `0.10.0` sorts
//! after `0.9.0` rather than lexically before `0.3.1`.

use std::collections::HashSet;

use crate::BenchmarkSnapshot;

/// Version identity key: `version@short_sha`, so two builds of the same
/// version at different commits stay distinct (issue #19).
///
/// Falls back to the bare version when the SHA is empty or `"unknown"` (legacy
/// history predating the SHA field), so those snapshots still group sanely.
pub fn version_key(version: &str, git_sha: &str) -> String {
    if git_sha.is_empty() || git_sha == "unknown" {
        version.to_string()
    } else {
        format!("{version}@{git_sha}")
    }
}

/// The version keys of `history`, deduplicated and ordered by (semver,
/// timestamp) — the ordering `cross_version` presents releases in.
///
/// Semver ordering (not lexicographic) means `0.9.0` precedes `0.10.0`; ties on
/// version are broken by the RFC 3339 timestamp (chronological), and any
/// unparseable version sorts last but deterministically.
pub fn ordered_version_keys(history: &[BenchmarkSnapshot]) -> Vec<String> {
    let mut items: Vec<(String, (u64, u64, u64), String)> = history
        .iter()
        .map(|s| {
            (
                version_key(&s.version, &s.git_sha),
                semver_sort_key(&s.version),
                s.timestamp.clone(),
            )
        })
        .collect();
    // (semver, timestamp, key) — the trailing key makes the order total and
    // stable across equal (semver, timestamp) pairs.
    items.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.cmp(&b.2)).then(a.0.cmp(&b.0)));

    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter_map(|(key, _, _)| seen.insert(key.clone()).then_some(key))
        .collect()
}

/// Parse a `MAJOR.MINOR.PATCH` version (tolerating a leading `v` and a
/// `-pre`/`+build` suffix) into a numerically sortable tuple. Anything that
/// isn't three integer components sorts last via an all-`MAX` key.
///
/// The pre-release/build suffix is dropped, so `0.4.0-rc.1` and `0.4.0` share a
/// key and order only by their timestamps. Pre-release *precedence* is not
/// modelled — the release-history axis records concrete builds, not a semver
/// precedence ladder, and no column is expected to distinguish a pre-release
/// from its release beyond the `@short_sha` suffix ([`version_key`]).
fn semver_sort_key(version: &str) -> (u64, u64, u64) {
    let core = version
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()
        .unwrap_or("");
    let mut it = core.split('.');
    let next = |it: &mut std::str::Split<'_, char>| it.next().and_then(|s| s.parse::<u64>().ok());
    match (next(&mut it), next(&mut it), next(&mut it)) {
        (Some(a), Some(b), Some(c)) => (a, b, c),
        _ => (u64::MAX, u64::MAX, u64::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_key_orders_numerically_not_lexically() {
        assert!(semver_sort_key("0.9.0") < semver_sort_key("0.10.0"));
        assert!(semver_sort_key("0.3.1") < semver_sort_key("0.9.0"));
        // Leading v and pre-release suffix are tolerated.
        assert_eq!(semver_sort_key("v0.3.1"), (0, 3, 1));
        assert_eq!(semver_sort_key("0.4.0-rc.1"), (0, 4, 0));
        // Unparseable sorts last.
        assert_eq!(semver_sort_key("nightly"), (u64::MAX, u64::MAX, u64::MAX));
        assert!(semver_sort_key("9.9.9") < semver_sort_key("nightly"));
    }

    #[test]
    fn version_key_uses_sha_when_known() {
        assert_eq!(version_key("0.3.1", "abc1234"), "0.3.1@abc1234");
        assert_eq!(version_key("0.3.1", ""), "0.3.1");
        assert_eq!(version_key("0.3.1", "unknown"), "0.3.1");
    }
}
