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
    // A panic inside the body poisons the in-process Mutex. The lock must
    // still be acquirable by subsequent callers (via unwrap_or_else on the
    // PoisonError).
    let _ = catch_unwind(|| {
        run_maybe_serial(true, || panic!("intentional panic"));
    });

    // If poison recovery is broken, this will deadlock or panic.
    let mut entered = false;
    run_maybe_serial(true, || {
        entered = true;
    });
    assert!(entered, "serial lock should be acquirable after a prior panic");
}
