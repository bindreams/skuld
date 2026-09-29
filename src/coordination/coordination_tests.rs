//! Tests for the SQLite coordination module.

use std::sync::atomic::{AtomicU32, Ordering::SeqCst};
use std::time::Duration;

use crate::coordination::test_hooks::{retry_rendezvous, set_test_retry_hook};
use crate::coordination::{can_start, coordinate, is_retryable, open_db, register, SERIAL_ALL, SERIAL_NONE};
use crate::label::Label;

use super::rendezvous::rendezvous;

/// Hard-link `path` and its `-wal`/`-shm` companions to `<path's
/// dir>/<name>.db`(`-wal`/`-shm`), returning the new main-file path — a
/// *witness* onto the exact committed state at the moment of the call,
/// reachable independent of anything that later happens to `path` itself
/// (an `unlink`, e.g.). All three are hard-linked, not just the main file:
/// under WAL mode a commit lands in `-wal`, not the main file, until
/// checkpointed, so a witness missing its own `-wal`/`-shm` companions
/// would see a stale, pre-commit view instead of the state this function's
/// caller actually observed.
#[cfg(unix)]
fn hard_link_db_and_companions(path: &std::path::Path, name: &str) -> std::path::PathBuf {
    let witness = path.with_file_name(format!("{name}.db"));
    std::fs::hard_link(path, &witness).unwrap();
    for suffix in ["-wal", "-shm"] {
        let from = companion_path(path, suffix);
        if from.exists() {
            std::fs::hard_link(&from, companion_path(&witness, suffix)).unwrap();
        }
    }
    witness
}

/// `path` with `suffix` appended verbatim to the filename — mirrors
/// `super::companion_path` (private to `coordination.rs`) for test-side use
/// naming `-wal`/`-shm` companions.
#[cfg(unix)]
fn companion_path(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    std::path::PathBuf::from(name)
}

/// Retries a `*_with_no_retry_cap` test lets its worker make against a lock
/// that stays held, before releasing it. Each is one signal from the worker's
/// own retry arm, so the count is exact, not inferred from elapsed time. Eight
/// exceeds any small fixed cap (the usual "retry a few times" shape) and runs
/// past the 10 ms to 200 ms backoff's doubling phase (10, 20, 40, 80, 160) onto
/// its 200 ms plateau. No finite count proves "uncapped"; this rules out every
/// cap below it.
const NO_CAP_RETRIES: usize = 8;

/// Create a temporary database for testing.
pub(super) fn temp_db() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test-coordination.db");
    (dir, path)
}

/// Run [`register`] in a `BEGIN IMMEDIATE`/`COMMIT` pair (it debug-asserts a
/// transaction); returns the row id.
fn register_in_txn(conn: &rusqlite::Connection, name: &str, labels: &[Label], serial_filter: &str) -> i64 {
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let id = register(conn, name, labels, serial_filter).unwrap();
    conn.execute_batch("COMMIT").unwrap();
    id
}

/// `register`'s transaction contract is a `debug_assert!`, so it is only
/// checked in debug builds.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "must be called inside an active transaction")]
fn register_outside_a_transaction_trips_its_debug_assert() {
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);

    let _ = register(&conn, "no_transaction", &[], SERIAL_NONE);
}

// can_start =====

#[test]
fn non_serial_can_start_when_empty() {
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);
    assert!(can_start(&conn, &[], SERIAL_NONE).unwrap());
}

#[test]
fn non_serial_blocked_by_global_serial() {
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);
    register_in_txn(&conn, "blocker", &[], SERIAL_ALL);
    assert!(!can_start(&conn, &[], SERIAL_NONE).unwrap());
}

#[test]
fn non_serial_blocked_by_matching_filter() {
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);
    let docker = Label::__new("docker");
    // A serial test filtering on "docker" is running
    register_in_txn(&conn, "serial_docker", &[], "docker");
    // A test WITH label docker is blocked
    assert!(!can_start(&conn, &[docker], SERIAL_NONE).unwrap());
    // A test WITHOUT label docker is NOT blocked
    assert!(can_start(&conn, &[], SERIAL_NONE).unwrap());
}

#[test]
fn global_serial_blocked_when_anything_running() {
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);
    register_in_txn(&conn, "some_test", &[], SERIAL_NONE);
    assert!(!can_start(&conn, &[], SERIAL_ALL).unwrap());
}

#[test]
fn global_serial_can_start_when_empty() {
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);
    assert!(can_start(&conn, &[], SERIAL_ALL).unwrap());
}

#[test]
fn filtered_serial_blocked_by_matching_running_test() {
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);
    let docker = Label::__new("docker");
    // A non-serial test with label "docker" is running
    register_in_txn(&conn, "docker_test", &[docker], SERIAL_NONE);
    // A serial test filtering on "docker" is blocked
    assert!(!can_start(&conn, &[], "docker").unwrap());
}

#[test]
fn filtered_serial_not_blocked_by_non_matching() {
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);
    let network = Label::__new("network");
    // A test with label "network" is running
    register_in_txn(&conn, "network_test", &[network], SERIAL_NONE);
    // A serial test filtering on "docker" is NOT blocked
    assert!(can_start(&conn, &[], "docker").unwrap());
}

#[test]
fn filtered_serial_and_semantics() {
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);
    let a = Label::__new("a");
    let b = Label::__new("b");

    // Test with only [a] is running
    register_in_txn(&conn, "test_a", &[a], SERIAL_NONE);
    // serial = "a & b" should NOT be blocked (running test doesn't have both a and b)
    assert!(can_start(&conn, &[], "a & b").unwrap());

    // Now add a test with [a, b]
    register_in_txn(&conn, "test_ab", &[a, b], SERIAL_NONE);
    // serial = "a & b" IS now blocked
    assert!(!can_start(&conn, &[], "a & b").unwrap());
}

#[test]
fn filtered_serial_not_semantics() {
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);
    let a = Label::__new("a");
    let b = Label::__new("b");

    // Test with label [a] is running
    register_in_txn(&conn, "test_a", &[a], SERIAL_NONE);
    // serial = "!a" should NOT be blocked (running test HAS label a)
    assert!(can_start(&conn, &[], "!a").unwrap());

    // Test with label [b] is running (no label a)
    register_in_txn(&conn, "test_b", &[b], SERIAL_NONE);
    // serial = "!a" IS now blocked (test_b doesn't have a, so !a matches)
    assert!(!can_start(&conn, &[], "!a").unwrap());
}

// register / cleanup =====

#[test]
fn register_and_delete() {
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);
    let docker = Label::__new("docker");
    let id = register_in_txn(&conn, "my_test", &[docker], SERIAL_NONE);

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM running", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);

    // Direct DELETE (same as TestRegistration::drop)
    conn.execute("DELETE FROM running WHERE id = ?1", [id]).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM running", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn delete_cascades_labels() {
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);
    let a = Label::__new("a");
    let b = Label::__new("b");
    let id = register_in_txn(&conn, "test", &[a, b], SERIAL_NONE);

    let label_count: i64 = conn.query_row("SELECT COUNT(*) FROM labels", [], |r| r.get(0)).unwrap();
    assert_eq!(label_count, 2);

    conn.execute("DELETE FROM running WHERE id = ?1", [id]).unwrap();
    let label_count: i64 = conn.query_row("SELECT COUNT(*) FROM labels", [], |r| r.get(0)).unwrap();
    assert_eq!(label_count, 0);
}

// TestRegistration guard =====

