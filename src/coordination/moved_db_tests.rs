//! Tests that each moved-DB check is load-bearing on its own: every scenario
//! here is caught by exactly one check, so removing that check (and nothing
//! else) turns the test red.

use super::coordination_tests::temp_db;
use super::{
    coordinate, open_db, set_test_after_write_hook, set_test_retry_hook, AfterWriteSite, FileIdentity, SERIAL_NONE,
};

/// `dir/link` -> `dir/first`, with `dir/first/.skuld.db` opened through it.
/// Returns the symlink-routed path.
fn open_through_symlink(dir: &std::path::Path) -> std::path::PathBuf {
    let first = dir.join("first");
    std::fs::create_dir(&first).unwrap();
    std::os::unix::fs::symlink(&first, dir.join("link")).unwrap();
    dir.join("link").join(".skuld.db")
}

/// Atomically repoint `dir/link` at `target`.
fn retarget_link(dir: &std::path::Path, target: &std::path::Path) {
    let tmp = dir.join("link.tmp");
    std::os::unix::fs::symlink(target, &tmp).unwrap();
    std::fs::rename(&tmp, dir.join("link")).unwrap();
}

/// Only `SQLITE_FCNTL_HAS_MOVED` sees this: the caller's path still resolves
/// to the original inode (through a hard link in a retargeted directory), but
/// the path SQLite resolved at open time now names a different file.
#[test]
fn db_has_moved_is_caught_by_the_sqlite_fcntl_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = open_through_symlink(dir.path());
    let (conn, identity) = open_db(&path);

    let second = dir.path().join("second");
    std::fs::create_dir(&second).unwrap();
    let first_db = dir.path().join("first").join(".skuld.db");
    std::fs::hard_link(&first_db, second.join(".skuld.db")).unwrap();
    let other = dir.path().join("other.db");
    std::fs::write(&other, b"not a real sqlite db").unwrap();
    std::fs::rename(&other, &first_db).unwrap();
    retarget_link(dir.path(), &second);

    assert_eq!(
        FileIdentity::of(&path),
        Some(identity.main),
        "precondition: the caller's path still resolves to the original inode"
    );
    assert!(
        super::db_has_moved(&conn, &path, identity.main),
        "SQLITE_FCNTL_HAS_MOVED must report the file at SQLite's own resolved path as replaced"
    );
}

/// Only the independent `FileIdentity` comparison sees this: a symlink
/// ancestor retargeted to a directory holding a different file leaves
/// SQLite's own resolved path untouched.
#[test]
fn db_has_moved_is_caught_by_the_file_identity_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = open_through_symlink(dir.path());
    let (conn, identity) = open_db(&path);

    let second = dir.path().join("second");
    std::fs::create_dir(&second).unwrap();
    std::fs::write(second.join(".skuld.db"), b"a different file").unwrap();
    retarget_link(dir.path(), &second);

    assert!(
        !super::has_moved_via_fcntl(&conn),
        "precondition: SQLite's own resolved path is untouched"
    );
    assert!(
        super::db_has_moved(&conn, &path, identity.main),
        "the FileIdentity comparison must catch a retargeted symlink ancestor"
    );
}

