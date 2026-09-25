//! Deterministic cross-process counterpart to
//! `src/coordination/lock_tests.rs`'s in-process `try_lock`/`WouldBlock`
//! test, and the replacement for the removed `tests/wal_cold_start_race_regression.rs`:
//! that test tried to catch the WAL cold-start race `open_db`'s doc
//! describes as a *symptom* of missing exclusion, stress-testing up to 64
//! processes x 30 rounds without ever reproducing it, even against pre-lock
//! code — a test that never failed on the old code can't guard anything.
//! This test instead guards the *mechanism* the fix actually relies on
//! directly: that `lock::with_init_lock`'s exclusion really is exclusive
//! across genuinely separate OS processes, not just within one.
//!
//! Uses two support binaries, `lock_hold_probe` and `lock_try_probe`, driving
//! `skuld::__private::probe_hold_init_lock`/`probe_try_init_lock` — the same
//! real lock file, real `flock`/`LockFileEx` calls, and real `lock::lock_path`
//! naming that `connect`/`open_db` use in production, not a simulation of
//! them.
//!
//! No sleeps anywhere: a stdout/stdin byte handshake makes every ordering
//! constraint explicit instead of timing-dependent. The holder only signals
//! ready *after* `with_init_lock` has actually acquired the OS-level lock
//! (acquisition happens before its held closure runs), so a `try_lock` raced
//! against that ready signal is guaranteed to observe genuine contention, not
//! a scheduling accident; and the holder's OS lock is guaranteed released by
//! the time `Child::wait_with_output` returns, since the OS releases
//! `flock`/`LockFileEx` locks when the holding process's file descriptors
//! close at process exit.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

fn hold_probe(db_path: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lock_hold_probe"));
    cmd.env("SKULD_LOCK_HOLD_PROBE_DB", db_path);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd
}

fn try_probe(db_path: &std::path::Path) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lock_try_probe"));
    cmd.env("SKULD_LOCK_TRY_PROBE_DB", db_path);
    cmd.output().unwrap_or_else(|e| panic!("spawn lock_try_probe: {e}"))
}

#[test]
fn a_second_process_try_lock_reports_would_block_while_another_process_holds_the_lock_then_succeeds_after_release() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join(".skuld.db");

    let mut holder = hold_probe(&db_path).spawn().expect("spawn lock_hold_probe");

    // Wait for the holder to signal it is genuinely inside with_init_lock's
    // held closure (meaning the OS-level lock is already acquired, not just
    // "process spawned") before racing a try_lock against it.
    let mut ready = [0u8; 1];
    holder
        .stdout
        .as_mut()
        .expect("stdout was piped")
        .read_exact(&mut ready)
        .unwrap_or_else(|e| panic!("failed to read lock_hold_probe's ready signal: {e}"));
    assert_eq!(ready[0], b'R', "unexpected ready byte {:?}", ready[0] as char);

    let out = try_probe(&db_path);
    assert!(
        out.status.success(),
        "lock_try_probe failed while the holder was active: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.stdout,
        b"B",
        "a second process's try_lock must report WouldBlock while lock_hold_probe holds the \
         lock; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    holder
        .stdin
        .as_mut()
        .expect("stdin was piped")
        .write_all(b"G")
        .expect("release lock_hold_probe");
    let holder_out = holder.wait_with_output().expect("wait lock_hold_probe");
    assert!(
        holder_out.status.success(),
        "lock_hold_probe failed: {}",
        String::from_utf8_lossy(&holder_out.stderr)
    );

    let out = try_probe(&db_path);
    assert!(
        out.status.success(),
        "lock_try_probe failed after the holder released: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.stdout,
        b"K",
        "a second process's try_lock must succeed once lock_hold_probe has released the lock; \
         stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
