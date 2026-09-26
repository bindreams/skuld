//! Tests for the coordination DB init lock ([`super::lock`]).

#[cfg(windows)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering::SeqCst};
use std::sync::Barrier;

#[cfg(windows)]
use super::lock::lock_path;
#[cfg(unix)]
use super::lock::{lock_exclusive, EINTR_RETRIES};
use super::lock::{open_lock_target, try_lock_exclusive, with_init_lock};

#[cfg(windows)]
#[test]
fn lock_path_appends_dot_lock_to_the_full_db_path_verbatim() {
    let db_path = PathBuf::from(r"C:\some\dir\.skuld.db");
    assert_eq!(lock_path(&db_path), PathBuf::from(r"C:\some\dir\.skuld.db.lock"));
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
///
/// Windows-only: Unix has no lock file to race a `create(true)` open
/// against — it locks `db_path`'s parent directory, which already exists
/// before [`with_init_lock`] is ever called (see `lock.rs`'s module doc) —
/// so this scenario doesn't arise there; the general concurrent-callers
/// test above already covers Unix's actual concurrency shape.
#[cfg(windows)]
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
        let fresh = open_lock_target(&db_path);
        match try_lock_exclusive(&fresh) {
            Err(std::fs::TryLockError::WouldBlock) => {}
            other => panic!(
                "a fresh handle's try_lock() must report WouldBlock while with_init_lock \
                 already holds the exclusive lock, got {other:?}"
            ),
        }
    });
}

/// Regression guard: a missing parent directory must panic `with_init_lock`
/// immediately, not spin forever treating "can't open" as "try again."
/// `open_lock_target` has no retry loop, so a missing directory can't
/// resolve itself by trying the open again.
#[test]
fn with_init_lock_panics_immediately_when_the_profile_directory_does_not_exist_instead_of_spinning() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("nonexistent-subdir").join(".skuld.db");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| with_init_lock(&db_path, || {})));

    assert!(
        result.is_err(),
        "with_init_lock must panic when db_path's parent directory doesn't exist, not hang"
    );
}

/// `lock_exclusive`'s blocking `flock` must retry past `EINTR`, not surface
/// it as a failure: a process with a handler installed without
/// `SA_RESTART` for some signal unrelated to this crate — a test harness's
/// own signal handling, for example — can have this call interrupted by it,
/// and this crate has no say over that handler's flags.
///
/// Proven with a real interrupt, not a mocked error. `flock` locks are
/// scoped to the open file description, not the process or thread, so a
/// second, independently-opened handle on the same lock target genuinely
/// blocks behind the first. A non-`SA_RESTART` `SIGUSR1` handler is
/// installed, then the holder thread bombards the blocked thread with that
/// signal — via its `pthread_t`, reported back over a channel rather than
/// assumed, and with no sleep — until `EINTR_RETRIES` (incremented only on
/// the real `EINTR` arm inside `lock_exclusive`, never anywhere in this
/// test) proves a signal actually landed inside the blocking syscall, not
/// just before or after it. Only then does the holder release the lock;
/// the blocked call must still go on to succeed.
#[cfg(unix)]
#[test]
fn lock_exclusive_retries_past_eintr_from_a_non_restarting_handler() {
    // Other tests in this same process may also drive lock_exclusive's
    // EINTR arm incidentally (unlikely, but the counter is process-wide,
    // shared with every other #[test] in this binary) — pin the baseline
    // actually observed instead of assuming zero.
    let baseline = EINTR_RETRIES.load(SeqCst);

    extern "C" fn noop_handler(_signum: libc::c_int) {}

    // Safety: installs a process-wide handler for SIGUSR1 with no
    // SA_RESTART, so a blocking syscall this handler interrupts reports
    // EINTR instead of resuming — the exact condition under test. No other
    // test in this binary sends or handles SIGUSR1.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = noop_handler as *const () as libc::sighandler_t;
        assert_eq!(libc::sigemptyset(&mut action.sa_mask), 0, "sigemptyset failed");
        action.sa_flags = 0; // deliberately omits SA_RESTART
        assert_eq!(
            libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()),
            0,
            "sigaction(SIGUSR1) failed: {}",
            std::io::Error::last_os_error()
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join(".skuld.db");

    let holder = open_lock_target(&db_path);
    lock_exclusive(&holder).expect("uncontended lock must succeed immediately");
    let waiter = open_lock_target(&db_path);

    let (tid_tx, tid_rx) = std::sync::mpsc::channel();
    let waiter_thread = std::thread::spawn(move || {
        // Report this thread's own id before doing anything blocking, so
        // the main thread never has to guess whether the id it has is
        // still valid.
        tid_tx
            .send(unsafe { libc::pthread_self() })
            .expect("main thread must still be waiting to receive the pthread id");
        lock_exclusive(&waiter)
    });
    let waiter_pthread = tid_rx.recv().expect("waiter thread must report its pthread id");

    // Bombard the blocked thread with the non-restarting signal until
    // there's direct evidence — lock_exclusive's own retry counter moving —
    // that a real EINTR was retried, not just that the call eventually
    // returned. Uncapped: it only stops once that evidence exists. The
    // holder keeps the lock the whole time, so the waiter thread has
    // nowhere to go but blocked inside flock (or the brief gap between a
    // delivered signal and re-entering it) for as long as this loop runs.
    while EINTR_RETRIES.load(SeqCst) == baseline {
        // Safety: waiter_pthread names a thread that is still alive and
        // unjoined for the entire loop — its JoinHandle isn't joined until
        // after the loop exits below.
        let rc = unsafe { libc::pthread_kill(waiter_pthread, libc::SIGUSR1) };
        assert_eq!(rc, 0, "pthread_kill(SIGUSR1) failed with errno {rc}");
    }

    drop(holder);

    waiter_thread
        .join()
        .expect("waiter thread must not panic")
        .expect("lock_exclusive must still succeed once EINTR is retried past and the lock is free");

    assert!(
        EINTR_RETRIES.load(SeqCst) > baseline,
        "test precondition: at least one EINTR must have been retried inside lock_exclusive"
    );
}
