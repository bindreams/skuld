//! Directory links for tests that retarget an ancestor of a coordination directory. Included with
//! `#[path]` by `src/coordination/registry_tests.rs` and `tests/drop_panic_during_unwind_cli.rs`,
//! so there is one copy of the platform-specific code.

use std::path::Path;

/// Make `link` a directory link to `target`: a symlink on Unix, a junction on Windows, which
/// needs no privilege.
pub fn link_dir(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    junction::create(target, link).unwrap();
}

/// Point the existing directory link `link` at `target`.
pub fn retarget_dir(target: &Path, link: &Path) {
    // A symlink is removed with `remove_file` on Unix, a junction with `remove_dir` on Windows.
    #[cfg(unix)]
    std::fs::remove_file(link).unwrap();
    #[cfg(windows)]
    std::fs::remove_dir(link).unwrap();
    link_dir(target, link);
}