#[test]
fn registration_guard_cleans_up_on_drop() {
    let (_dir, path) = temp_db();
    {
        let _reg = coordinate(&path, "guarded_test", &[], SERIAL_NONE);
        let (conn, _identity) = open_db(&path);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM running", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
    // After drop, the entry should be gone
    let (conn, _identity) = open_db(&path);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM running", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn registration_guard_cleans_up_on_panic() {
    let (_dir, path) = temp_db();
    let _ = std::panic::catch_unwind(|| {
        let _reg = coordinate(&path, "panicking_test", &[], SERIAL_NONE);
        panic!("intentional panic");
    });
    let (conn, _identity) = open_db(&path);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM running", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

/// `.skuld.db` deleted mid-run — main file and its `-wal`/`-shm`
/// companions, all of them — recreates fresh, same as the very first
/// connection of the run, relying on POSIX's tolerance for unlinking a file
/// another open handle still points at (see `connect`'s doc). Recreation
/// also resets `running`'s `AUTOINCREMENT` sequence back to 1 (tracked in
/// `sqlite_sequence`, part of the schema that goes with the deleted file),
/// so a test (`b`) registered against the fresh incarnation can end up with
/// the exact same numeric `id` an earlier test (`a`), registered against
/// the deleted one, already had.
///
/// A real review probe once found this live on `main`: an old design that
/// reconnected fresh in `Drop` (instead of keeping the original connection)
/// has no way to tell the two incarnations apart by id alone, and silently
/// deletes whichever row currently has that id — `b`'s, not `a`'s, once the
/// file's been recreated. `a` now fails loudly instead (its connection's
/// file has moved — see `DbIdentity::panic_if_moved`), which incidentally also
/// closes the id-collision hole: a drop that panics before ever reaching
/// the `DELETE` can't delete the wrong row either.
///
/// Unix-only, and genuinely deletes the file (not just its content, via a
/// second connection) — both for the same reason: this specific bug's
/// precondition is two *physically distinct* incarnations of `.skuld.db`
/// (different inodes) with `a`'s connection bound to the old one, which
/// only a real unlink-and-recreate produces; forcing the id collision some
/// other way (e.g. `DELETE FROM running` through a second connection to the
/// *same* file) leaves both registrations on the very same incarnation,
/// where nothing — this fix included — can tell them apart, since there is
/// genuinely nothing to tell apart. Windows can't reach this precondition
/// at all: deleting a file any handle still has open fails outright there
/// (confirmed — this test's own first version, before it was made
/// Unix-only, failed exactly that way on both Windows CI lanes), unlike
/// POSIX's unlink-while-open tolerance `connect`'s own doc relies on.
#[cfg(unix)]
#[test]
fn registration_drop_fails_loudly_instead_of_deleting_a_different_registration_that_reused_its_id() {
    let (_dir, path) = temp_db();

    let a = coordinate(&path, "a", &[], SERIAL_NONE);

    // Delete the main file and its WAL companions.
    std::fs::remove_file(&path).unwrap();
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let _ = std::fs::remove_file(std::path::PathBuf::from(wal));
    let mut shm = path.as_os_str().to_owned();
    shm.push("-shm");
    let _ = std::fs::remove_file(std::path::PathBuf::from(shm));

    // b registers against the freshly recreated DB, global-serial so its
    // continued presence is directly checkable via can_start below.
    let b = coordinate(&path, "b", &[], SERIAL_ALL);
    assert_eq!(
        a.id, b.id,
        "test precondition: a and b must have collided on the same numeric id (AUTOINCREMENT \
         restarting after the recreated file) for this test to actually exercise anything"
    );

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(a)));
    assert!(
        result.is_err(),
        "TestRegistration::drop must fail loudly once its own connection's file has been \
         deleted/replaced mid-run, not silently succeed"
    );

    let (conn, _identity) = open_db(&path);
    assert!(
        !can_start(&conn, &[], SERIAL_NONE).unwrap(),
        "dropping a must not have deleted b's still-live global-serial row just because they \
         reused the same numeric id across two incarnations of the DB file"
    );

    drop(b);
}

/// Deleting *only* `.skuld.db` mid-run (not its `-wal`/`-shm` companions,
/// unlike the sibling test above) is a real corruption hazard, not just a
/// harmless split like the "everything deleted" case: `-wal`/`-shm` are
/// identified by *path*, shared with whatever `b` recreates at the same
/// path, while `a`'s connection is still bound to the (now-detached) old
/// main file — the two halves stop agreeing on the database's actual size
/// and content. The owner's decision: fail loudly instead of writing
/// through a connection in that state. `a`'s cleanup now checks
/// `SQLITE_FCNTL_HAS_MOVED` (see `DbIdentity::panic_if_moved`) before its `DELETE`
/// and refuses to run it once the file it opened is gone, panicking with a
/// clear message instead — and never risks touching `-wal`/`-shm` at all,
/// so `b`'s own DB stays intact.
///
/// **RED, before this fix landed:** `a.id`/`b.id` collide as in the sibling
/// test; `drop(a)` used to *succeed* silently (it doesn't touch `b`'s
/// `-wal`/`-shm`, so nothing stopped it); `drop(b)` then panicked with
/// `SQLITE_IOERR_SHORT_READ` (extended code 522) — the corruption
/// surfacing on the *wrong* registration's drop, with no indication of
/// what actually went wrong. **GREEN, now:** `drop(a)` is the one that
/// panics, with a clear "deleted or replaced mid-run" message naming the
/// path; `drop(b)` succeeds, and `b`'s row is cleanly removed — `b`'s DB
/// was never at risk in the first place.
#[cfg(unix)]
#[test]
fn registration_drop_fails_loudly_instead_of_corrupting_when_only_the_main_file_is_deleted_mid_run() {
    let (_dir, path) = temp_db();

    let a = coordinate(&path, "a", &[], SERIAL_NONE);

    // Unlike the sibling test above: only the main file, not -wal/-shm.
    std::fs::remove_file(&path).unwrap();

    let b = coordinate(&path, "b", &[], SERIAL_ALL);
    assert_eq!(
        a.id, b.id,
        "test precondition: a and b must have collided on the same numeric id for this probe \
         to mean anything"
    );

    let result_a = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(a)));
    assert!(
        result_a.is_err(),
        "dropping a must fail loudly (its connection's file was deleted/replaced mid-run), not \
         silently succeed and leave the corruption for something else to discover"
    );

    let result_b = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(b)));
    assert!(
        result_b.is_ok(),
        "dropping b must succeed: b's own connection was never touched by a's file having been \
         deleted out from under a, so nothing about a's mishap should affect b"
    );

    let (conn, _identity) = open_db(&path);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM running", [], |r| r.get(0))
        .expect("b's DB must still be queryable, not corrupted, after this whole sequence");
    assert_eq!(
        count, 0,
        "b's row must have been cleanly deleted by its own successful drop"
    );
}

/// Finding 2 (round-6 review): deleting only `-wal` — the main file's own
/// identity untouched — used to go completely undetected: the reviewer's
/// cross-process probe showed a fresh connection registering `SERIAL_ALL`
/// beside a live `SERIAL_ALL` row whose owning connection's `-wal` had been
/// deleted, and that row's own `drop` then succeeding silently. Losing the
/// WAL replaces the database's *live* state (anything not yet checkpointed
/// into the main file) out from under every connection still holding it
/// open, which is exactly the "deleted or replaced mid-run" hazard the
/// owner's fail-loudly decision covers — not a narrower case exempt from
/// it.
///
/// **RED, before this fix:** `drop(a)` below succeeded silently even
/// though its `-wal` companion was deleted. **GREEN, now:**
/// `DbIdentity::has_moved`'s independent `-wal`/`-shm` identity
/// check catches it.
#[cfg(unix)]
#[test]
fn registration_drop_fails_loudly_when_only_the_wal_companion_is_deleted_mid_run() {
    let (_dir, path) = temp_db();
    let a = coordinate(&path, "a", &[], SERIAL_NONE);

    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    std::fs::remove_file(std::path::PathBuf::from(&wal)).unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(a)));
    assert!(
        result.is_err(),
        "drop(a) must panic once it discovers its -wal companion is gone, even though the main \
         file's own identity is untouched"
    );
}

/// The `-shm` counterpart of the test above — same hazard, the other
/// companion.
#[cfg(unix)]
#[test]
fn registration_drop_fails_loudly_when_only_the_shm_companion_is_deleted_mid_run() {
    let (_dir, path) = temp_db();
    let a = coordinate(&path, "a", &[], SERIAL_NONE);

    let mut shm = path.as_os_str().to_owned();
    shm.push("-shm");
    std::fs::remove_file(std::path::PathBuf::from(&shm)).unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(a)));
    assert!(
        result.is_err(),
        "drop(a) must panic once it discovers its -shm companion is gone, even though the main \
         file's own identity is untouched"
    );
}

