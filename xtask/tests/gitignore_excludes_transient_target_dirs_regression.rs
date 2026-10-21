//! Guards the `/target*` entries in the root and fixture `.gitignore`: cargo
//! creates a missing target dir under the temp name `target` + 6 random
//! alphanumerics (`tempfile`'s default) before renaming, so a literal
//! `/target` doesn't cover it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ignore::gitignore::GitignoreBuilder;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask's manifest dir has a parent")
        .to_path_buf()
}

/// Every distinct Cargo workspace root under `repo_root` — the fixture
/// workspace resolves independently of the root one, so there are (at
/// least) two. Found by running `cargo metadata` against each `Cargo.toml`
/// the `ignore` walker turns up (which respects `.gitignore`, so it never
/// has to step into a target dir) and deduplicating by `workspace_root` —
/// cargo's own authority on what counts as a workspace root, rather than a
/// hand-rolled `[workspace]`-key check that could disagree with it.
fn workspace_roots(repo_root: &Path) -> BTreeSet<PathBuf> {
    let mut roots = BTreeSet::new();
    for entry in ignore::WalkBuilder::new(repo_root).build().flatten() {
        if entry.file_name() != "Cargo.toml" {
            continue;
        }
        let metadata = cargo_metadata::MetadataCommand::new()
            .manifest_path(entry.path())
            .no_deps()
            .exec()
            .unwrap_or_else(|e| panic!("cargo metadata for {:?} failed: {e}", entry.path()));
        roots.insert(metadata.workspace_root.into_std_path_buf());
    }
    roots
}

/// Whether `candidate` is ignored by the globs in `gitignore_file` alone.
/// Avoids `git check-ignore`, which also reads global `core.excludesFile`
/// and `.git/info/exclude` and could mask a missing entry.
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
fn ignores_target_suffix_dirs() {
    let roots = workspace_roots(&repo_root());
    assert!(!roots.is_empty(), "found no Cargo workspace roots to check");

    let mut failures = Vec::new();
    for root in &roots {
        let gitignore = root.join(".gitignore");
        if !gitignore.exists() {
            failures.push(format!("{gitignore:?}: no such file"));
            continue;
        }

        // Cargo-shaped: digits and uppercase letters, like the
        // `target3133ED` this repo's own history hit.
        if !is_ignored_by(&gitignore, &root.join("target3133ED")) {
            failures.push(format!(
                "{gitignore:?}: does not ignore target<suffix> (e.g. target3133ED)"
            ));
        }
        // `/target*` should still cover the bare literal name — a pattern
        // that stopped matching `target` itself while matching something
        // else wouldn't be caught by the suffix check alone.
        if !is_ignored_by(&gitignore, &root.join("target")) {
            failures.push(format!("{gitignore:?}: does not ignore the literal `target`"));
        }
        // Negative control: a pattern broad enough to swallow everything
        // (e.g. bare `*`) would pass both checks above. `src` isn't
        // matched by `/target*` under any correct reading of the pattern.
        if is_ignored_by(&gitignore, &root.join("src")) {
            failures.push(format!("{gitignore:?}: incorrectly ignores `src`"));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
