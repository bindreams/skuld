//! Tests for the Unix-only atomic publish machinery. See
//! [`super::publish`]'s module doc for the one requirement this exercises.
//!
//! `publish_creates_three_0666_files_despite_umask` and
//! `concurrent_publishers_all_converge_on_one_0666_file` are not here: both
//! need genuine subprocesses (the former because `umask` is process-global;
//! the latter because SQLite's locking is per-process, so in-process
//! threads racing to publish would share lock state that genuinely separate
//! processes don't — see `tests/coordination_publish_cli.rs`'s module doc)
//! and live in `tests/coordination_publish_cli.rs` instead.

use std::os::unix::fs::{MetadataExt, PermissionsExt};

use super::publish::{
    classify_rename_errno, create_publish_temp, create_publish_temp_with, ensure_published, ensure_published_with,
    handle_rename_result, rename_publish_temp, RenameError,
};

/// Downcast a `catch_unwind` payload to its panic message.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        panic!("panic payload was not a string")
    }
}

#[test]
fn a_lost_publish_race_uses_the_winners_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join(".skuld.db");

    // Two temps, simulating two processes racing to publish the same target.
    let winner_tmp = create_publish_temp(dir.path());
    std::fs::write(&winner_tmp, b"winner").unwrap();
    let loser_tmp = create_publish_temp(dir.path());
    std::fs::write(&loser_tmp, b"loser").unwrap();

    rename_publish_temp(dir.path(), &winner_tmp, &target);
    assert_eq!(std::fs::read(&target).unwrap(), b"winner");
    assert!(!winner_tmp.exists(), "the winner's temp is gone once renamed in");

    // The loser must not panic (EEXIST is not an error) and must discard
    // its own temp rather than touch the winner's file.
    rename_publish_temp(dir.path(), &loser_tmp, &target);
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"winner",
        "the winner's file must be untouched"
    );
    assert!(!loser_tmp.exists(), "a lost race must remove the loser's own temp");
}

#[test]
fn create_publish_temp_retries_past_a_name_collision() {
    // Simulates two processes in different pid namespaces landing on the
    // same candidate name (see `create_publish_temp_with`'s doc): the first
    // candidate is already taken by an unrelated file, so the call must
    // retry onto the next candidate rather than panicking.
    let dir = tempfile::tempdir().unwrap();
    let collided = dir.path().join(".skuld-publish-collide.tmp");
    std::fs::write(&collided, b"someone else's temp").unwrap();
    let free_path = dir.path().join(".skuld-publish-free.tmp");

    let mut calls = 0u32;
    let tmp = create_publish_temp_with(dir.path(), |_| {
        calls += 1;
        if calls == 1 {
            collided.clone()
        } else {
            free_path.clone()
        }
    });

    assert_eq!(calls, 2, "must retry exactly once past the single collision");
    assert_eq!(tmp, free_path, "must land on the fresh candidate");
    assert_eq!(
        std::fs::read(&collided).unwrap(),
        b"someone else's temp",
        "the colliding file must be left untouched"
    );
    let meta = std::fs::metadata(&tmp).unwrap();
    assert_eq!(meta.mode() & 0o777, 0o666);
}

#[test]
fn classify_rename_errno_maps_known_errnos() {
    assert_eq!(classify_rename_errno(libc::EEXIST), RenameError::AlreadyExists);
    assert_eq!(
        classify_rename_errno(libc::EINVAL),
        RenameError::Unsupported(libc::EINVAL)
    );
    assert_eq!(
        classify_rename_errno(libc::ENOTSUP),
        RenameError::Unsupported(libc::ENOTSUP)
    );
    assert_eq!(
        classify_rename_errno(libc::ENOSYS),
        RenameError::KernelTooOld(libc::ENOSYS)
    );
    assert_eq!(classify_rename_errno(libc::EACCES), RenameError::Other(libc::EACCES));
}

/// `RenameError::KernelTooOld` (an old kernel's `renameat2` returning
/// `ENOSYS`) can't be produced by a real rename on any kernel this test
/// suite runs on, so this drives `handle_rename_result` directly with a
/// synthetic `RenameError` instead of going through `atomic_rename_no_replace`.
#[test]
fn a_kernel_too_old_error_panics_naming_the_kernel_not_the_filesystem() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = create_publish_temp(dir.path());
    let target = dir.path().join(".skuld.db");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handle_rename_result(dir.path(), &tmp, &target, Err(RenameError::KernelTooOld(libc::ENOSYS)))
    }));

    let err = result.expect_err("a kernel-too-old error must panic");
    let msg = panic_message(err.as_ref());
    assert!(
        msg.contains("kernel"),
        "message should blame the kernel, not the filesystem: {msg}"
    );
    assert!(
        !msg.contains("filesystem"),
        "message should not reuse Unsupported's filesystem wording: {msg}"
    );
    assert!(
        !tmp.exists(),
        "the temp must be cleaned up even though the rename never ran"
    );
}

/// Mirrors the `KernelTooOld` test above for `RenameError::Unsupported`
/// (`EINVAL`/`ENOTSUP`): also not producible by a real rename in this test
/// environment (it needs a filesystem that rejects `RENAME_NOREPLACE`
/// outright, e.g. some NFS configurations), so driven synthetically too.
#[test]
fn an_unsupported_error_panics_naming_the_filesystem_not_the_kernel() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = create_publish_temp(dir.path());
    let target = dir.path().join(".skuld.db");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handle_rename_result(dir.path(), &tmp, &target, Err(RenameError::Unsupported(libc::EINVAL)))
    }));

    let err = result.expect_err("an unsupported-filesystem error must panic");
    let msg = panic_message(err.as_ref());
    assert!(
        msg.contains("filesystem"),
        "message should blame the filesystem, not the kernel: {msg}"
    );
    assert!(
        !msg.contains("kernel"),
        "message should not reuse KernelTooOld's kernel wording: {msg}"
    );
    assert!(
        !tmp.exists(),
        "the temp must be cleaned up even though the rename never ran"
    );
}