/// Finding 1 (round-5 review): the moved-DB check used to run once, before
/// `retry_busy`'s loop, not inside it — so a `.skuld.db` deleted while
/// `Drop`'s `DELETE` was mid-retry (busy-waiting on a foreign connection's
/// held lock) went undetected: the check had already passed before the
/// move happened, and nothing re-checked once contention cleared and the
/// retried `DELETE` finally ran.
///
/// **RED, before this fix:** `drop(a)` below succeeded silently even
/// though `.skuld.db` was deleted while it was blocked retrying past a
/// foreign `BEGIN EXCLUSIVE`. **GREEN, now:** the check runs inside the
/// retried closure, before every attempt, so it catches the move the very
/// next time `retry_busy` calls back into it.
///
/// Verified through a *witness*, not just "did something eventually
/// panic": a hard link to the pre-move inode (kept alive independent of
/// the later `unlink`, same technique for `-wal`/`-shm` so the witness sees
/// the same committed state a fresh reader of the original name would),
/// checked *after* the whole sequence for whether `a`'s row is still
/// there. A check placed only *after* an already-completed write — which
/// is what removing just the in-loop check, but keeping the one after
/// `retry_busy` returns, would leave — still eventually panics here too
/// (the after-check catches it on the very next line), so "it panicked"
/// alone can't tell the two apart; "the DELETE never actually ran against
/// the orphaned file" can.
#[cfg(unix)]
#[test]
fn registration_drop_fails_loudly_when_the_db_moves_mid_retry() {
    let (_dir, path) = temp_db();
    let a = coordinate(&path, "a", &[], SERIAL_NONE);

    let witness = hard_link_db_and_companions(&path, "witness");

    // Force `drop(a)`'s DELETE into `retry_busy`'s loop: hold an exclusive
    // transaction open on a second, foreign connection first.
    let (foreign_conn, _identity) = open_db(&path);
    foreign_conn.execute_batch("BEGIN EXCLUSIVE").unwrap();

    let (worker, retry) = retry_rendezvous();
    let dropper = std::thread::spawn(move || {
        let _hook = set_test_retry_hook(worker);
        drop(a);
    });
    // Wait until the dropper has genuinely entered retry_busy's loop at
    // least once — an actual signal from the retry site itself, not a
    // fixed sleep standing in for one.
    retry.wait_for_retry();

    // Move the DB out from under the dropper while it's still blocked
    // retrying, then release the foreign lock so the retry can proceed.
    std::fs::remove_file(&path).unwrap();
    foreign_conn.execute_batch("COMMIT").unwrap();
    drop(foreign_conn);
    retry.release();
    retry.expect_no_more_retries();

    let result = dropper.join();
    assert!(
        result.is_err(),
        "drop(a) must panic once its retried DELETE discovers the DB moved mid-retry, not \
         succeed silently because the check only ran once before the retry loop started"
    );

    let witness_conn = rusqlite::Connection::open(&witness).unwrap();
    let count: i64 = witness_conn
        .query_row("SELECT COUNT(*) FROM running", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 1,
        "the retried DELETE must never have executed against the orphaned original file — a's \
         row must still be there, proving the moved-DB check caught the move before attempting \
         the write, not merely panicked sometime after an unguarded write already ran"
    );
}

/// Finding 2 (round-5 review): `open_db`'s own schema-init write
/// (`ensure_schema_locked`'s `retry_busy(conn, || conn.execute_batch(INIT_SQL))`)
/// had no moved-DB check at all — a `.skuld.db` deleted while this write
/// was mid-retry went completely unguarded, silently writing `INIT_SQL`
/// through a connection whose file identity had already changed
/// underneath it.
///
/// **RED, before this fix:** `open_db` below returned successfully even
/// though `.skuld.db` was deleted while its schema-init write was
/// mid-retry. **GREEN, now:** the check inside `retry_busy`'s closure
/// catches it on the next attempt.
///
/// Verified through a witness (see the sibling `TestRegistration` version
/// of this test for why "it panicked" alone doesn't distinguish an
/// in-loop check from one placed only after `retry_busy` returns): a hard
/// link, taken before the schema exists, to the pre-move inode. If
/// `INIT_SQL` ever actually ran against the orphaned original file before
/// the panic, the witness would show the `running` table created; it must
/// not.
#[cfg(unix)]
#[test]
fn open_db_schema_init_fails_loudly_when_the_db_moves_mid_retry() {
    let (_dir, path) = temp_db();

    // A foreign connection creates the file and holds an exclusive write
    // lock via WAL mode before skuld's own schema even exists at `path`,
    // forcing `open_db`'s own INIT_SQL write into `retry_busy`'s loop.
    let foreign_conn = rusqlite::Connection::open(&path).unwrap();
    foreign_conn
        .execute_batch("PRAGMA journal_mode=WAL; BEGIN EXCLUSIVE")
        .unwrap();

    let witness = hard_link_db_and_companions(&path, "witness");

    let (worker, retry) = retry_rendezvous();
    let path2 = path.clone();
    let opener = std::thread::spawn(move || {
        let _hook = set_test_retry_hook(worker);
        open_db(&path2);
    });
    retry.wait_for_retry();

    std::fs::remove_file(&path).unwrap();
    foreign_conn.execute_batch("COMMIT").unwrap();
    drop(foreign_conn);
    retry.release();
    retry.expect_no_more_retries();

    let result = opener.join();
    assert!(
        result.is_err(),
        "open_db's schema-init write must panic once it discovers the DB moved mid-retry, not \
         return a connection that silently wrote INIT_SQL through a stale file identity"
    );

    let witness_conn = rusqlite::Connection::open(&witness).unwrap();
    let table_count: i64 = witness_conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='running'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        table_count, 0,
        "INIT_SQL must never have executed against the orphaned original file — the moved-DB \
         check must catch this before the write, not merely panic sometime after an unguarded \
         write already ran"
    );
}

/// Finding 4a's corollary (round-5 review): the init lock itself can split
/// the same way a DB file can "move" — wholesale replacement of the
/// profile directory (rename aside, `mkdir` fresh) while the lock is held
/// leaves the holder locking the old, now-detached directory while a fresh
/// opener locks the new one, defeating the mutual exclusion `open_db`'s own
/// schema-init write depends on. `InitLockHeld::target_has_split` catches
/// it the same way `FileIdentity` catches a moved DB file: an `fstat`
/// recorded at acquisition, compared against a fresh `stat` of the same
/// path on every check.
///
/// **RED, before this fix:** `open_db` below returned successfully even
/// though its own profile directory was replaced wholesale while its
/// schema-init write was mid-retry. **GREEN, now:** the check inside
/// `retry_busy`'s closure catches it.
#[cfg(unix)]
#[test]
fn open_db_fails_loudly_when_the_profile_directory_is_replaced_wholesale_mid_retry() {
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

    // Replace the profile directory wholesale — rename aside, `mkdir` a
    // fresh one at the same path — while `opener`'s init lock is held and
    // its schema-init write is stuck retrying.
    let aside = outer.path().join("profile-aside");
    std::fs::rename(&profile, &aside).unwrap();
    std::fs::create_dir(&profile).unwrap();

    foreign_conn.execute_batch("COMMIT").unwrap();
    drop(foreign_conn);
    retry.release();
    retry.expect_no_more_retries();

    let result = opener.join();
    assert!(
        result.is_err(),
        "open_db must panic once it discovers its own init lock's directory was replaced \
         wholesale mid-retry, not silently keep writing under a lock that no longer excludes \
         anyone"
    );
}

/// Finding 4 (round-6 review): the end-to-end test above can't distinguish
/// "`panic_on_split_lock`'s in-loop call is present" from "it's missing but
/// `DbIdentity::panic_if_moved`'s co-located in-loop call still catches the same
/// event" — a wholesale directory replacement necessarily also changes
/// what `path` resolves to, so the DB-file identity check independently
/// catches every reachable end-to-end scenario that splits the lock,
/// making a mutation of `panic_on_split_lock`'s call site alone
/// unobservable there by construction, not by a gap in that test. This
/// tests `target_has_split` itself directly instead, the same way
/// `db_has_moved_detects_a_fresh_file_renamed_over_the_path_while_a_connection_is_open`
/// tests `DbIdentity::has_moved` directly for the same reason.
#[cfg(unix)]
#[test]
fn init_lock_target_has_split_detects_a_wholesale_directory_replacement() {
    let outer = tempfile::tempdir().unwrap();
    let profile = outer.path().join("profile");
    std::fs::create_dir(&profile).unwrap();
    let path = profile.join("test-coordination.db");

    let mut split_detected = false;
    super::lock::with_init_lock(&path, |token| {
        let aside = outer.path().join("profile-aside");
        std::fs::rename(&profile, &aside).unwrap();
        std::fs::create_dir(&profile).unwrap();
        split_detected = token.target_has_split();
    });
    assert!(
        split_detected,
        "InitLockHeld::target_has_split must detect a wholesale directory replacement while the \
         lock is held"
    );
}