/// Only `panic_on_split_lock`'s in-loop call can stop the schema write here:
/// the DB file keeps its inode (hard-linked into the replacement directory),
/// so every moved-DB check passes, and only the init lock's directory split.
/// The witness proves `INIT_SQL` never ran.
#[test]
fn open_db_schema_write_is_stopped_by_a_split_lock_alone() {
    let outer = tempfile::tempdir().unwrap();
    let profile = outer.path().join("profile");
    std::fs::create_dir(&profile).unwrap();
    let path = profile.join("test-coordination.db");

    let foreign_conn = rusqlite::Connection::open(&path).unwrap();
    foreign_conn
        .execute_batch("PRAGMA journal_mode=WAL; BEGIN EXCLUSIVE")
        .unwrap();

    let (tx, rx) = std::sync::mpsc::channel();
    let path2 = path.clone();
    let opener = std::thread::spawn(move || {
        set_test_retry_hook(tx);
        open_db(&path2);
    });
    rx.recv()
        .expect("open_db never retried — test setup is broken, not the fix");

    let aside = outer.path().join("profile-aside");
    std::fs::rename(&profile, &aside).unwrap();
    std::fs::create_dir(&profile).unwrap();
    std::fs::hard_link(aside.join("test-coordination.db"), &path).unwrap();

    foreign_conn.execute_batch("COMMIT").unwrap();
    drop(foreign_conn);

    assert!(
        opener.join().is_err(),
        "open_db must panic once its init lock's directory was replaced"
    );

    let witness = rusqlite::Connection::open(aside.join("test-coordination.db")).unwrap();
    let tables: i64 = witness
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='running'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        tables, 0,
        "INIT_SQL must not have run: the split must be caught before the write, not after it"
    );
}

/// A move landing after `coordinate`'s COMMIT succeeded must still end loud,
/// not hand back a registration in an orphaned file.
#[test]
fn coordinate_fails_loudly_when_the_db_moves_right_after_its_commit() {
    let (_dir, path) = temp_db();
    let path2 = path.clone();
    let result = std::thread::spawn(move || {
        let doomed = path2.clone();
        set_test_after_write_hook(AfterWriteSite::Coordinate, move || {
            std::fs::remove_file(&doomed).unwrap();
        });
        // Forgotten, not dropped: its own Drop would also panic on the
        // moved DB and mask a `coordinate` that returned normally.
        std::mem::forget(coordinate(&path2, "a", &[], SERIAL_NONE));
    })
    .join();
    assert!(
        result.is_err(),
        "coordinate must panic when the DB moved between its COMMIT and its return"
    );
}

/// Same for `TestRegistration::drop`: a move landing after the DELETE
/// succeeded must still panic.
#[test]
fn registration_drop_fails_loudly_when_the_db_moves_right_after_its_delete() {
    let (_dir, path) = temp_db();
    let a = coordinate(&path, "a", &[], SERIAL_NONE);
    let path2 = path.clone();
    let result = std::thread::spawn(move || {
        set_test_after_write_hook(AfterWriteSite::Drop, move || {
            std::fs::remove_file(&path2).unwrap();
        });
        drop(a);
    })
    .join();
    assert!(
        result.is_err(),
        "drop must panic when the DB moved between its DELETE and its return"
    );
}

fn io_err() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error {
            code: rusqlite::ErrorCode::SystemIoFailure,
            extended_code: rusqlite::ffi::SQLITE_IOERR_SHORT_READ,
        },
        Some("disk I/O error".to_string()),
    )
}

/// `moved_db_message_for_full` says "moved" only when a check confirms it.
#[test]
fn full_io_message_is_reported_as_is_when_nothing_moved() {
    let (_dir, path) = temp_db();
    let (conn, identity) = open_db(&path);
    let msg = super::moved_db_message_for_full(&conn, &io_err(), &path, &identity);
    assert!(
        msg.as_ref().is_some_and(|m| !m.contains("deleted or replaced mid-run")
            && m.contains(path.to_str().unwrap())
            && m.contains("extended code")
            && m.contains("system errno")),
        "got {msg:?}"
    );
}

/// A lost `-wal` alone is a confirmed move for the companion-aware message.
#[test]
fn full_io_message_says_moved_when_only_a_companion_was_lost() {
    let (_dir, path) = temp_db();
    let (conn, identity) = open_db(&path);
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    std::fs::remove_file(&wal).unwrap();
    let msg = super::moved_db_message_for_full(&conn, &io_err(), &path, &identity);
    assert!(
        msg.as_ref().is_some_and(|m| m.contains("deleted or replaced mid-run")),
        "got {msg:?}"
    );
}
