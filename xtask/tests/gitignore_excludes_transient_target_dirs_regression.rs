//! Guards the mechanism behind the repo's `/target*` `.gitignore` entries
//! (fixture and repo root): a *literal* `/target` doesn't cover the
//! directory cargo creates while atomically initializing a target
//! directory it finds missing — a create-under-a-temporary-name-then-
//! rename, where the temporary name is `target` plus a 6-character random
//! alphanumeric suffix (`tempfile`'s own default `NUM_RAND_CHARS`; this is
//! where CI run 36320721947's `target3133ED` came from — alphanumeric, not
//! specifically hex, even though that particular suffix happened to look
//! hex-shaped). A scan that respects `.gitignore` at all still steps into
//! a literally-unlisted `target<suffix>` mid-rename; the wildcard excludes
//! it categorically. Reverting either `/target*` back to `/target` leaves
//! every other test in the workspace green — this is the only thing that
//! would catch it.

use ignore::gitignore::GitignoreBuilder;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask's manifest dir has a parent")
        .to_path_buf()
}

/// Whether `candidate` is ignored by exactly the globs in `gitignore_file`
/// — nothing else. Deliberately does not shell out to `git check-ignore`:
/// that also consults the current user's (or CI runner's) global excludes
/// file and this repo's `.git/info/exclude`, neither of which this repo
/// controls or this test can see from its source. Measured directly: with
/// a global `core.excludesFile` containing a `target*` pattern,
/// `git check-ignore` reports "ignored" even with the literal, unfixed
/// `/target` entry still in this repo's own `.gitignore` — which would
/// make this regression test pass while the bug it exists to catch was
/// still there. `GitignoreBuilder` parses gitignore syntax itself (the
/// same engine `ripgrep` and `cargo`'s own `ignore`-crate-based tooling
/// use) from exactly the one file handed to `add`, with no notion of a
/// global excludes file or any other `.gitignore` to consult.
fn is_ignored_by(gitignore_file: &Path, candidate: &Path) -> bool {
    let dir = gitignore_file.parent().expect("gitignore file has a parent directory");
    let mut builder = GitignoreBuilder::new(dir);
    if let Some(err) = builder.add(gitignore_file) {
        panic!("failed to parse {gitignore_file:?}: {err}");
    }
    let matcher = builder.build().expect("build gitignore matcher");
    matcher.matched(candidate, /* is_dir */ true).is_ignore()
}

#[test]
fn gitignore_excludes_transient_target_dirs_in_the_fixture_and_at_the_repo_root() {
    let root = repo_root();
    let fixture_gitignore = root.join("cargo-skuld/tests/fixtures/test-workspace/.gitignore");
    let root_gitignore = root.join(".gitignore");

    assert!(
        is_ignored_by(
            &fixture_gitignore,
            &fixture_gitignore.parent().unwrap().join("targetXk9mQz")
        ),
        "cargo-skuld/tests/fixtures/test-workspace/.gitignore must ignore target<suffix> \
         directories, not just the literal `target`"
    );
    assert!(
        is_ignored_by(&root_gitignore, &root.join("targetXk9mQz")),
        "the repo root .gitignore must ignore target<suffix> directories, not just the literal \
         `target`"
    );
}