/// Finding 6 (round-6 review): the Windows counterpart — `target_has_split`
/// used to be always `false` there, reasoning that `FILE_SHARE_DELETE`
/// being withheld already ruled out the lock file itself being split out
/// from under a holder. That reasoning has the same gap `FileIdentity`'s
/// own Windows test documents: a symlink ancestor of the lock file's own
/// path being retargeted changes what a fresh opener's `lock_path` resolves
/// to without touching the held file at all.
#[cfg(windows)]
#[test]
fn init_lock_target_has_split_detects_a_retargeted_symlink_ancestor_windows() {
    let dir = tempfile::tempdir().unwrap();
    let real1 = dir.path().join("real1");
    let real2 = dir.path().join("real2");
    std::fs::create_dir(&real1).unwrap();
    std::fs::create_dir(&real2).unwrap();
    let link = dir.path().join("link");
    std::os::windows::fs::symlink_dir(&real1, &link)
        .expect("creating a directory symlink must succeed in this CI environment");
    let path = link.join(".skuld.db");

    let mut split_detected = false;
    super::lock::with_init_lock(&path, |token| {
        std::fs::remove_dir(&link).unwrap();
        std::os::windows::fs::symlink_dir(&real2, &link).unwrap();
        split_detected = token.target_has_split();
    });
    assert!(
        split_detected,
        "InitLockHeld::target_has_split must detect a retargeted symlink ancestor while the \
         lock is held"
    );
}

/// Finding 5 (round-6 review): `coordinate`'s own loop must catch a move
/// landing mid-retry too, the same way `open_db`'s and `TestRegistration`'s
/// do — checked (via `DbIdentity::panic_if_moved`) at the top of every
/// iteration, before the transaction closure runs, not just once before
/// the loop started.
#[cfg(unix)]
#[test]
fn coordinate_fails_loudly_when_the_db_moves_mid_retry() {
    let (_dir, path) = temp_db();

    let (foreign_conn, _identity) = open_db(&path);
    foreign_conn.execute_batch("BEGIN EXCLUSIVE").unwrap();

    let witness = hard_link_db_and_companions(&path, "witness");

    let (worker, retry) = retry_rendezvous();
    let path2 = path.clone();
    let waiter = std::thread::spawn(move || {
        let _hook = set_test_retry_hook(worker);
        coordinate(&path2, "waiter", &[], SERIAL_NONE)
    });
    retry.wait_for_retry();

    std::fs::remove_file(&path).unwrap();
    foreign_conn.execute_batch("COMMIT").unwrap();
    drop(foreign_conn);
    retry.release();
    retry.expect_no_more_retries();

    let result = waiter.join();
    assert!(
        result.is_err(),
        "coordinate must panic once its per-iteration check discovers the DB moved mid-retry, \
         not silently register through a stale connection"
    );

    let witness_conn = rusqlite::Connection::open(&witness).unwrap();
    let count: i64 = witness_conn
        .query_row("SELECT COUNT(*) FROM running", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 0,
        "the retried transaction must never have executed against the orphaned original file \
         — no row for 'waiter' must have been inserted there, proving the moved-DB check \
         caught the move before attempting the write"
    );
}

/// Finding 4a (round-5 review): `SQLITE_FCNTL_HAS_MOVED` alone doesn't
/// reliably catch a symlink *ancestor* of `path` being retargeted mid-run —
/// see `DbIdentity::has_moved`'s doc for why. `FileIdentity`'s own independent
/// dev+ino check closes this: it always re-resolves the caller's own
/// `path` fresh, following whatever a symlink in it currently points to
/// right now, rather than relying solely on what SQLite itself tracks
/// internally.
///
/// **RED, before this fix:** `drop(a)` below succeeded silently even
/// though `a`'s connection was opened through a symlink retargeted to a
/// completely different directory before the drop ran. **GREEN, now:** the
/// independent `FileIdentity` comparison catches it.
#[cfg(unix)]
#[test]
fn registration_drop_fails_loudly_when_a_parent_symlink_is_retargeted_mid_run() {
    let dir = tempfile::tempdir().unwrap();
    let real1 = dir.path().join("real1");
    let real2 = dir.path().join("real2");
    std::fs::create_dir(&real1).unwrap();
    std::fs::create_dir(&real2).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real1, &link).unwrap();
    let path = link.join(".skuld.db");

    let a = coordinate(&path, "a", &[], SERIAL_ALL);

    // Retarget atomically — a fresh symlink renamed over the old one, not
    // an edit of the existing one — the same shape a real swap to a
    // different profile directory mid-run would take.
    let tmp = dir.path().join("link.tmp");
    std::os::unix::fs::symlink(&real2, &tmp).unwrap();
    std::fs::rename(&tmp, &link).unwrap();

    // `b` registers through the now-retargeted symlink — a completely
    // different, independent database from `a`'s perspective — and must
    // not block waiting on `a`'s (unrelated) global-serial slot.
    let b = coordinate(&path, "b", &[], SERIAL_ALL);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(a)));
    assert!(
        result.is_err(),
        "drop(a) must panic: its connection's file is reachable only through the OLD symlink \
         target, which `path` no longer names now that the symlink was retargeted"
    );

    drop(b);
}

/// Finding 6 (round-6 review): the Windows counterpart of the test above.
/// `FileIdentity` used to be a ZST on Windows — reasoning held that
/// `winOpen`'s withheld `FILE_SHARE_DELETE` already ruled out anything
/// worth detecting there, since no other handle can delete or rename the
/// main file itself out from under a connection that holds it open. That
/// reasoning has a real gap: a symlink or junction *ancestor* of the path
/// being retargeted changes what the path resolves to without touching
/// the file itself at all, which `FILE_SHARE_DELETE` says nothing about —
/// confirmed for real by a throwaway CI probe before this fix landed
/// (`drop(a)` succeeded silently after exactly this retarget). `FileIdentity`
/// now has a real Windows implementation (`GetFileInformationByHandle`'s
/// volume serial + file index, via a fresh open on each check — not the
/// connection's own already-open handle, which wouldn't observe a later
/// retarget at all).
///
/// Doesn't register a `b` through the retargeted symlink the way the Unix
/// test does: doing so hit a reproducible, separate Windows-specific I/O
/// error (`SQLITE_IOERR_SHORT_READ`) unrelated to this fix — worth its own
/// investigation, but not what this test exists to verify.
#[cfg(windows)]
#[test]
fn registration_drop_fails_loudly_when_a_parent_symlink_is_retargeted_mid_run_windows() {
    let dir = tempfile::tempdir().unwrap();
    let real1 = dir.path().join("real1");
    let real2 = dir.path().join("real2");
    std::fs::create_dir(&real1).unwrap();
    std::fs::create_dir(&real2).unwrap();
    let link = dir.path().join("link");
    std::os::windows::fs::symlink_dir(&real1, &link)
        .expect("creating a directory symlink must succeed in this CI environment");
    let path = link.join(".skuld.db");

    let a = coordinate(&path, "a", &[], SERIAL_ALL);

    // Retarget: remove the symlink reparse point itself (not real1's
    // contents), then point a fresh one at real2.
    std::fs::remove_dir(&link).unwrap();
    std::os::windows::fs::symlink_dir(&real2, &link).unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(a)));
    assert!(
        result.is_err(),
        "drop(a) must panic: its connection's file is reachable only through the OLD symlink \
         target, which `path` no longer names now that the symlink was retargeted"
    );
}

/// Finding 4 (round-6 review): a direct unit test of `DbIdentity::has_moved` itself,
/// against a fresh file renamed atomically over `path` while `conn` stays
/// open — not going through `coordinate`/`Drop`, so a mutation that only
/// breaks one of `DbIdentity::has_moved`'s two independent checks can't hide behind
/// the other one catching the same higher-level scenario for an unrelated
/// reason.
#[cfg(unix)]
#[test]
fn db_has_moved_detects_a_fresh_file_renamed_over_the_path_while_a_connection_is_open() {
    let (_dir, path) = temp_db();
    let (conn, identity) = open_db(&path);

    // A completely different file, renamed atomically over `path` — an
    // ordinary `rename(2)`, not skuld's own delete/republish machinery.
    let other_path = path.with_file_name("other.db");
    std::fs::write(&other_path, b"not a real sqlite db").unwrap();
    std::fs::rename(&other_path, &path).unwrap();

    assert!(
        identity.has_moved(&conn, &path),
        "db_has_moved must detect a fresh file renamed over path while conn is still open"
    );
}

