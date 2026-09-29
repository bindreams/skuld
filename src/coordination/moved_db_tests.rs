//! Tests that each moved-DB check is load-bearing on its own: every scenario
//! here is caught by exactly one check, so removing that check (and nothing
//! else) turns the test red.

use super::coordination_tests::temp_db;
use super::moved_db::{
    companion_path, has_moved_via_fcntl, open_file_identities, scan_open_file_identities, DbIdentity, Failure,
    FileIdentity,
};
use super::test_hooks::{retry_rendezvous, set_test_retry_hook, set_test_seam_hook, Seam};
use super::{coordinate, open_db, SERIAL_NONE};

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
    // Main file only: the companions are not what this test isolates.
    let identity = DbIdentity {
        companions: None,
        ..identity
    };

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
        identity.has_moved(&conn, &path),
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
    // Main file only: the companions are not what this test isolates.
    let identity = DbIdentity {
        companions: None,
        ..identity
    };

    let second = dir.path().join("second");
    std::fs::create_dir(&second).unwrap();
    std::fs::write(second.join(".skuld.db"), b"a different file").unwrap();
    retarget_link(dir.path(), &second);

    assert!(
        !has_moved_via_fcntl(&conn),
        "precondition: SQLite's own resolved path is untouched"
    );
    assert!(
        identity.has_moved(&conn, &path),
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

    let (worker, retry) = retry_rendezvous();
    let path2 = path.clone();
    let opener = std::thread::spawn(move || {
        let _hook = set_test_retry_hook(worker);
        open_db(&path2);
    });
    retry.wait_for_retry();

    let aside = outer.path().join("profile-aside");
    std::fs::rename(&profile, &aside).unwrap();
    std::fs::create_dir(&profile).unwrap();
    std::fs::hard_link(aside.join("test-coordination.db"), &path).unwrap();

    foreign_conn.execute_batch("COMMIT").unwrap();
    drop(foreign_conn);
    retry.release();

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

/// The joined thread panicked with the moved-DB message naming `path`, not for
/// an unrelated reason (a hook's own `unwrap`, another check, another bug).
fn assert_moved_panic(result: std::thread::Result<()>, path: &std::path::Path) {
    let payload = result.expect_err("the thread must have panicked");
    let msg = crate::coordination::panic_payload_message(payload.as_ref());
    assert!(
        msg.contains(&format!("{path:?}")) && msg.contains("deleted or replaced mid-run"),
        "the panic must be the moved-DB message naming {path:?}: {msg:?}"
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
        let _hook = set_test_seam_hook(Seam::Commit, move || {
            std::fs::remove_file(&doomed).unwrap();
        });
        // Forgotten, not dropped: its own Drop would also panic on the
        // moved DB and mask a `coordinate` that returned normally.
        std::mem::forget(coordinate(&path2, "a", &[], SERIAL_NONE));
    })
    .join();
    assert_moved_panic(result, &path);
}

/// Same for `TestRegistration::drop`: a move landing after the DELETE
/// succeeded must still panic.
#[test]
fn registration_drop_fails_loudly_when_the_db_moves_right_after_its_delete() {
    let (_dir, path) = temp_db();
    let a = coordinate(&path, "a", &[], SERIAL_NONE);
    let path2 = path.clone();
    let result = std::thread::spawn(move || {
        let _hook = set_test_seam_hook(Seam::Delete, move || {
            std::fs::remove_file(&path2).unwrap();
        });
        drop(a);
    })
    .join();
    assert_moved_panic(result, &path);
}

/// A SQLite failure of `code`, captured against `conn` as production would.
fn failure(conn: &rusqlite::Connection, code: rusqlite::ErrorCode, extended: i32, text: &str) -> Failure {
    let err = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error {
            code,
            extended_code: extended,
        },
        Some(text.to_owned()),
    );
    Failure::capture(conn, err)
}

fn io_failure(conn: &rusqlite::Connection) -> Failure {
    failure(
        conn,
        rusqlite::ErrorCode::SystemIoFailure,
        rusqlite::ffi::SQLITE_IOERR_SHORT_READ,
        "disk I/O error",
    )
}

/// One failure per error class the message builder must handle identically.
fn every_class(conn: &rusqlite::Connection) -> Vec<Failure> {
    use rusqlite::ffi::*;
    use rusqlite::ErrorCode::*;
    vec![
        io_failure(conn),
        failure(conn, DiskFull, SQLITE_FULL, "database or disk is full"),
        failure(
            conn,
            DatabaseCorrupt,
            SQLITE_CORRUPT,
            "database disk image is malformed",
        ),
        failure(conn, CannotOpen, SQLITE_CANTOPEN, "unable to open database file"),
        failure(conn, ReadOnly, SQLITE_READONLY, "attempt to write a readonly database"),
        failure(conn, NotADatabase, SQLITE_NOTADB, "file is not a database"),
        failure(conn, DatabaseBusy, SQLITE_BUSY, "database is locked"),
    ]
}

