//! Tests for the coordination DB init lock ([`super::lock`]).

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering::SeqCst};
use std::sync::Barrier;

use super::lock::{lock_path, with_init_lock};

#[test]
fn lock_path_appends_dot_lock_to_the_full_db_path_verbatim() {
    let db_path = PathBuf::from("/some/dir/.skuld.db");
    assert_eq!(lock_path(&db_path), PathBuf::from("/some/dir/.skuld.db.lock"));
}

/// `with_init_lock` must be a *mutual exclusion* primitive, not just "don't
/// panic under concurrency": many threads race to enter the same critical
/// section at once, lined up on a `Barrier` so they all arrive together —
/// maximizing the chance a missing exclusion would show up — and each
/// checks, via an atomic counter rather than a sleep-widened window, that
/// it is ever the *only* thread inside. A single overlap anywhere across
/// any thread's dwell is a hard failure, not a flaky one: the counter only
/// reads above 1 while two threads are genuinely both inside at once, so
/// this either catches a real violation or it doesn't run into one — there
/// is nothing timing-dependent about what counts as a failure.
#[test]
fn with_init_lock_serializes_concurrent_callers() {
    const THREADS: usize = 32;
    const ROUNDS: usize = 20;

    for _ in 0..ROUNDS {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join(".skuld.db");
        let in_critical_section = AtomicI64::new(0);
        let max_seen = AtomicI64::new(0);
        let barrier = Barrier::new(THREADS);

        std::thread::scope(|s| {
            for _ in 0..THREADS {
                s.spawn(|| {
                    barrier.wait();
                    with_init_lock(&db_path, || {
                        let now = in_critical_section.fetch_add(1, SeqCst) + 1;
                        max_seen.fetch_max(now, SeqCst);
                        // Give any missing exclusion room to show up: real
                        // CPU work inside the section, not a sleep — the
                        // failure signal is the atomic counter above, not
                        // timing.
                        let mut acc = 0u64;
                        for i in 0..10_000u64 {
                            acc = acc.wrapping_add(i);
                        }
                        std::hint::black_box(acc);
                        in_critical_section.fetch_sub(1, SeqCst);
                    });
                });
            }
        });

        assert_eq!(
            max_seen.load(SeqCst),
            1,
            "with_init_lock let more than one thread into the critical section at once"
        );
    }
}

/// Two lock files racing to be created (`OpenOptions::create(true)`, not
/// `create_new`) is the concurrency shape a `<db path>.lock` file itself is
/// exposed to on every real call, since [`with_init_lock`] never
/// pre-creates it ahead of time. Proven the same way as above: real
/// concurrent creation, checked for exclusivity, not just absence of a
/// panic.
#[test]
fn with_init_lock_serializes_even_when_the_lock_file_itself_does_not_exist_yet() {
    const THREADS: usize = 32;

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join(".skuld.db");
    assert!(
        !lock_path(&db_path).exists(),
        "test precondition: lock file must not pre-exist"
    );

    let in_critical_section = AtomicI64::new(0);
    let max_seen = AtomicI64::new(0);
    let barrier = Barrier::new(THREADS);

    std::thread::scope(|s| {
        for _ in 0..THREADS {
            s.spawn(|| {
                barrier.wait();
                with_init_lock(&db_path, || {
                    let now = in_critical_section.fetch_add(1, SeqCst) + 1;
                    max_seen.fetch_max(now, SeqCst);
                    in_critical_section.fetch_sub(1, SeqCst);
                });
            });
        }
    });

    assert_eq!(
        max_seen.load(SeqCst),
        1,
        "with_init_lock let more than one thread into the critical section at once"
    );
}

/// Deterministic counterpart to the two statistical tests above: rather than
/// racing many threads and checking they never overlap, this holds
/// `with_init_lock` open and, from *inside* it, `try_lock`s a completely
/// fresh handle on the same lock file. `flock`/`LockFileEx` locks are scoped
/// to the open file description/handle, not the process or thread, so a
/// second, independently-opened handle contending the same lock — even from
/// the same thread — must report `WouldBlock`, not silently succeed. This
/// can't flake: there is no timing window to miss, since the fresh `try_lock`
/// only ever runs while `with_init_lock`'s own lock is provably still held.
#[test]
fn a_fresh_try_lock_reports_would_block_while_with_init_lock_holds_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join(".skuld.db");

    with_init_lock(&db_path, || {
        let fresh = std::fs::OpenOptions::new()
            .read(true)
            .open(lock_path(&db_path))
            .expect("lock file must already exist while with_init_lock holds it");
        match fresh.try_lock() {
            Err(std::fs::TryLockError::WouldBlock) => {}
            other => panic!(
                "a fresh handle's try_lock() must report WouldBlock while with_init_lock \
                 already holds the exclusive lock, got {other:?}"
            ),
        }
    });
}