/// `DbIdentity::has_moved` skips the `SQLITE_FCNTL_HAS_MOVED` file-control on
/// Windows entirely (SQLite's `winFileControl` has no case for it — it
/// always answers `SQLITE_NOTFOUND`, confirmed against the `bundled`
/// `sqlite3.c` this crate compiles) and instead relies on an invariant:
/// `winOpen` opens the main database file without `FILE_SHARE_DELETE`, so
/// while any connection holds it open, nothing else on the system can
/// delete or rename it. This test exercises that invariant directly,
/// independent of `DbIdentity::has_moved`'s own logic — if a future SQLite/rusqlite
/// upgrade ever changes the share mode, this test starts failing instead of
/// a moved database silently going unnoticed.
#[cfg(windows)]
#[test]
fn windows_open_db_file_blocks_delete_and_rename_while_held() {
    let (_dir, path) = temp_db();
    let a = coordinate(&path, "a", &[], SERIAL_NONE);

    // Specifically `ERROR_SHARING_VIOLATION` (32) — not just any error — so
    // this test can't quietly start passing for an unrelated reason (a
    // missing/inaccessible path, say) while the actual invariant under
    // test silently stops holding.
    let remove_err = std::fs::remove_file(&path).expect_err(
        "a registration's connection is still open on this file; Windows must refuse to delete \
         it out from under that connection",
    );
    assert_eq!(
        remove_err.raw_os_error(),
        Some(32),
        "expected ERROR_SHARING_VIOLATION specifically, not just any failure: {remove_err}"
    );

    let renamed = path.with_file_name("renamed.skuld.db");
    let rename_err = std::fs::rename(&path, &renamed).expect_err(
        "a registration's connection is still open on this file; Windows must refuse to rename \
         it out from under that connection",
    );
    assert_eq!(
        rename_err.raw_os_error(),
        Some(32),
        "expected ERROR_SHARING_VIOLATION specifically, not just any failure: {rename_err}"
    );

    drop(a);
}

// Concurrent coordination =====

#[test]
fn global_serial_prevents_concurrent_execution() {
    const THREADS: usize = 8;
    let (_dir, path) = temp_db();

    let running = AtomicU32::new(0);
    let points = rendezvous(THREADS);

    std::thread::scope(|s| {
        for point in points {
            s.spawn(|| {
                point.wait();
                let _reg = coordinate(&path, "serial_test", &[], SERIAL_ALL);
                running.fetch_add(1, SeqCst);
                std::thread::sleep(Duration::from_millis(10));
                assert_eq!(running.load(SeqCst), 1, "global serial allowed concurrent execution");
                running.fetch_sub(1, SeqCst);
            });
        }
    });
}

#[test]
fn non_serial_allows_concurrent_execution() {
    const THREADS: usize = 8;
    const { assert!(THREADS >= 2) };
    let (_dir, path) = temp_db();

    // Two rendezvous points: the first races every thread into coordinate()
    // together; the second holds every thread past fetch_add before any exits,
    // so peak == THREADS regardless of coordinate() latency. A live-but-stuck
    // participant (a deadlock inside coordinate()) still hangs the rest; see
    // the non-blocking counterpart below.
    let peak = AtomicU32::new(0);
    let running = AtomicU32::new(0);
    let entry_points = rendezvous(THREADS);
    let observation_points = rendezvous(THREADS);

    std::thread::scope(|s| {
        for (entry, observation) in entry_points.into_iter().zip(observation_points) {
            s.spawn(|| {
                entry.wait();
                let _reg = coordinate(&path, "parallel_test", &[], SERIAL_NONE);
                let n = running.fetch_add(1, SeqCst) + 1;
                peak.fetch_max(n, SeqCst);
                observation.wait();
                running.fetch_sub(1, SeqCst);
            });
        }
    });

    debug_assert!(peak.load(SeqCst) <= THREADS as u32);
    assert_eq!(
        peak.load(SeqCst) as usize,
        THREADS,
        "non-serial tests should run concurrently",
    );
}

/// Non-blocking counterpart to `non_serial_allows_concurrent_execution`:
/// checks `can_start` directly against already-registered rows, so a
/// regression fails instead of hanging. The rows carry labels, and one serial
/// row holds a filter the candidate does not match: neither may block a
/// non-serial test, while the same serial row still blocks a matching one.
#[test]
fn non_serial_registrations_do_not_block_can_start_for_another_non_serial_test() {
    const REGISTRATIONS: usize = 7;
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);
    let docker = Label::__new("docker");
    let network = Label::__new("network");
    let cache = Label::__new("cache");

    for i in 0..REGISTRATIONS {
        register_in_txn(&conn, &format!("parallel_test_{i}"), &[docker, network], SERIAL_NONE);
    }
    register_in_txn(&conn, "serial_cache", &[], "cache");

    assert!(
        can_start(&conn, &[docker, network], SERIAL_NONE).unwrap(),
        "{REGISTRATIONS} labelled non-serial registrations and a serial row with a non-matching \
         filter must not block another non-serial test from starting"
    );
    assert!(
        !can_start(&conn, &[cache], SERIAL_NONE).unwrap(),
        "the serial row must still block a test its filter matches"
    );
}

#[test]
fn filtered_serial_blocks_only_matching_tests() {
    let (_dir, path) = temp_db();
    let docker = Label::__new("docker");
    let network = Label::__new("network");

    // Start a serial=docker test
    let _serial_reg = coordinate(&path, "serial_docker", &[], "docker");

    // A test with label "network" (not matching filter) can start
    let _net_reg = coordinate(&path, "net_test", &[network], SERIAL_NONE);

    // A test with label "docker" (matching filter) should be blocked.
    // Test this by checking can_start, since coordinate would block.
    let (conn, _identity) = open_db(&path);
    assert!(!can_start(&conn, &[docker], SERIAL_NONE).unwrap());
}

/// Genuine `SQLITE_BUSY` on the very first schema creation: something outside
/// Skuld's locking holds a `BEGIN EXCLUSIVE` on the empty file while `open_db`
/// creates the tables. It reproduces quickly only because `connect_locked`
/// disables rusqlite's 5 s `busy_timeout`.
///
/// Deterministic: the worker's retry hook is a rendezvous (see `test_hooks`),
/// and the test lets it retry [`NO_CAP_RETRIES`] times against the held lock
/// before releasing it, so a cap below that fails. No sleep, no clock.
#[test]
fn open_db_creates_schema_past_a_foreign_held_exclusive_lock_with_no_retry_cap() {
    let (_dir, path) = temp_db();

    // A raw connection that never goes through Skuld's init lock at all —
    // standing in for "something outside Skuld" (see `open_db`'s doc) —
    // creates the file and holds a real `BEGIN EXCLUSIVE` on it before the
    // schema exists.
    let foreign_conn = rusqlite::Connection::open(&path).unwrap();
    foreign_conn.execute_batch("BEGIN EXCLUSIVE").unwrap();

    let (worker, retry) = retry_rendezvous();
    let waiter_path = path.clone();
    let waiter = std::thread::spawn(move || {
        let _hook = set_test_retry_hook(worker);
        let (conn, _identity) = open_db(&waiter_path);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM running", [], |r| r.get(0))
            .expect("schema must exist and be queryable once open_db returns");
        count
    });

    retry.pass_retries(NO_CAP_RETRIES);
    retry.wait_for_retry();

    foreign_conn.execute_batch("COMMIT").unwrap();
    drop(foreign_conn);
    retry.release();
    retry.expect_no_more_retries();

    let count = waiter
        .join()
        .expect("open_db must not panic under uncapped busy retry, however many retries it takes");
    assert_eq!(count, 0);
}

/// The same uncapped retry for [`TestRegistration`]'s cleanup, whose `DELETE`
/// can contend with a live `coordinate` caller's `BEGIN EXCLUSIVE`. Same
/// rendezvous, with [`NO_CAP_RETRIES`] retries before the lock is released.
#[test]
fn registration_drop_deletes_past_a_concurrent_held_exclusive_lock_with_no_retry_cap() {
    let (_dir, path) = temp_db();
    let reg = coordinate(&path, "guarded_test_busy", &[], SERIAL_NONE);

    // A concurrent, already-initialized connection — the ordinary shape of
    // a live `coordinate` caller elsewhere in the system, which never takes
    // the coordination DB's init lock once its own `open_db` call returns.
    let (foreign_conn, _identity) = open_db(&path);
    foreign_conn.execute_batch("BEGIN EXCLUSIVE").unwrap();

    let (worker, retry) = retry_rendezvous();
    let dropper = std::thread::spawn(move || {
        let _hook = set_test_retry_hook(worker);
        drop(reg);
    });

    retry.pass_retries(NO_CAP_RETRIES);
    retry.wait_for_retry();

    foreign_conn.execute_batch("COMMIT").unwrap();
    drop(foreign_conn);
    retry.release();
    retry.expect_no_more_retries();

    dropper
        .join()
        .expect("TestRegistration::drop must not panic under uncapped busy retry");

    let (conn, _identity) = open_db(&path);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM running", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 0,
        "the row must actually be deleted, not left behind after a warned-and-swallowed failure"
    );
}