/// Nothing moved: every class is reported as what it is, naming the path and
/// SQLite's extended code, and never mislabelled as a move.
#[test]
fn failure_message_reports_every_class_as_is_when_nothing_moved() {
    let (_dir, path) = temp_db();
    let (conn, identity) = open_db(&path);
    for f in every_class(&conn) {
        let msg = identity.failure_message(&conn, &f, &path, "some context");
        assert!(
            msg.contains("some context")
                && msg.contains(&format!("{path:?}"))
                && msg.contains("extended code")
                && msg.contains("system errno")
                && !msg.contains("deleted or replaced mid-run"),
            "{:?} must be reported as-is: {msg:?}",
            f.error()
        );
    }
}

/// A full disk is I/O-class: its message names the path, the extended code and
/// the disk-full text, and says SQLite recorded no errno for it (its errno
/// would be stale).
#[test]
fn failure_message_names_a_full_disk() {
    let (_dir, path) = temp_db();
    let (conn, identity) = open_db(&path);
    let full = failure(
        &conn,
        rusqlite::ErrorCode::DiskFull,
        rusqlite::ffi::SQLITE_FULL,
        "database or disk is full",
    );

    let msg = identity.failure_message(&conn, &full, &path, "ctx");

    assert!(
        msg.contains(&format!("{path:?}"))
            && msg.contains("database or disk is full")
            && msg.contains(&format!("extended code: Some({})", rusqlite::ffi::SQLITE_FULL))
            && msg.contains("none recorded"),
        "{msg:?}"
    );
}

/// Once the DB moved, every error class gets the clear "moved" message: any of
/// them can be the symptom of a swapped `-wal`/`-shm`.
#[test]
fn failure_message_says_moved_for_every_class_when_the_db_moved() {
    let (_dir, path) = temp_db();
    let (conn, identity) = open_db(&path);
    std::fs::remove_file(&path).unwrap();
    for f in every_class(&conn) {
        let msg = identity.failure_message(&conn, &f, &path, "ctx");
        assert!(
            msg.contains("deleted or replaced mid-run") && msg.contains(&format!("{path:?}")),
            "{:?} must say moved once the DB moved: {msg:?}",
            f.error()
        );
    }
}

/// A lost `-wal` alone is a confirmed move.
#[test]
fn failure_message_says_moved_when_only_a_companion_was_lost() {
    let (_dir, path) = temp_db();
    let (conn, identity) = open_db(&path);
    std::fs::remove_file(companion_path(&path, "-wal")).unwrap();
    let msg = identity.failure_message(&conn, &io_failure(&conn), &path, "ctx");
    assert!(msg.contains("deleted or replaced mid-run"), "{msg:?}");
}

/// The errno is read when the failure is captured, not when the message is
/// built: a later failure on the same connection (a best-effort ROLLBACK, say)
/// overwrites `sqlite3_system_errno`, and would otherwise be paired with the
/// original error.
#[cfg(unix)]
#[test]
fn a_captured_failure_keeps_its_errno_after_a_later_failure_overwrites_it() {
    let dir = tempfile::tempdir().unwrap();
    let (_db_dir, path) = temp_db();
    let (conn, identity) = open_db(&path);
    let attach = |target: &std::path::Path| {
        conn.execute_batch(&format!("ATTACH DATABASE '{}' AS other", target.display()))
            .expect_err("attach must fail")
    };
    // ENOENT: the directory does not exist.
    let first = Failure::capture(&conn, attach(&dir.path().join("missing").join("x.db")));
    // ENOTDIR: a regular file where a directory is needed.
    std::fs::write(dir.path().join("file"), b"").unwrap();
    let _second = Failure::capture(&conn, attach(&dir.path().join("file").join("x.db")));

    let msg = identity.failure_message(&conn, &first, &path, "ctx");

    assert!(
        msg.contains(&format!("system errno: {}", libc::ENOENT)),
        "the first failure's own errno must survive the second: {msg:?}"
    );
}

// Record-time identity =====

/// The panic message of a caught `open_db`-style call that must have panicked.
fn panic_message<T>(result: std::thread::Result<T>) -> String {
    let payload = match result {
        Ok(_) => panic!("expected a panic"),
        Err(p) => p,
    };
    crate::coordination::panic_payload_message(payload.as_ref()).to_owned()
}

/// A different file renamed over the path after the `SQLITE_FCNTL_HAS_MOVED`
/// check passed and before the stat it is validated against. Recording the
/// stat alone would adopt the replacement as this connection's own identity;
/// the record must reject it instead, before any write.
#[test]
fn record_rejects_a_file_swapped_in_between_the_fcntl_check_and_the_stat() {
    let (dir, path) = temp_db();
    let other = dir.path().join("other.db");
    let swap_path = path.clone();
    let _seam = set_test_seam_hook(Seam::Fcntl, move || {
        std::fs::write(&other, b"a different file").unwrap();
        std::fs::rename(&other, &swap_path).unwrap();
    });

    let msg = panic_message(std::panic::catch_unwind(|| open_db(&path)));

    assert!(
        msg.contains(&format!("{path:?}")) && msg.contains("open-to-record window"),
        "the record must reject the swap before schema init writes anything: {msg:?}"
    );
}

