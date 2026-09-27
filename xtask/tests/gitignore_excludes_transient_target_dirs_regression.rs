//! Guards the mechanism behind the repo's `/target*` `.gitignore` entries
//! (fixture and repo root): a *literal* `/target` doesn't cover
//! `target<hex>`, the temporary name cargo's atomic
//! create-under-a-temp-name-then-rename uses while initializing a target
//! directory it finds missing (this is where CI run 36320721947's
//! `target3133ED` came from). A scan that respects `.gitignore` at all
//! still steps into a literally-unlisted `target<hex>` mid-rename; the
//! wildcard excludes it categorically. Reverting either `/target*` back
//! to `/target` leaves every other test in the workspace green — this is
//! the only thing that would catch it.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask's manifest dir has a parent")
        .to_path_buf()
}

/// `git check-ignore --no-index -q` — `--no-index` so this works for a
/// path that doesn't exist on disk (`target<hex>` is transient; nothing
/// should have to create one just to test this), `-q` to suppress the
/// matched-pattern output we don't need. Exit 0 means ignored, exit 1
/// means not ignored; any other exit code is a real error (e.g. a
/// malformed path or `git` itself failing) that must not be silently
/// folded into "not ignored".
fn is_git_ignored(repo_root: &Path, relative_path: &str) -> bool {
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(["check-ignore", "--no-index", "-q", relative_path])
        .output()
        .expect("spawn git check-ignore");
    match output.status.code() {
        Some(0) => true,
        Some(1) => false,
        _ => panic!(
            "git check-ignore exited with {:?} for {relative_path:?}, expected 0 or 1. stderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

#[test]
fn gitignore_excludes_transient_target_dirs_in_the_fixture_and_at_the_repo_root() {
    let root = repo_root();
    assert!(
        is_git_ignored(&root, "cargo-skuld/tests/fixtures/test-workspace/target0A1B2C"),
        "cargo-skuld/tests/fixtures/test-workspace/.gitignore must ignore target<hex> \
         directories, not just the literal `target`"
    );
    assert!(
        is_git_ignored(&root, "target0A1B2C"),
        "the repo root .gitignore must ignore target<hex> directories, not just the literal \
         `target`"
    );
}
