//! Subject of subprocess invocations in `tests/lock_emfile_regression.rs`.
//! Not a real product binary.
//!
//! Lowers this fresh process's own `RLIMIT_NOFILE` soft limit to 3 —
//! exactly stdin/stdout/stderr, which a freshly spawned child already has
//! open, and nothing more — before touching the coordination DB path at
//! all. The kernel refuses a new `open()` once the number of descriptors
//! already open reaches the soft limit, so with the limit set at or below
//! the 3 already open, the very next `open()` this process performs is
//! guaranteed to fail `EMFILE`. No loop is needed to reach that state, and
//! this doesn't depend on whatever the platform's real (much higher, and
//! environment-dependent) default limit happens to be.
//!
//! Regression guard for M-a: `open_lock_target`'s single open-and-panic (no
//! retry loop) must turn that `EMFILE` into an immediate panic, not an
//! infinite spin that mistakes "can't open" for "try again."
//!
//! Reads `SKULD_LOCK_EMFILE_PROBE_DB` (required: the coordination DB path
//! to try to lock). Never returns normally: `probe_hold_init_lock`'s
//! `open_lock_target` call must panic before ever reaching the closure
//! below — a normal return, or the closure running, is itself the failure
//! this probe exists to catch.

// EMFILE via RLIMIT_NOFILE is a Unix concept; keep this binary buildable
// everywhere so `cargo build --workspace` never breaks on Windows, but only
// the Unix half does anything — the driver test file gates its subprocess
// calls to `#[cfg(unix)]` too, so the fallback below is never exercised.
#[cfg(unix)]
fn main() {
    let db_path = std::env::var("SKULD_LOCK_EMFILE_PROBE_DB").expect("driver must set SKULD_LOCK_EMFILE_PROBE_DB");
    let db_path = std::path::Path::new(&db_path);

    let lim = libc::rlimit {
        rlim_cur: 3,
        rlim_max: 3,
    };
    // SAFETY: `lim` is a fully-initialized plain value that setrlimit only
    // reads, and this process has no threads besides the one running main
    // (nothing before this point spawns any).
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) };
    assert_eq!(
        rc,
        0,
        "lock_emfile_probe: setrlimit(RLIMIT_NOFILE, 3) failed: {}",
        std::io::Error::last_os_error()
    );

    skuld::__private::probe_hold_init_lock(db_path, || {
        panic!(
            "lock_emfile_probe: acquired the lock despite RLIMIT_NOFILE=3; EMFILE should have \
             made open() fail before ever reaching this closure"
        );
    });

    panic!("lock_emfile_probe: probe_hold_init_lock returned normally; expected a panic on EMFILE");
}

#[cfg(not(unix))]
fn main() {
    eprintln!("lock_emfile_probe is Unix-only (EMFILE via RLIMIT_NOFILE doesn't apply elsewhere)");
    std::process::exit(1);
}