/// `connect_locked` must disable rusqlite's own default `busy_timeout` on
/// every connection it returns, not just avoid setting one of our own:
/// `InnerConnection::open_with_flags` (rusqlite's own connection-open path,
/// which every real `open_db`/`TestRegistration`-cleanup connection goes
/// through) calls `sqlite3_busy_timeout(db, 5000)` unconditionally, so
/// simply never calling `Connection::busy_timeout` ourselves is not enough —
/// the three tests above (and `coordinate`'s own retry test below) only
/// reproduce genuine, fast `SQLITE_BUSY` contention because this pragma
/// reads back `0`, not `5000`.
#[test]
fn open_db_disables_rusqlites_default_busy_timeout() {
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);
    let busy_timeout_ms: i64 = conn.query_row("PRAGMA busy_timeout", [], |row| row.get(0)).unwrap();
    assert_eq!(
        busy_timeout_ms, 0,
        "open_db's connection must not carry rusqlite's own default 5000 ms busy_timeout"
    );
}

/// `coordinate`'s own retry arm (not `retry_busy`) retries a busy
/// `BEGIN EXCLUSIVE` uncapped, on the error code alone. Same rendezvous, with
/// [`NO_CAP_RETRIES`] retries before the lock is released.
#[test]
fn coordinate_retries_a_busy_begin_exclusive_with_no_retry_cap() {
    let (_dir, path) = temp_db();
    // Create the schema up front so the contention below lands on
    // `coordinate`'s own `BEGIN EXCLUSIVE`, not on schema creation inside
    // `open_db`.
    drop(open_db(&path));

    let (foreign_conn, _identity) = open_db(&path);
    foreign_conn.execute_batch("BEGIN EXCLUSIVE").unwrap();

    let (worker, retry) = retry_rendezvous();
    let waiter_path = path.clone();
    let waiter = std::thread::spawn(move || {
        let _hook = set_test_retry_hook(worker);
        coordinate(&waiter_path, "busy_retry_test", &[], SERIAL_NONE)
    });

    retry.pass_retries(NO_CAP_RETRIES);
    retry.wait_for_retry();

    foreign_conn.execute_batch("COMMIT").unwrap();
    drop(foreign_conn);
    retry.release();
    retry.expect_no_more_retries();

    let _reg = waiter
        .join()
        .expect("coordinate must not panic under uncapped busy retry, however many retries it takes");
}

// is_retryable =====

#[test]
fn is_retryable_matches_busy_only() {
    use rusqlite::{ffi, Error};

    let busy = Error::SqliteFailure(ffi::Error::new(ffi::SQLITE_BUSY), Some("database is locked".into()));
    // SQLITE_LOCKED is deliberately excluded: with no connection in this
    // crate ever using shared-cache mode, it can only mean a same-connection
    // self-conflict, which is permanent and not something retrying resolves
    // — see `is_retryable`'s own doc.
    let locked = Error::SqliteFailure(ffi::Error::new(ffi::SQLITE_LOCKED), None);
    let constraint = Error::SqliteFailure(ffi::Error::new(ffi::SQLITE_CONSTRAINT), None);

    assert!(is_retryable(&busy));
    assert!(!is_retryable(&locked));
    assert!(!is_retryable(&constraint));
    assert!(!is_retryable(&Error::QueryReturnedNoRows));
}

// is_pid_alive PID range guard =====
//
// See `is_pid_alive`'s own doc comment in `coordination.rs` for why a PID
// of `0` or `>= 2^31` must be rejected before the `kill(pid as i32, 0)` cast.

#[cfg(unix)]
#[test]
fn is_pid_alive_rejects_pid_zero_instead_of_checking_its_own_process_group() {
    // A naive `kill(0, 0)` always succeeds (checks the caller's own process
    // group), which would make a bogus PID-0 entry look permanently alive.
    assert!(!crate::coordination::is_pid_alive(0));
}

#[cfg(unix)]
#[test]
fn is_pid_alive_rejects_pids_that_would_wrap_negative_as_an_i32() {
    assert!(!crate::coordination::is_pid_alive(1u32 << 31));
    assert!(!crate::coordination::is_pid_alive(u32::MAX));
}

// Canonicalization at the storage boundary =====
//
// Verify that `coordinate()` collapses redundant and tautological serial
// filters before INSERTing them, so the DB invariant holds: every stored
// serial_filter is either `""`, `"*"`, or a canonical Display string.

fn stored_serial_filter(conn: &rusqlite::Connection, name: &str) -> String {
    conn.query_row("SELECT serial_filter FROM running WHERE name = ?1", [name], |row| {
        row.get::<_, String>(0)
    })
    .expect("test row should exist")
}

#[test]
fn coordinate_stores_canonical_form_for_redundant_filter() {
    let (_dir, path) = temp_db();
    let _reg = coordinate(&path, "redundant", &[], "(a) | (a)");
    let (conn, _identity) = open_db(&path);
    assert_eq!(stored_serial_filter(&conn, "redundant"), "a");
}

#[test]
fn coordinate_collapses_tautology_to_global_serial_sentinel() {
    let (_dir, path) = temp_db();
    let _reg = coordinate(&path, "taut", &[], "a | !a");
    let (conn, _identity) = open_db(&path);
    assert_eq!(stored_serial_filter(&conn, "taut"), SERIAL_ALL);
}

#[test]
fn coordinate_collapses_contradiction_to_non_serial_sentinel() {
    let (_dir, path) = temp_db();
    let _reg = coordinate(&path, "contra", &[], "a & !a");
    let (conn, _identity) = open_db(&path);
    assert_eq!(stored_serial_filter(&conn, "contra"), SERIAL_NONE);
}

#[test]
fn coordinate_preserves_serial_none_sentinel() {
    let (_dir, path) = temp_db();
    let _reg = coordinate(&path, "none", &[], SERIAL_NONE);
    let (conn, _identity) = open_db(&path);
    assert_eq!(stored_serial_filter(&conn, "none"), SERIAL_NONE);
}

#[test]
fn coordinate_preserves_serial_all_sentinel() {
    // Use a fresh DB so SERIAL_ALL isn't blocked by the SERIAL_NONE row above.
    let (_dir, path) = temp_db();
    let _reg = coordinate(&path, "all", &[], SERIAL_ALL);
    let (conn, _identity) = open_db(&path);
    assert_eq!(stored_serial_filter(&conn, "all"), SERIAL_ALL);
}

// Schema migration v0 → v1 =====

#[test]
fn migration_rewrites_legacy_non_canonical_rows() {
    let (_dir, path) = temp_db();
    // Open and seed the DB with the OLD schema (user_version still 0) plus
    // a row containing a legacy non-canonical serial_filter that happens to
    // simplify to the canonical "a".
    let (conn, _identity) = open_db(&path);
    // Reset version so the migration runs again on the next open.
    conn.execute("PRAGMA user_version = 0", []).unwrap();
    register_in_txn(&conn, "legacy", &[], "(a) | (a)");
    drop(conn);

    // Re-open. open_db should run migrate_schema and rewrite the legacy row
    // in place. After the migration, user_version is 1 and the row's filter
    // is the canonical Display form.
    let (conn, _identity) = open_db(&path);
    let stored: String = stored_serial_filter(&conn, "legacy");
    assert_eq!(stored, "a", "legacy row should be canonicalized");
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
    assert_eq!(version, 1, "schema version should bump to 1");
}

#[test]
fn migration_skips_already_canonical_rows() {
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);
    conn.execute("PRAGMA user_version = 0", []).unwrap();
    register_in_txn(&conn, "already_canonical", &[], "a");
    drop(conn);

    let (conn, _identity) = open_db(&path);
    assert_eq!(stored_serial_filter(&conn, "already_canonical"), "a");
}

