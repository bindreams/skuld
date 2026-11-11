//! Regression guard: with zero file descriptors left to spend (the
//! probe's `RLIMIT_NOFILE` soft limit is lowered to exactly the 3 already
//! open — stdin/stdout/stderr — leaving none free), opening the global mutex's
//! lock target fails `EMFILE`; `open_lock_target` must turn that into
//! an immediate panic, not an infinite spin. See
//! `tests/support_bins/lock_emfile_probe/main.rs`'s module doc for how the
//! subprocess reaches `EMFILE` deterministically, without looping.
//!
//! Unix-only: `RLIMIT_NOFILE`/`EMFILE` are Unix concepts. Runs the probe as
//! a genuine subprocess rather than lowering this test binary's own
//! `RLIMIT_NOFILE` in-process — `cargo test`'s default harness runs many
//! unit and integration tests concurrently, on multiple threads, inside one
//! process; starving that whole process of file descriptors would corrupt
//! every other test racing alongside this one, the same class of hazard
//! `tests/coordination_modes_cli.rs`'s module doc documents for `umask`.
//!
//! No timeout on the subprocess wait: if `open_lock_target` ever regresses
//! back to a retry loop, this test would hang instead of failing cleanly —
//! the same trade-off `tests/lock_contention_regression.rs`'s child-process
//! waits already make, and the fix under test here removes the loop
//! entirely rather than bounding it, so there's nothing left to time out.

#![cfg(unix)]

use std::process::Command;

#[test]
fn open_lock_target_panics_immediately_on_emfile_instead_of_spinning() {
    let dir = skuld::TempDir::new().unwrap();
    // Exists, so that the open below can only fail for want of a descriptor.
    let coord_dir = dir.path().join(".skuld");
    std::fs::create_dir(&coord_dir).unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lock_emfile_probe"));
    cmd.env("SKULD_LOCK_EMFILE_PROBE_COORD_DIR", &coord_dir);
    let output = cmd.output().unwrap_or_else(|e| panic!("spawn lock_emfile_probe: {e}"));

    assert!(
        !output.status.success(),
        "lock_emfile_probe must panic (nonzero exit) when the lock target's open fails EMFILE, \
         not succeed; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to open the coordination directory") && stderr.contains("os error 24"),
        "expected lock_emfile_probe to panic from open_lock_target's own panic message, with EMFILE, got \
         stderr:\n{stderr}"
    );
}
