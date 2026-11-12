//! Deterministic cross-process counterpart to `src/coordination/lock_tests.rs`'s in-process
//! `try_lock`/`WouldBlock` test: `lock::with_global_lock`'s exclusion really is exclusive across
//! genuinely separate OS processes, not just within one.
//!
//! Uses two support binaries, `lock_hold_probe` and `lock_try_probe`, driving
//! `skuld::__private::probe_hold_global_lock`/`probe_try_global_lock` — the same real lock target
//! and real `flock`/`LockFileEx` calls that registration uses in production, not a simulation.
//!
//! A stdout/stdin byte handshake orders the processes: the holder signals ready only after
//! `with_global_lock` has acquired the lock, so the `try_lock` observes real contention. The OS
//! releases the holder's lock when it exits, before `Child::wait_with_output` returns.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

fn hold_probe(coord_dir: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lock_hold_probe"));
    cmd.env("SKULD_LOCK_HOLD_PROBE_COORD_DIR", coord_dir);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd
}

fn try_probe(coord_dir: &std::path::Path) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lock_try_probe"));
    cmd.env("SKULD_LOCK_TRY_PROBE_COORD_DIR", coord_dir);
    cmd.output().unwrap_or_else(|e| panic!("spawn lock_try_probe: {e}"))
}

#[test]
fn a_second_process_try_lock_reports_would_block_while_another_process_holds_the_lock_then_succeeds_after_release() {
    let dir = skuld::TempDir::new().unwrap();
    let coord_dir = dir.path().join(".skuld");
    std::fs::create_dir(&coord_dir).unwrap();

    let mut holder = hold_probe(&coord_dir).spawn().expect("spawn lock_hold_probe");

    // Wait for the holder to signal it is genuinely inside with_global_lock's
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

    let out = try_probe(&coord_dir);
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

    let out = try_probe(&coord_dir);
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

/// The lock target's identity must stay stable while held. Windows: deleting the lock file must
/// fail (no `FILE_SHARE_DELETE`).
#[cfg(windows)]
#[test]
fn a_holder_cannot_have_its_lock_target_deleted_out_from_under_it() {
    let dir = skuld::TempDir::new().unwrap();
    let coord_dir = dir.path().join(".skuld");
    std::fs::create_dir(&coord_dir).unwrap();

    let mut holder = hold_probe(&coord_dir).spawn().expect("spawn lock_hold_probe");
    let mut ready = [0u8; 1];
    holder
        .stdout
        .as_mut()
        .expect("stdout was piped")
        .read_exact(&mut ready)
        .unwrap_or_else(|e| panic!("failed to read lock_hold_probe's ready signal: {e}"));
    assert_eq!(ready[0], b'R', "unexpected ready byte {:?}", ready[0] as char);

    // `global.lock` is opened without FILE_SHARE_DELETE.
    std::fs::remove_file(coord_dir.join("global.lock"))
        .expect_err("the global lock file must not be removable while a holder has it locked");

    let out = try_probe(&coord_dir);
    assert!(
        out.status.success(),
        "lock_try_probe failed after the deletion attempt above: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.stdout,
        b"B",
        "a second process's try_lock must still report WouldBlock after the failed deletion; stderr:\n{}",
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

    let out = try_probe(&coord_dir);
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