/// `migrate_schema`'s `BEGIN IMMEDIATE` contends with a live `BEGIN EXCLUSIVE`
/// (its `PRAGMA user_version` read does not: a WAL read never conflicts with a
/// write lock). It must retry uncapped rather than skip the migration on the
/// first `SQLITE_BUSY`. Same rendezvous, with [`NO_CAP_RETRIES`] retries.
#[test]
fn migrate_schema_completes_past_a_foreign_held_exclusive_lock_with_no_retry_cap() {
    let (_dir, path) = temp_db();
    // Seed a pre-migration DB: schema exists (user_version back at 0), one
    // legacy non-canonical row for the migration to actually rewrite.
    let (conn, _identity) = open_db(&path);
    conn.execute("PRAGMA user_version = 0", []).unwrap();
    register_in_txn(&conn, "legacy", &[], "(a) | (a)");
    drop(conn);

    let foreign_conn = rusqlite::Connection::open(&path).unwrap();
    foreign_conn.execute_batch("BEGIN EXCLUSIVE").unwrap();

    let (worker, retry) = retry_rendezvous();
    let waiter_path = path.clone();
    let waiter = std::thread::spawn(move || {
        let _hook = set_test_retry_hook(worker);
        open_db(&waiter_path);
    });

    retry.pass_retries(NO_CAP_RETRIES);
    retry.wait_for_retry();

    foreign_conn.execute_batch("COMMIT").unwrap();
    drop(foreign_conn);
    retry.release();
    retry.expect_no_more_retries();

    waiter
        .join()
        .expect("open_db (and migrate_schema within it) must not panic under uncapped busy retry");

    let (conn, _identity) = open_db(&path);
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
    assert_eq!(
        version, 1,
        "migration must have completed, not silently skipped, despite contention"
    );
    assert_eq!(
        stored_serial_filter(&conn, "legacy"),
        "a",
        "migration must have actually run and canonicalized the legacy row"
    );
}

#[test]
fn migration_leaves_unparseable_live_rows_alone() {
    let (_dir, path) = temp_db();
    let (conn, _identity) = open_db(&path);
    conn.execute("PRAGMA user_version = 0", []).unwrap();
    // Insert a row with garbage that won't parse, owned by THIS process
    // (i.e. an alive instance) — the migration must not delete it.
    let our_pid = std::process::id();
    conn.execute(
        "INSERT INTO running (instance_id, name, serial_filter) VALUES (?1, ?2, ?3)",
        rusqlite::params![format!("{our_pid}:0"), "garbage", "this is not a filter!!"],
    )
    .unwrap();
    drop(conn);

    let (conn, _identity) = open_db(&path);
    let kept: String = conn
        .query_row("SELECT serial_filter FROM running WHERE name = 'garbage'", [], |row| {
            row.get(0)
        })
        .expect("live unparseable row should be preserved");
    assert_eq!(kept, "this is not a filter!!");
}

// Drop-panic-during-unwind downgrade path =====

/// Regression guard for the `&payload` vs `payload.as_ref()` footgun
/// documented on `downgraded_warning_message` in `coordination.rs`. These
/// cases exercise `panic_payload_message`'s extraction logic in isolation —
/// the `.as_ref()` deref already happened before the payload reaches it.
/// `downgraded_warning_message_carries_the_real_panic_message_through_the_box`
/// below, and `drop_panic_during_unwind_cli.rs`'s subprocess test, guard the
/// `&payload` vs `.as_ref()` choice itself.
#[test]
fn panic_payload_message_extracts_a_runtime_formatted_string_payload() {
    let payload = std::panic::catch_unwind(|| {
        let uid = 502;
        panic!("dynamic message uid {uid}");
    })
    .unwrap_err();
    assert_eq!(
        crate::coordination::panic_payload_message(payload.as_ref()),
        "dynamic message uid 502"
    );
}

#[test]
fn panic_payload_message_extracts_a_static_str_payload() {
    let payload = std::panic::catch_unwind(|| {
        panic!("static message");
    })
    .unwrap_err();
    assert_eq!(
        crate::coordination::panic_payload_message(payload.as_ref()),
        "static message"
    );
}

#[test]
fn panic_payload_message_falls_back_on_a_non_string_payload() {
    let payload = std::panic::catch_unwind(|| {
        std::panic::panic_any(42_i32);
    })
    .unwrap_err();
    assert_eq!(
        crate::coordination::panic_payload_message(payload.as_ref()),
        "<non-string panic payload>"
    );
}

/// Exercises the exact call convention `Drop::drop` uses —
/// `downgraded_warning_message(&payload)` — which is what actually
/// regression-guards the footgun documented on `downgraded_warning_message`
/// in `coordination.rs`: passing the un-deref'd `&payload` here would wrongly
/// fall back to `"<non-string panic payload>"` for a real, runtime-formatted
/// string payload.
#[test]
fn downgraded_warning_message_carries_the_real_panic_message_through_the_box() {
    let payload = std::panic::catch_unwind(|| {
        let uid = 502;
        panic!("dynamic message uid {uid}");
    })
    .unwrap_err();

    let msg = crate::coordination::downgraded_warning_message(&payload);

    assert!(
        msg.contains("dynamic message uid 502"),
        "must surface the real panic message through the Box, not the \
         non-string-payload fallback: {msg:?}"
    );
    assert!(
        !msg.contains("<non-string panic payload>"),
        "regression: `&payload` was passed instead of `payload.as_ref()`, so the \
         downcast silently missed and fell back to the placeholder: {msg:?}"
    );
}

/// The downgrade in `TestRegistration::drop` is conditioned on
/// `std::thread::panicking()`: it must fire *only* while the thread is
/// already unwinding from another panic. A normal drop (nothing else
/// unwinding) with a DB that's gone unusable between registration and drop
/// must still panic loudly — that's the whole point of `retry_busy`'s
/// callers using `unwrap_or_else(|e| panic!(...))` on a non-retryable
/// error, and downgrading unconditionally would silently swallow every one
/// of them.
///
/// Corrupts via a *second* connection dropping the `running` table, not by
/// replacing the DB file itself: `reg` keeps its own original connection
/// for its entire lifetime rather than ever reconnecting (see
/// `TestRegistration`'s own doc for why), so corrupting the path alone
/// wouldn't reach its cleanup at all — its connection's open file
/// descriptor still points at the original, valid inode regardless. A
/// second connection dropping the table is a schema change every
/// connection to that same file sees on its next statement, `reg`'s
/// included.
#[test]
fn drop_panics_loudly_on_a_corrupt_db_when_nothing_else_is_unwinding() {
    let (_dir, path) = temp_db();
    let reg = coordinate(&path, "normal_drop_corrupt_db", &[], SERIAL_NONE);

    // Corrupt the schema out from under `reg`'s own connection after
    // registration, via a second connection, so the DELETE inside `reg`'s
    // drop, below, fails.
    let (saboteur, _identity) = open_db(&path);
    saboteur.execute_batch("DROP TABLE running").unwrap();
    drop(saboteur);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(reg)));
    assert!(
        result.is_err(),
        "drop must panic when the DB is unusable and no other unwind is already in flight"
    );
}

// Failures recorded during an unwind =====

/// A registration dropped while its thread unwinds cannot panic (that aborts),
/// so its refusal to write through a broken DB is recorded process-wide for the
/// runner to fail the run with.
#[test]
fn drop_during_an_unwind_records_the_downgraded_failure() {
    let (_dir, path) = temp_db();
    let a = coordinate(&path, "a", &[], SERIAL_NONE);
    let (saboteur, _) = open_db(&path);
    saboteur.execute_batch("DROP TABLE running").unwrap();
    drop(saboteur);

    let result = std::thread::spawn(move || {
        let _held = a;
        panic!("the test's own failure");
    })
    .join();

    assert!(result.is_err());
    let recorded = crate::coordination::violations::recorded();
    assert!(
        recorded
            .iter()
            .any(|m| m.contains(&format!("{path:?}")) && m.contains("no such table")),
        "the downgraded failure must be recorded with its path and cause: {recorded:?}"
    );
}

// Init lock file permissions =====

/// Unix locks `db_path`'s parent directory itself, not a sibling `.lock`
/// file (see `lock.rs`'s module doc), so `open_db` must succeed — and
/// never touch that file at all — even when a `.lock` file left behind by
/// an older Skuld release (one that did lock a sibling file, published at
/// whatever mode a root lane's umask gave it) has no owner write bit.
#[cfg(unix)]
#[test]
fn open_db_succeeds_and_ignores_a_leftover_lock_file_with_no_owner_write_bit() {
    use std::os::unix::fs::PermissionsExt;

    let (_dir, path) = temp_db();
    let mut leftover_lock_name = path.as_os_str().to_owned();
    leftover_lock_name.push(".lock");
    let leftover_lock_path = std::path::PathBuf::from(leftover_lock_name);
    std::fs::write(&leftover_lock_path, b"").unwrap();
    std::fs::set_permissions(&leftover_lock_path, std::fs::Permissions::from_mode(0o444)).unwrap();

    let (conn, _identity) = open_db(&path);
    conn.execute_batch("PRAGMA journal_mode = WAL;")
        .expect("open_db must return a usable connection regardless of a leftover lock file's permissions");

    let meta = std::fs::metadata(&leftover_lock_path).unwrap();
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o444,
        "open_db must never touch a leftover lock file at all, let alone change its mode"
    );
}

// connect() ask-forgiveness retry =====

