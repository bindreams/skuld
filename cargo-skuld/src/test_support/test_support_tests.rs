use crate::test_support::*;

#[cfg(unix)]
fn link_directory(target: &std::path::Path, link: &std::path::Path) {
    std::os::unix::fs::symlink(target, link).expect("symlink");
}

#[cfg(windows)]
fn link_directory(target: &std::path::Path, link: &std::path::Path) {
    // A directory junction, not a symlink: junctions need no elevated
    // privilege (`SeCreateSymbolicLinkPrivilege`) or Developer Mode, unlike
    // `std::os::windows::fs::symlink_dir` — which would make this test
    // fail in ordinary CI, not just prove the property it's for.
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .status()
        .expect("spawn mklink /J");
    assert!(status.success(), "mklink /J failed");
}

/// The concrete regression this guards: an earlier version hashed
/// `target_dir`'s path as given, so a symlinked `CARGO_TARGET_DIR` and its
/// real path — two different strings for the same physical directory —
/// hashed to two different lock files that didn't exclude each other at
/// all. The current design (locking a file *inside* the target
/// directory) doesn't compute a name from the path at all, so it doesn't
/// need canonicalizing or even comparing the two paths as strings —
/// opening `<target>/.skuld-fixture.lock` through a symlink/junction and
/// through the real path resolves to the same underlying file, and
/// `flock`/`LockFileEx` contend on that, not on the path string used to
/// reach it. Verified behaviorally: lock via the real path, then a
/// second, independent handle opened through the symlink/junction must
/// observe `WouldBlock` — not just compare equal as strings, which
/// doesn't actually prove the two exclude each other.
#[test]
fn symlinked_target_dir_locks_the_same_underlying_file_as_its_real_path() {
    let base = tempfile::tempdir().expect("tempdir");
    let real_target = base.path().join("real-target");
    std::fs::create_dir(&real_target).expect("mkdir real-target");
    let linked_target = base.path().join("linked-target");
    link_directory(&real_target, &linked_target);

    let real_path = lock_file_path(&real_target);
    let real_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&real_path)
        .expect("open lock file via real path");
    lock_exclusive(&real_file).expect("lock via real path");

    let linked_path = lock_file_path(&linked_target);
    let linked_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&linked_path)
        .expect("open lock file via symlink/junction");
    match try_lock_exclusive(&linked_file) {
        Err(std::fs::TryLockError::WouldBlock) => {}
        other => panic!(
            "a target dir reached via a symlink/junction must lock the same underlying file as \
             its real path — otherwise the two don't exclude each other; got {other:?}"
        ),
    }
}

/// Guards the mechanism `metadata_tests`/`discovery_tests`/`gen_and_run.rs`
/// actually rely on: that `lock_fixture_workspace` really acquires an
/// exclusive lock a second handle is excluded from, not just that today's
/// tests happen to pass. A mutant that deleted the `lock_exclusive` call
/// inside `lock_fixture_workspace` still passed every other fixture-
/// touching test (measured). `flock`/`LockFileEx` exclude a second handle
/// opened in the same process just as they would one opened by a separate
/// process — no subprocess needed to prove it, and no timing: the second
/// handle's `try_lock_exclusive` only ever runs after `lock_fixture_
/// workspace` has already returned with the first lock held.
#[test]
fn lock_fixture_workspace_really_locks_something_a_second_handle_is_excluded_from() {
    let _guard = lock_fixture_workspace();

    let path = lock_file_path(_guard.target_dir());
    let second_handle = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("open a second handle on {path:?}: {e}"));
    match try_lock_exclusive(&second_handle) {
        Err(std::fs::TryLockError::WouldBlock) => {}
        other => panic!(
            "a second handle on the fixture lock file must observe WouldBlock while \
             lock_fixture_workspace's guard is still held; got {other:?}"
        ),
    }
}

/// Guards `ensure_target_dir`'s use of `cargo_util::paths::
/// create_dir_all_excluded_from_backups_atomic` — that a freshly created
/// target directory is actually marked the way cargo's own target-dir
/// init marks one, not just that the directory exists. A plain
/// `std::fs::create_dir_all` here would satisfy every other fixture-
/// touching test (none of them check these marks) while silently
/// reintroducing the Time Machine/iCloud/content-indexing regression this
/// function exists to avoid. Confirmed RED: swapping the real
/// implementation for `std::fs::create_dir_all(target_dir).unwrap()`
/// fails this test's `CACHEDIR.TAG` assertion.
#[test]
fn ensure_target_dir_marks_a_fresh_directory_exactly_the_way_cargo_would() {
    let base = tempfile::tempdir().expect("tempdir");
    let target_dir = base.path().join("fresh-target");

    ensure_target_dir(&target_dir);

    assert!(target_dir.is_dir(), "ensure_target_dir must create the directory");
    assert!(
        target_dir.join("CACHEDIR.TAG").exists(),
        "must write CACHEDIR.TAG the same way cargo's own target-dir init does"
    );

    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("tmutil")
            .arg("isexcluded")
            .arg(&target_dir)
            .output()
            .expect("spawn tmutil isexcluded");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("[Excluded]"),
            "target dir must be excluded from Time Machine backups; tmutil said: {stdout}"
        );
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
        const FILE_ATTRIBUTE_NOT_CONTENT_INDEXED: u32 = 0x2000;
        let attrs = std::fs::metadata(&target_dir)
            .expect("stat target dir")
            .file_attributes();
        assert!(
            attrs & FILE_ATTRIBUTE_NOT_CONTENT_INDEXED != 0,
            "target dir must have FILE_ATTRIBUTE_NOT_CONTENT_INDEXED set, got attrs = {attrs:#x}"
        );
    }
}
