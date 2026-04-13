//! Tests for the serial lock mechanism in [`crate::runner`].

use std::panic::catch_unwind;
use std::sync::atomic::{AtomicU32, Ordering::SeqCst};
use std::sync::Barrier;
use std::time::Duration;

use crate::runner::run_maybe_serial;

#[test]
fn serial_lock_prevents_concurrent_execution() {
    const THREADS: usize = 8;

    let barrier = Barrier::new(THREADS);
    let running = AtomicU32::new(0);

    std::thread::scope(|s| {
        for _ in 0..THREADS {
            s.spawn(|| {
                barrier.wait();
                run_maybe_serial(true, || {
                    running.fetch_add(1, SeqCst);
                    std::thread::sleep(Duration::from_millis(10));
                    assert_eq!(running.load(SeqCst), 1, "serial lock allowed concurrent execution");
                    running.fetch_sub(1, SeqCst);
                });
            });
        }
    });
}

#[test]
fn non_serial_allows_concurrent_execution() {
    const THREADS: usize = 8;

    let barrier = Barrier::new(THREADS);
    let peak = AtomicU32::new(0);
    let running = AtomicU32::new(0);

    std::thread::scope(|s| {
        for _ in 0..THREADS {
            s.spawn(|| {
                barrier.wait();
                run_maybe_serial(false, || {
                    let n = running.fetch_add(1, SeqCst) + 1;
                    peak.fetch_max(n, SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    running.fetch_sub(1, SeqCst);
                });
            });
        }
    });

    assert!(
        peak.load(SeqCst) > 1,
        "non-serial path should allow concurrent execution"
    );
}

#[test]
fn serial_lock_recovers_after_panic() {
    // A panic inside the body unwinds the stack, dropping the fd-lock
    // guard and closing the file. The next caller opens a fresh file and
    // acquires the lock without issue.
    let _ = catch_unwind(|| {
        run_maybe_serial(true, || panic!("intentional panic"));
    });

    // If the lock were somehow leaked, this would deadlock.
    let mut entered = false;
    run_maybe_serial(true, || {
        entered = true;
    });
    assert!(entered, "serial lock should be acquirable after a prior panic");
}

// retry_on_eintr tests -----

/// Retry a blocking I/O operation if interrupted by a signal (`EINTR`).
///
/// This is the same logic inlined in `with_serial_lock` (which cannot call this
/// function directly because `fd_lock::RwLock::write()` returns a guard that
/// borrows the lock, and that borrow cannot escape an `FnMut` closure). These
/// tests validate the retry pattern is correct.
fn retry_on_eintr<T>(mut f: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    loop {
        match f() {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

#[test]
fn retry_on_eintr_retries_interrupted() {
    let attempts = AtomicU32::new(0);
    let result = retry_on_eintr(|| {
        if attempts.fetch_add(1, SeqCst) < 3 {
            Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "EINTR"))
        } else {
            Ok(42)
        }
    });
    assert_eq!(result.unwrap(), 42);
    assert_eq!(attempts.load(SeqCst), 4); // 3 retries + 1 success
}

#[test]
fn retry_on_eintr_propagates_other_errors() {
    let result: std::io::Result<()> =
        retry_on_eintr(|| Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "EACCES")));
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);
}

#[test]
fn retry_on_eintr_succeeds_immediately() {
    let result = retry_on_eintr(|| Ok(99));
    assert_eq!(result.unwrap(), 99);
}