/// `connect()` opens first and only publishes on a `CANTOPEN` that
/// `symlink_metadata` confirms is genuine absence. A dangling symlink is
/// the other shape: `ensure_published`'s rename cannot replace it
/// (`RENAME_NOREPLACE` reports `EEXIST` against a symlink regardless of
/// what it points to, so publishing silently no-ops), and `symlink_metadata`
/// reports the link itself, not `NotFound` — so this is never treated as
/// absence. The open must not paper over that with `SQLITE_OPEN_CREATE`
/// either: doing so would silently create a fresh `0644 & ~umask` file
/// through the symlink, which is exactly the lockout this module exists to
/// prevent. `connect()` must instead panic loudly, naming the path, rather
/// than resurrect the file or retry forever.
#[cfg(unix)]
#[test]
fn connect_panics_loudly_on_a_dangling_symlink_instead_of_recreating_the_db() {
    let (_dir, path) = temp_db();
    std::os::unix::fs::symlink(path.with_file_name("does-not-exist"), &path).unwrap();

    let result = std::panic::catch_unwind(|| crate::coordination::connect(&path));
    let payload = result.expect_err("connect() must panic on a dangling symlink, not create a fresh file");
    let msg = crate::coordination::panic_payload_message(payload.as_ref());
    assert!(
        msg.contains(&path.to_string_lossy().into_owned()),
        "panic message should name the path: {msg:?}"
    );
    assert!(
        msg.contains("could not open coordination DB"),
        "panic message should be the neutral open-failure wording, not claim the DB vanished \
         (it doesn't: ensure_published recreates a plain absence): {msg:?}"
    );

    let meta = std::fs::symlink_metadata(&path).unwrap();
    assert!(
        meta.file_type().is_symlink(),
        "connect() must not have replaced the dangling symlink with a fresh file"
    );
}

/// `connect_with` runs under `path`'s init lock (via `connect`/`open_db`),
/// which excludes every other *Skuld* process, not external interference —
/// something outside Skuld (a human, another tool) can still delete
/// `.skuld.db` between `ensure_published` returning and the retried open
/// running, repeatedly, and `connect`'s own doc promises that any such
/// absence gets recreated fresh rather than panicking. This drives the
/// publish hook to leave `path` absent for the first two rounds and only
/// really publish on the third, proving the loop keeps retrying through
/// repeated genuine absence rather than giving up after one round trip —
/// there is no attempt cap.
#[cfg(unix)]
#[test]
fn connect_with_retries_through_repeated_genuine_absence_with_no_attempt_cap() {
    let (_dir, path) = temp_db();
    let mut publish_calls = 0u32;

    let conn = crate::coordination::connect_with(&path, |p| {
        publish_calls += 1;
        if publish_calls < 3 {
            return;
        }
        crate::coordination::publish::ensure_published(p);
    });

    conn.execute_batch("PRAGMA journal_mode = WAL;")
        .expect("the connection returned after the retries must be a usable, open database");
    assert_eq!(
        publish_calls, 3,
        "connect_with must call the publish hook again for every round of genuine absence"
    );
}

/// Many threads racing `open_db` against the same, initially-absent path,
/// each serialized through `path`'s init lock: since every `open_db` call
/// runs its whole open/recheck/publish sequence under that lock (see
/// `connect_with`'s doc), no two threads' opens, rechecks, or publishes can
/// interleave — at most one thread ever sees genuine absence and publishes;
/// every other thread, once it acquires the lock, finds the file already
/// published and just opens it on the first try. Every thread must return a
/// connection, never panic.
///
/// This does not exercise a recheck landing after a *concurrent* publish —
/// the lock rules that out entirely for real callers; see
/// `connect_with_panics_when_a_publish_lands_between_the_failed_open_and_the_recheck`
/// for that interleaving, driven directly against the unlocked
/// `connect_with` instead.
///
/// Repeated across many fresh paths in one test (rather than relying on a
/// single race) because a missing exclusion would only show up on some
/// iterations, not all of them.
#[cfg(unix)]
#[test]
fn connect_survives_many_threads_racing_the_same_absent_path() {
    const THREADS: usize = 16;
    const ROUNDS: usize = 20;

    for _ in 0..ROUNDS {
        let (_dir, path) = temp_db();
        let points = rendezvous(THREADS);
        std::thread::scope(|s| {
            for point in points {
                s.spawn(|| {
                    point.wait();
                    // `open_db`, not a bare `connect()`, matches how every
                    // real caller reaches `connect()`: it also runs schema
                    // creation through `retry_busy`, which absorbs any
                    // transient `SQLITE_BUSY` from schema-creation
                    // contention on its own — a raw `connect()` plus a bare
                    // `PRAGMA` here would instead surface that as an error,
                    // conflating it with the lock-serialized absence race
                    // this test targets.
                    let (_conn, _identity) = open_db(&path);
                });
            }
        });
    }
}

/// `connect_with` alone — unlike `connect()`/`open_db`, which always run it
/// under `path`'s init lock — has no way to tell a legitimate concurrent
/// publisher's rename, landing between the failed open and the
/// `symlink_metadata` recheck, apart from a genuinely broken entry (a
/// dangling symlink or a directory): both present as "something's there
/// now" to that recheck, and `connect_with` panics either way (see its own
/// doc). That is exactly the interleaving the init lock exists to rule out
/// for every real caller. Proven here via `connect_with_hooks` — the same
/// implementation `connect_with` itself runs on, with a hook landing a real
/// publish in the gap between the failed open and the recheck
/// deterministically, rather than via a race between threads: `connect_with`
/// must panic, not silently succeed, confirming the lock — not the recheck
/// logic itself — is what makes that interleaving safe.
#[cfg(unix)]
#[test]
fn connect_with_panics_when_a_publish_lands_between_the_failed_open_and_the_recheck() {
    let (_dir, path) = temp_db();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::coordination::connect_with_hooks(
            &path,
            // The simulated concurrent publisher: a legitimate publish,
            // landing right where `connect_with`'s doc says an unlocked
            // caller cannot tell it apart from a genuinely broken entry.
            crate::coordination::publish::ensure_published,
            |_p| {
                panic!(
                    "ensure_published must not run: the recheck should already have found the \
                     path present (from the simulated concurrent publish) and panicked before \
                     ever calling the publish hook"
                )
            },
        )
    }));

    let payload = result.expect_err(
        "connect_with must panic, not silently succeed, when its recheck finds a path a \
         concurrent (lock-unexcluded) publisher already landed — this is why every real caller \
         holds path's init lock for connect_with's whole open/recheck/publish sequence",
    );
    let msg = crate::coordination::panic_payload_message(payload.as_ref());
    assert!(
        msg.contains("could not open coordination DB"),
        "panic message should be connect_with's own open-failure wording: {msg:?}"
    );
}

// The `SQLITE_READONLY` WAL cold-start race documented on `open_db` has no
// direct regression test anywhere in this crate: SQLite's own unix VFS
// serializes `-shm` creation across every thread *of one process* through a
// process-local mutex, so the race is only observable between genuinely
// separate OS processes and isn't reliably reproducible in-process.
// `tests/lock_contention_regression.rs` instead deterministically guards the
// mechanism that removes this race outright — `lock::with_init_lock`'s
// mutual exclusion — which is provable without needing to reproduce the
// race it was built to remove.

// Windows is_pid_alive: only "no such process" means dead =====

/// `clean_stale_entries` must not delete a live process's row just because
/// `OpenProcess` failed for a reason other than "no such process" —
/// `ERROR_ACCESS_DENIED` (a process owned by another user, or a
/// protected/elevated one) means the process exists but we can't query it,
/// same shape as Unix's `EPERM`. Only `ERROR_INVALID_PARAMETER` — what
/// `OpenProcess` returns for a nonexistent PID — means dead.
#[cfg(windows)]
#[test]
fn win32_open_process_error_means_dead_only_for_invalid_parameter() {
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_INVALID_PARAMETER};

    assert!(
        crate::coordination::win32_open_process_error_means_dead(ERROR_INVALID_PARAMETER.to_hresult()),
        "ERROR_INVALID_PARAMETER (no such process) must mean dead"
    );
    assert!(
        !crate::coordination::win32_open_process_error_means_dead(ERROR_ACCESS_DENIED.to_hresult()),
        "ERROR_ACCESS_DENIED must mean alive-but-inaccessible, mirroring Unix's EPERM"
    );
    assert!(
        !crate::coordination::win32_open_process_error_means_dead(ERROR_FILE_NOT_FOUND.to_hresult()),
        "an unrelated error code must not be treated as 'no such process' either"
    );
}
