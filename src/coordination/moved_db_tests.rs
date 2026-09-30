//! Tests that each moved-DB check is load-bearing on its own: every scenario
//! here is caught by exactly one check, so removing that check (and nothing
//! else) turns the test red.

use super::coordination_tests::temp_db;
use super::moved_db::Failure;
#[cfg(unix)]
use super::moved_db::FileIdentity;
#[cfg(unix)]
use super::moved_db::{companion_path, fd_identity, has_moved_via_fcntl, DbIdentity};
#[cfg(unix)]
use super::test_hooks::{retry_rendezvous, set_test_retry_hook};
use super::test_hooks::{set_test_seam_hook, Seam};
use super::{coordinate, open_db, SERIAL_NONE};

fn symlink_dir(target: &std::path::Path, link: &std::path::Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(target, link)
        .expect("creating a directory symlink must succeed in this CI environment");
}

/// `dir/link` -> `dir/first`, with `dir/first/.skuld.db` opened through it.
/// Returns the symlink-routed path.
fn open_through_symlink(dir: &std::path::Path) -> std::path::PathBuf {
    let first = dir.join("first");
    std::fs::create_dir(&first).unwrap();
    symlink_dir(&first, &dir.join("link"));
    dir.join("link").join(".skuld.db")
}

/// Repoint `dir/link` at `target`: atomically on Unix (a fresh symlink renamed
/// over the old one), remove-and-recreate on Windows.
fn retarget_link(dir: &std::path::Path, target: &std::path::Path) {
    #[cfg(unix)]
    {
        let tmp = dir.join("link.tmp");
        symlink_dir(target, &tmp);
        std::fs::rename(&tmp, dir.join("link")).unwrap();
    }
    #[cfg(windows)]
    {
        std::fs::remove_dir(dir.join("link")).unwrap();
        symlink_dir(target, &dir.join("link"));
    }
}

/// Repoint `dir/link` at a new, empty directory: the DB no longer exists at
/// the path, on either platform.
fn retarget_to_empty_dir(dir: &std::path::Path) {
    let empty = dir.join("empty");
    std::fs::create_dir(&empty).unwrap();
    retarget_link(dir, &empty);
}

