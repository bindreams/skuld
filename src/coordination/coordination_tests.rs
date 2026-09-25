//! Tests for the SQLite coordination module.

use std::sync::atomic::{AtomicU32, Ordering::SeqCst};
use std::sync::Barrier;
use std::time::Duration;

use crate::coordination::{can_start, coordinate, is_retryable, open_db, register, SERIAL_ALL, SERIAL_NONE};
use crate::label::Label;

/// Create a temporary database for testing.
fn temp_db() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test-coordination.db");
    (dir, path)
}

// can_start =====

#[test]
fn non_serial_can_start_when_empty() {
    let (_dir, path) = temp_db();
    let conn = open_db(&path);
    assert!(can_start(&conn, &[], SERIAL_NONE).unwrap());
}

#[test]
fn non_serial_blocked_by_global_serial() {
    let (_dir, path) = temp_db();
    let conn = open_db(&path);
    register(&conn, "blocker", &[], SERIAL_ALL).unwrap();
    assert!(!can_start(&conn, &[], SERIAL_NONE).unwrap());
}

#[test]
fn non_serial_blocked_by_matching_filter() {
    let (_dir, path) = temp_db();
    let conn = open_db(&path);
    let docker = Label::__new("docker");
    // A serial test filtering on "docker" is running
    register(&conn, "serial_docker", &[], "docker").unwrap();
    // A test WITH label docker is blocked
    assert!(!can_start(&conn, &[docker], SERIAL_NONE).unwrap());
    // A test WITHOUT label docker is NOT blocked
    assert!(can_start(&conn, &[], SERIAL_NONE).unwrap());
}

#[test]
fn global_serial_blocked_when_anything_running() {
    let (_dir, path) = temp_db();
    let conn = open_db(&path);
    register(&conn, "some_test", &[], SERIAL_NONE).unwrap();
    assert!(!can_start(&conn, &[], SERIAL_ALL).unwrap());
}

#[test]
fn global_serial_can_start_when_empty() {
    let (_dir, path) = temp_db();
    let conn = open_db(&path);
    assert!(can_start(&conn, &[], SERIAL_ALL).unwrap());
}

#[test]
fn filtered_serial_blocked_by_matching_running_test() {
    let (_dir, path) = temp_db();
    let conn = open_db(&path);
    let docker = Label::__new("docker");
    // A non-serial test with label "docker" is running
    register(&conn, "docker_test", &[docker], SERIAL_NONE).unwrap();
    // A serial test filtering on "docker" is blocked
    assert!(!can_start(&conn, &[], "docker").unwrap());
}

#[test]
fn filtered_serial_not_blocked_by_non_matching() {
    let (_dir, path) = temp_db();
    let conn = open_db(&path);
    let network = Label::__new("network");
    // A test with label "network" is running
    register(&conn, "network_test", &[network], SERIAL_NONE).unwrap();
    // A serial test filtering on "docker" is NOT blocked
    assert!(can_start(&conn, &[], "docker").unwrap());
}

#[test]
fn filtered_serial_and_semantics() {
    let (_dir, path) = temp_db();
    let conn = open_db(&path);
    let a = Label::__new("a");
    let b = Label::__new("b");

    // Test with only [a] is running
    register(&conn, "test_a", &[a], SERIAL_NONE).unwrap();
    // serial = "a & b" should NOT be blocked (running test doesn't have both a and b)
    assert!(can_start(&conn, &[], "a & b").unwrap());

    // Now add a test with [a, b]
    register(&conn, "test_ab", &[a, b], SERIAL_NONE).unwrap();
    // serial = "a & b" IS now blocked
    assert!(!can_start(&conn, &[], "a & b").unwrap());
}