/// A different file renamed over the path right after the open: SQLite's own
/// record already disagrees with the path, which `SQLITE_FCNTL_HAS_MOVED` reports.
#[test]
fn record_rejects_a_file_swapped_in_right_after_the_open() {
    let (dir, path) = temp_db();
    let other = dir.path().join("other.db");
    let swap_path = path.clone();
    let _seam = set_test_seam_hook(Seam::Open, move || {
        std::fs::write(&other, b"a different file").unwrap();
        std::fs::rename(&other, &swap_path).unwrap();
    });

    let msg = panic_message(std::panic::catch_unwind(|| open_db(&path)));

    assert!(
        msg.contains(&format!("{path:?}")) && msg.contains("already reported moved"),
        "SQLITE_FCNTL_HAS_MOVED must reject the swap at record time: {msg:?}"
    );
}

/// A symlink ancestor retargeted right after the open: the connection's file
/// and what `path` now resolves to disagree, so nothing may be recorded.
#[test]
fn record_rejects_an_ancestor_retargeted_right_after_the_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = open_through_symlink(dir.path());
    let second = dir.path().join("second");
    std::fs::create_dir(&second).unwrap();
    std::fs::write(second.join(".skuld.db"), b"a different file").unwrap();
    let root = dir.path().to_owned();
    let _seam = set_test_seam_hook(Seam::Open, move || retarget_link(&root, &second));

    let msg = panic_message(std::panic::catch_unwind(|| open_db(&path)));

    assert!(
        msg.contains(&format!("{path:?}")) && msg.contains("open-to-record window"),
        "the record must reject the retarget before schema init writes anything: {msg:?}"
    );
}

/// SQLite's own path is swapped after the fcntl check passed while the
/// caller's path, through a retargeted ancestor, still resolves to the original
/// file (hard-linked). Only the second stat of SQLite's path sees it.
#[test]
fn record_rejects_sqlites_own_path_swapped_while_the_callers_path_still_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let path = open_through_symlink(dir.path());
    let root = dir.path().to_owned();
    let _seam = set_test_seam_hook(Seam::Fcntl, move || {
        let first_db = root.join("first").join(".skuld.db");
        let second = root.join("second");
        std::fs::create_dir(&second).unwrap();
        std::fs::hard_link(&first_db, second.join(".skuld.db")).unwrap();
        let other = root.join("other.db");
        std::fs::write(&other, b"a different file").unwrap();
        std::fs::rename(&other, &first_db).unwrap();
        retarget_link(&root, &second);
    });

    let msg = panic_message(std::panic::catch_unwind(|| open_db(&path)));

    assert!(
        msg.contains(&format!("{path:?}")) && msg.contains("while its identity was being recorded"),
        "the second stat of SQLite's own path must reject this: {msg:?}"
    );
}

/// The `-wal`/`-shm` this connection has open are unlinked after schema init
/// and recreated by another connection before the companions are recorded.
/// Recording the path's files would adopt the other connection's; the record
/// must reject them, because a connection writing to an unlinked WAL while
/// another uses the new one is the split-brain this crate exists to prevent.
#[test]
fn record_rejects_companions_swapped_after_schema_init() {
    let (_dir, path) = temp_db();
    let swap_path = path.clone();
    let _seam = set_test_seam_hook(Seam::SchemaInit, move || {
        // Replace each companion with a copy on a new inode, the way another
        // opener recreating it would, while this connection keeps the old ones.
        for suffix in ["-wal", "-shm"] {
            let companion = companion_path(&swap_path, suffix);
            let copy = companion_path(&swap_path, &format!("{suffix}.copy"));
            std::fs::copy(&companion, &copy).unwrap();
            std::fs::rename(&copy, &companion).unwrap();
        }
    });

    let msg = panic_message(std::panic::catch_unwind(|| open_db(&path)));

    assert!(
        msg.contains(&format!("{path:?}")) && msg.contains("-wal/-shm"),
        "the record must reject swapped companions: {msg:?}"
    );
}

// Open-file identities =====

/// A file this process holds open is listed, and stops being once closed.
#[test]
fn open_file_identities_lists_exactly_the_files_this_process_holds_open() {
    let dir = tempfile::tempdir().unwrap();
    let file = std::fs::File::create(dir.path().join("f")).unwrap();
    let id = FileIdentity::of(&dir.path().join("f")).unwrap();

    assert!(open_file_identities().contains(&id), "an open file must be listed");
    drop(file);
    assert!(!open_file_identities().contains(&id), "a closed file must not be");
}

/// The fallback that tries every descriptor agrees with the directory listing
/// for a file this test holds open.
#[test]
fn scanning_descriptors_finds_the_same_open_file_as_the_directory_listing() {
    let dir = tempfile::tempdir().unwrap();
    let _file = std::fs::File::create(dir.path().join("f")).unwrap();
    let id = FileIdentity::of(&dir.path().join("f")).unwrap();

    assert!(scan_open_file_identities(4096).contains(&id));
}