/// Only `SQLITE_FCNTL_HAS_MOVED` sees this: the caller's path still resolves
/// to the original inode (through a hard link in a retargeted directory), but
/// the path SQLite resolved at open time now names a different file.
#[cfg(unix)]
#[test]
fn db_has_moved_is_caught_by_the_sqlite_fcntl_alone() {
    let dir = crate::TempDir::new().unwrap();
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
#[cfg(unix)]
#[test]
fn db_has_moved_is_caught_by_the_file_identity_alone() {
    let dir = crate::TempDir::new().unwrap();
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
#[cfg(unix)]
#[test]
fn open_db_schema_write_is_stopped_by_a_split_lock_alone() {
    let outer = crate::TempDir::new().unwrap();
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
    retry.expect_no_more_retries();

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
    let dir = crate::TempDir::new().unwrap();
    let path = open_through_symlink(dir.path());
    let path2 = path.clone();
    let root = dir.path().to_owned();
    let result = std::thread::spawn(move || {
        let _hook = set_test_seam_hook(Seam::Commit, move || retarget_to_empty_dir(&root));
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
    let dir = crate::TempDir::new().unwrap();
    let path = open_through_symlink(dir.path());
    let a = coordinate(&path, "a", &[], SERIAL_NONE);
    let root = dir.path().to_owned();
    let result = std::thread::spawn(move || {
        let _hook = set_test_seam_hook(Seam::Delete, move || retarget_to_empty_dir(&root));
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
    let dir = crate::TempDir::new().unwrap();
    let path = open_through_symlink(dir.path());
    let (conn, identity) = open_db(&path);
    retarget_to_empty_dir(dir.path());
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
#[cfg(unix)]
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
    let dir = crate::TempDir::new().unwrap();
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

// Companions =====

/// Both platforms record `-wal` and `-shm`, and a fresh open reports nothing moved.
#[test]
fn open_db_records_both_companions_and_reports_them_unmoved() {
    let (_dir, path) = temp_db();
    let (conn, identity) = open_db(&path);

    assert!(identity.companions.is_some(), "open_db must record the companions");
    assert!(!identity.has_moved(&conn, &path));
}

/// A retargeted ancestor is reported through the main identity. The tracked
/// companions are the connection's own files, found through SQLite's resolved
/// path (Unix) or the main handle's final path (Windows), so the retarget does
/// not redirect them and they are not what reports it.
#[test]
fn an_ancestor_retarget_is_caught_by_the_main_identity_not_the_companions() {
    let dir = crate::TempDir::new().unwrap();
    let path = open_through_symlink(dir.path());
    let (conn, identity) = open_db(&path);

    retarget_to_empty_dir(dir.path());

    assert!(identity.has_moved(&conn, &path), "the retarget must be reported");
    let companions = identity.companions.as_ref().unwrap();
    assert!(
        !companions.wal.has_moved() && !companions.shm.has_moved(),
        "the companions must still name the connection's own files"
    );
}

/// Windows cannot replace a companion out from under an open connection: the
/// share mode withholds `FILE_SHARE_DELETE`, which is why recording them needs
/// no cross-check there. If a SQLite upgrade changes that, this fails instead
/// of a swapped companion going unnoticed.
#[cfg(windows)]
#[test]
fn windows_open_db_companions_block_delete_and_rename_while_held() {
    let (_dir, path) = temp_db();
    let (_conn, identity) = open_db(&path);
    let companions = identity.companions.as_ref().unwrap();
    for tracked in [&companions.wal, &companions.shm] {
        let remove_err = std::fs::remove_file(&tracked.path).expect_err("deleting a held companion must fail");
        assert_eq!(remove_err.raw_os_error(), Some(32), "{remove_err}");
        let renamed = tracked.path.with_extension("renamed");
        let rename_err = std::fs::rename(&tracked.path, &renamed).expect_err("renaming a held companion must fail");
        assert_eq!(rename_err.raw_os_error(), Some(32), "{rename_err}");
    }
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

/// A different file renamed over the path after the connection's fd was read
/// and before the path stats it is validated against. Recording a path stat
/// would adopt the replacement as this connection's own identity; the record
/// must reject it instead, before any write.
#[cfg(unix)]
#[test]
fn record_rejects_a_file_swapped_in_between_the_fd_identity_and_the_path_stats() {
    let (dir, path) = temp_db();
    let other = dir.path().join("other.db");
    let swap_path = path.clone();
    let _seam = set_test_seam_hook(Seam::Fd, move || {
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
#[cfg(unix)]
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
        msg.contains(&format!("{path:?}")) && msg.contains("open-to-record window"),
        "SQLITE_FCNTL_HAS_MOVED must reject the swap at record time: {msg:?}"
    );
}

/// A symlink ancestor retargeted right after the open: the connection's file
/// and what `path` now resolves to disagree, so nothing may be recorded.
#[test]
fn record_rejects_an_ancestor_retargeted_right_after_the_open() {
    let dir = crate::TempDir::new().unwrap();
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

/// SQLite's own path is swapped after the connection's fd was read while the
/// caller's path, through a retargeted ancestor, still resolves to the original
/// file (hard-linked). Only the stat of SQLite's own path sees it.
#[cfg(unix)]
#[test]
fn record_rejects_sqlites_own_path_swapped_while_the_callers_path_still_resolves() {
    let dir = crate::TempDir::new().unwrap();
    let path = open_through_symlink(dir.path());
    let root = dir.path().to_owned();
    let _seam = set_test_seam_hook(Seam::Fd, move || {
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
        msg.contains(&format!("{path:?}")) && msg.contains("no longer names the file"),
        "the second stat of SQLite's own path must reject this: {msg:?}"
    );
}

/// Companions this connection has open are replaced by copies on new inodes
/// after schema init, the way another opener recreating them would, before they
/// are recorded. Recording the path's files would adopt the other opener's; the
/// record must reject them, because a connection writing to an unlinked file
/// while another uses the new one is the split-brain this crate exists to
/// prevent. Each companion is swapped alone, so each one's check is load-bearing.
#[cfg(unix)]
fn assert_swapped_companions_are_rejected(swapped: &'static [&'static str]) {
    let (_dir, path) = temp_db();
    let swap_path = path.clone();
    let _seam = set_test_seam_hook(Seam::SchemaInit, move || {
        for suffix in swapped {
            let companion = companion_path(&swap_path, suffix);
            let copy = companion_path(&swap_path, &format!("{suffix}.copy"));
            std::fs::copy(&companion, &copy).unwrap();
            std::fs::rename(&copy, &companion).unwrap();
        }
    });

    let msg = panic_message(std::panic::catch_unwind(|| open_db(&path)));

    assert!(
        msg.contains(&format!("{path:?}")) && msg.contains("-wal/-shm"),
        "the record must reject swapped {swapped:?}: {msg:?}"
    );
}

#[cfg(unix)]
#[test]
fn record_rejects_a_wal_swapped_after_schema_init() {
    assert_swapped_companions_are_rejected(&["-wal"]);
}

#[cfg(unix)]
#[test]
fn record_rejects_a_shm_swapped_after_schema_init() {
    assert_swapped_companions_are_rejected(&["-shm"]);
}

// The connection's own fds =====

/// The mirrored struct layout is the bundled SQLite's. An upgrade must re-check
/// `unix_fds.rs` against its `os_unix.c`, then bump this.
#[cfg(unix)]
#[test]
fn bundled_sqlite_is_the_version_whose_unix_layout_is_mirrored() {
    assert_eq!(
        rusqlite::version(),
        "3.53.2",
        "the bundled SQLite changed: re-verify the unixFile/unixShm/unixShmNode layout mirrored in \
         moved_db/unix_fds.rs, then update this version"
    );
}

/// Pins the layout at run time: each fd read from SQLite is the file SQLite
/// created at that path, and is open.
#[cfg(unix)]
#[test]
fn the_connections_fds_are_the_files_sqlite_created() {
    use super::moved_db::unix_fds;
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);
    let db = std::path::PathBuf::from(conn.path().unwrap());
    for (what, fd, file) in [
        ("main", unix_fds::main_fd(&conn), db.clone()),
        ("wal", unix_fds::wal_fd(&conn), companion_path(&db, "-wal")),
        ("shm", unix_fds::shm_fd(&conn), companion_path(&db, "-shm")),
    ] {
        assert_eq!(
            fd_identity(fd),
            FileIdentity::of(&file),
            "the {what} fd {fd} must be open and be the file at {file:?}"
        );
    }
}

// Two filesystems =====

/// The two directories `SKULD_TEST_FS_A` / `SKULD_TEST_FS_B` name, on different
/// filesystems. `SKULD_TEST_TWO_FS=0` opts out explicitly; otherwise missing
/// provisioning fails the test rather than skipping it.
///
/// Provision, in a container or on CI (CI does this in `ci.yaml`), e.g.
/// `docker run --rm --network none --tmpfs /fs-a --tmpfs /fs-b ...` with
/// `SKULD_TEST_FS_A=/fs-a SKULD_TEST_FS_B=/fs-b`.
#[cfg(target_os = "linux")]
fn two_filesystems() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    if std::env::var("SKULD_TEST_TWO_FS").as_deref() == Ok("0") {
        eprintln!("SKULD_TEST_TWO_FS=0: skipping the cross-device test by explicit opt-out");
        return None;
    }
    let dir = |var: &str| {
        std::path::PathBuf::from(std::env::var_os(var).unwrap_or_else(|| {
            panic!(
                "{var} is not set: this test needs two directories on different filesystems (see \
                 `two_filesystems`), or SKULD_TEST_TWO_FS=0 to opt out"
            )
        }))
    };
    Some((dir("SKULD_TEST_FS_A"), dir("SKULD_TEST_FS_B")))
}

/// A file on another device with the same inode number replaces the database
/// (a symlink to it, so SQLite's own inode-only `HAS_MOVED` comparison still
/// answers "not moved"). Only the connection's own fd, compared with device as
/// well as inode, tells the two apart: recording `stat(path)` would adopt the
/// impostor.
#[cfg(target_os = "linux")]
#[test]
fn record_rejects_a_same_inode_file_on_another_device() {
    use std::os::unix::fs::MetadataExt;
    let Some((fs_a, fs_b)) = two_filesystems() else { return };
    let dir_a = crate::TempDir::new_in(&fs_a).unwrap();
    let dir_b = crate::TempDir::new_in(&fs_b).unwrap();
    assert_ne!(
        dir_a.path().metadata().unwrap().dev(),
        dir_b.path().metadata().unwrap().dev(),
        "SKULD_TEST_FS_A and SKULD_TEST_FS_B must be different filesystems"
    );
    let path = dir_a.path().join(".skuld.db");
    let (swap_path, b) = (path.clone(), dir_b.path().to_owned());
    let _seam = set_test_seam_hook(Seam::Open, move || {
        let ino = std::fs::metadata(&swap_path).unwrap().ino();
        // Create files on B until one has the database's inode number.
        for i in 0.. {
            let filler = b.join(format!("filler-{i}"));
            std::fs::write(&filler, b"an impostor").unwrap();
            let got = std::fs::metadata(&filler).unwrap().ino();
            assert!(
                got <= ino,
                "cannot give a file on B inode {ino}: its next inode is already {got}"
            );
            if got == ino {
                std::fs::rename(&filler, b.join(".skuld.db")).unwrap();
                break;
            }
        }
        std::fs::remove_file(&swap_path).unwrap();
        std::os::unix::fs::symlink(b.join(".skuld.db"), &swap_path).unwrap();
    });

    let msg = panic_message(std::panic::catch_unwind(|| open_db(&path)));

    assert!(
        msg.contains("no longer names the file"),
        "the record must reject a same-inode file on another device: {msg:?}"
    );
}

// Failures during an unwind =====

/// `coordinate` panics after its COMMIT when the DB moved. The registration
/// must already exist then, so unwinding runs its `Drop` (which refuses to
/// write and records that) instead of abandoning the row unnoticed.
#[test]
fn a_post_commit_panic_unwinds_through_the_registrations_drop() {
    let dir = crate::TempDir::new().unwrap();
    let path = open_through_symlink(dir.path());
    let path2 = path.clone();
    let root = dir.path().to_owned();
    let result = std::thread::spawn(move || {
        let _hook = set_test_seam_hook(Seam::Commit, move || retarget_to_empty_dir(&root));
        coordinate(&path2, "a", &[], SERIAL_NONE)
    })
    .join();

    assert!(result.is_err());
    let recorded = crate::coordination::violations::recorded();
    assert!(
        recorded
            .iter()
            .any(|m| m.contains(&format!("{path:?}")) && m.contains("deleted or replaced mid-run")),
        "the registration's Drop must have run during the unwind: {recorded:?}"
    );
}

/// The split check must also run after the companions are recorded: a
/// directory replaced wholesale in that window (every file hard-linked so no
/// identity check trips) would otherwise become the baseline.
#[cfg(unix)]
#[test]
fn open_db_rejects_a_split_landing_after_schema_init() {
    let outer = crate::TempDir::new().unwrap();
    let profile = outer.path().join("profile");
    std::fs::create_dir(&profile).unwrap();
    let path = profile.join("test-coordination.db");
    let swap_profile = profile.clone();
    let root = outer.path().to_owned();
    let _seam = set_test_seam_hook(Seam::SchemaInit, move || {
        let aside = root.join("profile-aside");
        std::fs::rename(&swap_profile, &aside).unwrap();
        std::fs::create_dir(&swap_profile).unwrap();
        for suffix in ["", "-wal", "-shm"] {
            let name = format!("test-coordination.db{suffix}");
            std::fs::hard_link(aside.join(&name), swap_profile.join(&name)).unwrap();
        }
    });

    let msg = panic_message(std::panic::catch_unwind(|| open_db(&path)));

    assert!(msg.contains("was split"), "{msg:?}");
}