#[test]
fn a_non_eexist_publish_error_is_named() {
    let dir = tempfile::tempdir().unwrap();

    // A structural failure (ENOTDIR: a path component that should be a
    // directory is a regular file instead), not a permissions failure. A
    // read-only *directory* would also make the rename fail on an ordinary
    // run, but root / CAP_DAC_OVERRIDE bypasses DAC permission checks
    // entirely and would make the rename silently succeed instead — turning
    // this into a false pass rather than exercising the panic path. ENOTDIR
    // isn't a permission check to bypass: no privilege level makes a regular
    // file behave as a directory.
    let not_a_dir = dir.path().join("not-a-dir");
    std::fs::write(&not_a_dir, b"").unwrap();
    let target = not_a_dir.join(".skuld.db");

    let tmp = create_publish_temp(dir.path());

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rename_publish_temp(dir.path(), &tmp, &target)
    }));

    let _ = std::fs::remove_file(&tmp);

    let err = result.expect_err("a non-EEXIST rename failure must panic");
    let msg = panic_message(err.as_ref());
    assert!(
        msg.contains("could not publish"),
        "message should name the failure: {msg}"
    );
    assert!(msg.contains(".skuld.db"), "message should name the target file: {msg}");
}

#[test]
fn a_vanished_own_temp_panics_loudly() {
    // Nothing in this module ever removes a temp it didn't create itself, so
    // a rename whose own source has vanished is not a benign, retryable
    // race — it's an unexplained failure, and must panic like any other
    // non-EEXIST rename error.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join(".skuld.db");

    let tmp = create_publish_temp(dir.path());
    std::fs::remove_file(&tmp).unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rename_publish_temp(dir.path(), &tmp, &target)
    }));

    let err = result.expect_err("a vanished own-temp must panic, not retry silently");
    let msg = panic_message(err.as_ref());
    assert!(
        msg.contains("could not publish"),
        "message should name the failure: {msg}"
    );
    assert!(!target.exists(), "nothing should have been published");
}

#[test]
fn ensure_published_creates_an_absent_file_at_0666() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join(".skuld.db");

    ensure_published(&target);

    let meta = std::fs::metadata(&target).unwrap_or_else(|e| panic!("{target:?} must exist: {e}"));
    assert!(meta.file_type().is_file());
    assert_eq!(meta.mode() & 0o777, 0o666);
}

#[test]
fn ensure_published_leaves_an_existing_file_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join(".skuld.db");
    std::fs::write(&target, b"already here").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();

    ensure_published(&target);

    // No verification, no chmod: an already-existing file — even one with a
    // narrow mode from before this uid's involvement — is used as-is.
    assert_eq!(std::fs::read(&target).unwrap(), b"already here");
    assert_eq!(std::fs::metadata(&target).unwrap().mode() & 0o777, 0o600);
}

/// The fast path this test pins: `connect()` runs on every connection, not
/// just the first, so an unconditional publish attempt (temp create, fchmod,
/// rename — three syscalls plus a retry loop) on every single one is wasted
/// work the moment `.skuld.db` already exists. An `EEXIST`-only no-op (what
/// `ensure_published` falls back to without this pre-check) still does the
/// temp create and fchmod before discovering that on the rename. A plain
/// existence check (lstat, not a full open+read) must skip the publish
/// attempt entirely instead.
#[test]
fn ensure_published_skips_publishing_when_the_target_already_exists() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join(".skuld.db");
    std::fs::write(&target, b"already here").unwrap();

    let mut called = false;
    ensure_published_with(&target, |_dir, _target| called = true);

    assert!(
        !called,
        "the publish closure must not run at all when the target already exists"
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"already here");
}

/// A dangling symlink at the target path counts as "already exists" for the
/// fast path too: `lstat`/`symlink_metadata` succeeds against the link
/// itself even though what it points to is gone. Skipping the publish here
/// is what the module doc's "even a dangling symlink" already assumes on the
/// `rename`'s `EEXIST` side; the fast path must agree, not attempt a publish
/// that the rename would just reject anyway.
#[test]
fn ensure_published_skips_publishing_for_a_dangling_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join(".skuld.db");
    std::os::unix::fs::symlink(dir.path().join("does-not-exist"), &target).unwrap();

    let mut called = false;
    ensure_published_with(&target, |_dir, _target| called = true);

    assert!(
        !called,
        "the publish closure must not run at all when the target is a dangling symlink"
    );
    let meta = std::fs::symlink_metadata(&target).unwrap();
    assert!(meta.file_type().is_symlink(), "the symlink must be left untouched");
}

/// Mirrors `ensure_published_creates_an_absent_file_at_0666`, but through the
/// injectable seam: confirms the fast path's absence check doesn't itself
/// false-positive and skip a genuinely absent target.
#[test]
fn ensure_published_publishes_when_the_target_is_absent() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join(".skuld.db");

    let mut called = false;
    ensure_published_with(&target, |_dir, _target| called = true);

    assert!(called, "the publish closure must run when the target is absent");
}
