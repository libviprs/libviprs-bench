// How to read `git status` run twice, once comparing file modes and once not.
//
// Shared verbatim with `build.rs` by `include!`, so no inner doc comments and no
// `mod` wrapper, and nothing here may touch the rest of the crate. A build
// script cannot use its own crate's library, and this needed to be reachable
// from a test: it was written inside `build.rs` with its two arguments bound the
// wrong way round, which inverted the published dirty flag AND made the note
// unreachable, and nothing covered any of it.

/// Whether a tree is dirty only because git is comparing file modes.
///
/// `ignoring` is `git status --porcelain -uno` with `core.fileMode=false`, and
/// `comparing` is the same with it on. Comparing is a superset of ignoring: a
/// mode change is a change git can see only when it is looking at modes. So the
/// only interesting disagreement is clean-without-modes and dirty-with-them,
/// which is exactly what a share that forces 0777 does to every file in a tree
/// nobody edited.
///
/// The condition used to be `ignoring && !comparing`, which is the other way
/// round and therefore never true.
pub fn mode_only_dirt(ignoring: bool, comparing: bool) -> Option<&'static str> {
    (comparing && !ignoring).then_some(
        "the tree reads as dirty only while git compares file modes, which is what a share \
         that forces 0777 does to every file; the dirty flag below ignores mode-only \
         differences",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // RED against the swapped bindings, which is how this shipped: the note
    // fired on `ignoring && !comparing`, and no tree can be dirty ignoring modes
    // and clean comparing them, so the branch was dead and the 0777 share it was
    // written for was refused in silence.
    #[test]
    fn mode_only_dirt_fires_on_the_case_it_was_written_for() {
        // The 0777 share: clean until git looks at modes.
        assert!(mode_only_dirt(false, true).is_some());

        // A tree with real edits is dirty both ways, and saying "only the modes"
        // about it would be wrong.
        assert!(mode_only_dirt(true, true).is_none());

        // A clean tree is clean.
        assert!(mode_only_dirt(false, false).is_none());

        // And the impossible combination is not a note either. Comparing modes
        // is a superset of ignoring them, so this cannot happen; if it ever
        // does, something is wrong upstream and inventing a sentence about file
        // modes would be the wrong answer.
        assert!(mode_only_dirt(true, false).is_none());
    }
}
