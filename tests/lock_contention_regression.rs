//! Deterministic cross-process counterpart to `src/coordination/lock_tests.rs`'s in-process
//! `try_lock`/`WouldBlock` test: `lock::with_global_lock`'s exclusion really is exclusive across
//! genuinely separate OS processes, not just within one.
//!
//! Uses two support binaries, `lock_hold_probe` and `lock_try_probe`, driving
//! `skuld::__private::probe_hold_global_lock`/`probe_try_global_lock` — the same real lock target
//! and real `flock`/`LockFileEx` calls that registration uses in production, not a simulation.
//!
//! No sleeps anywhere: a stdout/stdin byte handshake makes every ordering
//! constraint explicit instead of timing-dependent. The holder only signals
//! ready *after* `with_global_lock` has actually acquired the OS-level lock
//! (acquisition happens before its held closure runs), so a `try_lock` raced
//! against that ready signal is guaranteed to observe genuine contention, not
//! a scheduling accident; and the holder's OS lock is guaranteed released by
//! the time `Child::wait_with_output` returns, since the OS releases
//! `flock`/`LockFileEx` locks when the holding process's file descriptors
//! close at process exit.

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

/// `src/coordination/lock.rs`'s whole design rests on the lock target's own identity being stable
/// while a holder has it open — this proves that end to end across genuinely separate processes,
/// by a different mechanism per platform: on Windows, an attempt to delete the lock file itself
/// must fail outright (`FILE_SHARE_DELETE` was never granted); on Unix, the lock target is the
/// coordination directory, so deleting a file inside it *succeeds* — the point is that doing so
/// must not touch the directory's identity, so a fresh `try_lock` against the same directory must
/// still report `WouldBlock` afterward. Wholesale replacement of the directory itself is a
/// different risk this test doesn't claim to cover; see `lock.rs`'s module doc.
#[test]
fn a_holder_cannot_have_its_lock_target_deleted_out_from_under_it() {
    let dir = skuld::TempDir::new().unwrap();
    let coord_dir = dir.path().join(".skuld");
    std::fs::create_dir(&coord_dir).unwrap();
    // The Unix half of this test needs a file in the directory to delete below.
    let stand_in = coord_dir.join("stand-in");
    std::fs::write(&stand_in, b"").expect("create a stand-in file");

    let mut holder = hold_probe(&coord_dir).spawn().expect("spawn lock_hold_probe");
    let mut ready = [0u8; 1];
    holder
        .stdout
        .as_mut()
        .expect("stdout was piped")
        .read_exact(&mut ready)
        .unwrap_or_else(|e| panic!("failed to read lock_hold_probe's ready signal: {e}"));
    assert_eq!(ready[0], b'R', "unexpected ready byte {:?}", ready[0] as char);

    #[cfg(unix)]
    {
        // The lock target on Unix is the directory itself, so deleting a file in it must
        // *succeed* here, and must not touch the directory the lock is bound to. The directory
        // ends up empty.
        std::fs::remove_file(&stand_in)
            .expect("deleting a file in the coordination directory must succeed — it is not what's locked");
    }

    #[cfg(windows)]
    {
        // `global.lock` is the lock target on Windows, opened without FILE_SHARE_DELETE —
        // deleting it while lock_hold_probe holds it open must fail outright.
        std::fs::remove_file(coord_dir.join("global.lock"))
            .expect_err("the global lock file must not be removable while a holder has it locked");
    }

    let out = try_probe(&coord_dir);
    assert!(
        out.status.success(),
        "lock_try_probe failed after the deletion attempt above: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.stdout,
        b"B",
        "a second process's try_lock must still report WouldBlock after the deletion attempt \
         above (which failed on Windows because the lock file itself is protected; which \
         succeeded on Unix because deleting a file doesn't touch the directory that's \
         actually locked); stderr:\n{}",
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