#[test]
fn filtered_serial_not_semantics() {
    let (_dir, path) = temp_db();
    let conn = open_db(&path);
    let a = Label::__new("a");
    let b = Label::__new("b");

    // Test with label [a] is running
    register(&conn, "test_a", &[a], SERIAL_NONE).unwrap();
    // serial = "!a" should NOT be blocked (running test HAS label a)
    assert!(can_start(&conn, &[], "!a").unwrap());

    // Test with label [b] is running (no label a)
    register(&conn, "test_b", &[b], SERIAL_NONE).unwrap();
    // serial = "!a" IS now blocked (test_b doesn't have a, so !a matches)
    assert!(!can_start(&conn, &[], "!a").unwrap());
}

// register / cleanup =====

#[test]
fn register_and_delete() {
    let (_dir, path) = temp_db();
    let conn = open_db(&path);
    let docker = Label::__new("docker");
    let id = register(&conn, "my_test", &[docker], SERIAL_NONE).unwrap();

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
    let conn = open_db(&path);
    let a = Label::__new("a");
    let b = Label::__new("b");
    let id = register(&conn, "test", &[a, b], SERIAL_NONE).unwrap();

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
        let conn = open_db(&path);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM running", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
    // After drop, the entry should be gone
    let conn = open_db(&path);
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
    let conn = open_db(&path);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM running", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

// Concurrent coordination =====

#[test]
fn global_serial_prevents_concurrent_execution() {
    const THREADS: usize = 8;
    let (_dir, path) = temp_db();

    let barrier = Barrier::new(THREADS);
    let running = AtomicU32::new(0);

    std::thread::scope(|s| {
        for _ in 0..THREADS {
            s.spawn(|| {
                barrier.wait();
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

    // Two barriers: the first races every thread into coordinate() together
    // (stressing lock contention); the second holds every thread past
    // fetch_add before any exits, so peak == THREADS on success regardless
    // of per-thread coordinate() latency. If coordination regresses and
    // serializes non-serial tests, the second barrier deadlocks — the CI
    // job-level timeout is the intended backstop.
    let entry = Barrier::new(THREADS);
    let observation = Barrier::new(THREADS);
    let peak = AtomicU32::new(0);
    let running = AtomicU32::new(0);

    std::thread::scope(|s| {
        for _ in 0..THREADS {
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
    let conn = open_db(&path);
    assert!(!can_start(&conn, &[docker], SERIAL_NONE).unwrap());
}

#[test]
fn coordinate_retries_on_busy_lock() {
    use std::sync::mpsc;

    let (_dir, path) = temp_db();

    // Holder grabs EXCLUSIVE and holds it longer than the waiter's
    // busy_timeout (5 s). Empirically the busy handler can take up to
    // ~5.5 s to surrender, so we hold for 7 s to force a SQLITE_BUSY
    // return inside coordinate(). The waiter must survive this via the
    // outer retry loop instead of panicking on .unwrap().
    let holder_path = path.clone();
    let (lock_tx, lock_rx) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        let conn = open_db(&holder_path);
        conn.execute_batch("BEGIN EXCLUSIVE").unwrap();
        lock_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(7_000));
        conn.execute_batch("COMMIT").unwrap();
    });

    lock_rx.recv().unwrap();
    // On broken code: panics at src/coordination.rs:261 after ~5.5 s.
    // On fixed code: the next outer-loop iteration's BEGIN EXCLUSIVE
    // succeeds once the holder commits, then register() + COMMIT succeed.
    let waiter_started = std::time::Instant::now();
    let _reg = coordinate(&path, "waiter", &[], SERIAL_NONE);
    let waited = waiter_started.elapsed();

    holder.join().unwrap();

    // Guard against regression to a non-contending fast path: coordinate()
    // must have actually exhausted busy_timeout (5 s) at least once before
    // succeeding.
    assert!(
        waited >= Duration::from_secs(5),
        "waiter returned in {waited:?}; should have hit busy_timeout (>=5s)"
    );
}

// is_retryable =====

#[test]
fn is_retryable_matches_busy_and_locked_only() {
    use rusqlite::{ffi, Error};

    let busy = Error::SqliteFailure(ffi::Error::new(ffi::SQLITE_BUSY), Some("database is locked".into()));
    let locked = Error::SqliteFailure(ffi::Error::new(ffi::SQLITE_LOCKED), None);
    let constraint = Error::SqliteFailure(ffi::Error::new(ffi::SQLITE_CONSTRAINT), None);

    assert!(is_retryable(&busy));
    assert!(is_retryable(&locked));
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
    let conn = open_db(&path);
    assert_eq!(stored_serial_filter(&conn, "redundant"), "a");
}

#[test]
fn coordinate_collapses_tautology_to_global_serial_sentinel() {
    let (_dir, path) = temp_db();
    let _reg = coordinate(&path, "taut", &[], "a | !a");
    let conn = open_db(&path);
    assert_eq!(stored_serial_filter(&conn, "taut"), SERIAL_ALL);
}

#[test]
fn coordinate_collapses_contradiction_to_non_serial_sentinel() {
    let (_dir, path) = temp_db();
    let _reg = coordinate(&path, "contra", &[], "a & !a");
    let conn = open_db(&path);
    assert_eq!(stored_serial_filter(&conn, "contra"), SERIAL_NONE);
}

#[test]
fn coordinate_preserves_serial_none_sentinel() {
    let (_dir, path) = temp_db();
    let _reg = coordinate(&path, "none", &[], SERIAL_NONE);
    let conn = open_db(&path);
    assert_eq!(stored_serial_filter(&conn, "none"), SERIAL_NONE);
}

#[test]
fn coordinate_preserves_serial_all_sentinel() {
    // Use a fresh DB so SERIAL_ALL isn't blocked by the SERIAL_NONE row above.
    let (_dir, path) = temp_db();
    let _reg = coordinate(&path, "all", &[], SERIAL_ALL);
    let conn = open_db(&path);
    assert_eq!(stored_serial_filter(&conn, "all"), SERIAL_ALL);
}

// Schema migration v0 → v1 =====

#[test]
fn migration_rewrites_legacy_non_canonical_rows() {
    let (_dir, path) = temp_db();
    // Open and seed the DB with the OLD schema (user_version still 0) plus
    // a row containing a legacy non-canonical serial_filter that happens to
    // simplify to the canonical "a".
    let conn = open_db(&path);
    // Reset version so the migration runs again on the next open.
    conn.execute("PRAGMA user_version = 0", []).unwrap();
    register(&conn, "legacy", &[], "(a) | (a)").unwrap();
    drop(conn);

    // Re-open. open_db should run migrate_schema and rewrite the legacy row
    // in place. After the migration, user_version is 1 and the row's filter
    // is the canonical Display form.
    let conn = open_db(&path);
    let stored: String = stored_serial_filter(&conn, "legacy");
    assert_eq!(stored, "a", "legacy row should be canonicalized");
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
    assert_eq!(version, 1, "schema version should bump to 1");
}

#[test]
fn migration_skips_already_canonical_rows() {
    let (_dir, path) = temp_db();
    let conn = open_db(&path);
    conn.execute("PRAGMA user_version = 0", []).unwrap();
    register(&conn, "already_canonical", &[], "a").unwrap();
    drop(conn);

    let conn = open_db(&path);
    assert_eq!(stored_serial_filter(&conn, "already_canonical"), "a");
}

#[test]
fn migration_leaves_unparseable_live_rows_alone() {
    let (_dir, path) = temp_db();
    let conn = open_db(&path);
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

    let conn = open_db(&path);
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
/// must still panic loudly — that's the whole point of `connect()` calling
/// `unwrap_or_else(|e| panic!(...))` on the underlying SQLite open, and
/// downgrading unconditionally would silently swallow every one of them.
///
/// Corrupts by replacing the DB file with a directory rather than
/// `chmod`ing it narrow, so this runs on every platform, for the reasons
/// documented on `probe_drop_panic_during_unwind` in `src/lib.rs`.
#[test]
fn drop_panics_loudly_on_a_corrupt_db_when_nothing_else_is_unwinding() {
    let (_dir, path) = temp_db();
    let reg = coordinate(&path, "normal_drop_corrupt_db", &[], SERIAL_NONE);

    // Corrupt the DB after registration so the connect() call inside
    // `reg`'s drop, below, fails.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(reg)));
    assert!(
        result.is_err(),
        "drop must panic when the DB is unusable and no other unwind is already in flight"
    );
}

// Init lock file permissions =====

/// Regression guard for H1: the lock file must never need write access to
/// be locked (`flock`/`LockFileEx` only ever need read access on the
/// handle — see `lock.rs`'s module doc), so `open_db` must succeed even
/// against a pre-existing lock file that lacks the owner write bit
/// entirely. Before the fix, `open_lock_file` opened with `.write(true)`,
/// which would fail `EACCES` here exactly the way a root-published,
/// 0644-narrowed-by-umask lock file would fail a later non-root run.
#[cfg(unix)]
#[test]
fn open_db_succeeds_against_a_preexisting_lock_file_with_no_owner_write_bit() {
    use std::os::unix::fs::PermissionsExt;

    let (_dir, path) = temp_db();
    let lock_path = crate::coordination::lock::lock_path(&path);
    std::fs::write(&lock_path, b"").unwrap();
    std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o444)).unwrap();

    let conn = open_db(&path);
    conn.execute_batch("PRAGMA journal_mode = WAL;")
        .expect("open_db must return a usable connection even with a read-only lock file");

    let meta = std::fs::metadata(&lock_path).unwrap();
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o444,
        "open_db must not have changed the lock file's mode"
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

/// Many threads racing `connect()` against the same, initially-absent path:
/// one thread's failed open (genuinely absent at that instant) can have its
/// `symlink_metadata` recheck land *after* a concurrent thread's publish has
/// already landed the file — the open failed because of absence, but by the
/// time this thread looks, absence is no longer what `symlink_metadata`
/// reports. That must not be mistaken for a dangling-symlink/directory kind
/// of brokenness and panic; the file is simply there now, and the very next
/// open succeeds. Every thread must return a connection, never panic.
///
/// Repeated across many fresh paths in one test (rather than relying on a
/// single race) because the window this exercises is a handful of syscalls
/// wide — one iteration is not a reliable enough probe on its own.
#[cfg(unix)]
#[test]
fn connect_survives_many_threads_racing_the_same_absent_path() {
    const THREADS: usize = 16;
    const ROUNDS: usize = 20;

    for _ in 0..ROUNDS {
        let (_dir, path) = temp_db();
        let barrier = Barrier::new(THREADS);
        std::thread::scope(|s| {
            for _ in 0..THREADS {
                s.spawn(|| {
                    barrier.wait();
                    // `open_db`, not a bare `connect()`, matches how every
                    // real caller reaches `connect()`: it sets
                    // `busy_timeout` before touching the DB, which a raw
                    // `connect()` deliberately doesn't (that's `open_db`'s
                    // job, not `connect`'s) — asserting through a bare
                    // `PRAGMA` here would conflate `SQLITE_BUSY` from that
                    // missing timeout with the absence/TOCTOU race this
                    // test targets.
                    let _conn = open_db(&path);
                });
            }
        });
    }
}

// The `SQLITE_READONLY` WAL cold-start race documented on `open_db` (a
// connection that loses `PRAGMA journal_mode = WAL`'s negotiation over the
// freshly-created `-shm` file can come back permanently readonly for its
// own lifetime) has no direct regression test anywhere in this crate:
// SQLite's own unix VFS serializes `-shm` creation across every thread *of
// one process* through a process-local mutex, so the race is invisible to
// threads sharing a process, only observable (if at all) between genuinely
// separate OS processes — and even a genuine-subprocess version of this
// same write-based probe (16 processes x 20 rounds, barrier-synchronized)
// never reproduced it against pre-lock code either, so it was dropped
// rather than kept as a test that had never demonstrably failed on the code
// it was meant to guard. `tests/lock_contention_regression.rs` instead
// deterministically guards the mechanism that removes this race outright —
// `lock::with_init_lock`'s mutual exclusion — which is provable without
// needing to reproduce the race it was built to remove.

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
