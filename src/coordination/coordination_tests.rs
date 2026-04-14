//! Tests for the SQLite coordination module.

use std::sync::atomic::{AtomicU32, Ordering::SeqCst};
use std::sync::Barrier;
use std::time::Duration;

use crate::coordination::{can_start, coordinate, is_busy, open_db, register, SERIAL_ALL, SERIAL_NONE};
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
    let (_dir, path) = temp_db();

    let barrier = Barrier::new(THREADS);
    let peak = AtomicU32::new(0);
    let running = AtomicU32::new(0);

    std::thread::scope(|s| {
        for _ in 0..THREADS {
            s.spawn(|| {
                barrier.wait();
                let _reg = coordinate(&path, "parallel_test", &[], SERIAL_NONE);
                let n = running.fetch_add(1, SeqCst) + 1;
                peak.fetch_max(n, SeqCst);
                std::thread::sleep(Duration::from_millis(50));
                running.fetch_sub(1, SeqCst);
            });
        }
    });

    assert!(peak.load(SeqCst) > 1, "non-serial tests should run concurrently");
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
    let _reg = coordinate(&path, "waiter", &[], SERIAL_NONE);

    holder.join().unwrap();
}

// is_busy =====

#[test]
fn is_busy_matches_expected_codes() {
    use rusqlite::{ffi, Error, ErrorCode};

    let busy = Error::SqliteFailure(ffi::Error::new(ffi::SQLITE_BUSY), Some("database is locked".into()));
    let locked = Error::SqliteFailure(ffi::Error::new(ffi::SQLITE_LOCKED), None);
    let other = Error::SqliteFailure(ffi::Error::new(ffi::SQLITE_CONSTRAINT), None);

    assert_eq!(busy.sqlite_error_code(), Some(ErrorCode::DatabaseBusy));
    assert_eq!(locked.sqlite_error_code(), Some(ErrorCode::DatabaseLocked));
    assert_eq!(other.sqlite_error_code(), Some(ErrorCode::ConstraintViolation));

    assert!(is_busy(&busy));
    assert!(is_busy(&locked));
    assert!(!is_busy(&other));
}
