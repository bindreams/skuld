//! Regression guard: with zero file descriptors left to spend (the
//! probe's `RLIMIT_NOFILE` soft limit is lowered to exactly the 3 already
//! open — stdin/stdout/stderr — leaving none free), opening the coordination
//! DB's lock target fails `EMFILE`; `open_lock_target` must turn that into
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
//! `tests/coordination_publish_cli.rs`'s module doc documents for `umask`.
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
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join(".skuld.db");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lock_emfile_probe"));
    cmd.env("SKULD_LOCK_EMFILE_PROBE_DB", &db_path);
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
        stderr.contains("failed to open coordination DB"),
        "expected lock_emfile_probe to panic from open_lock_target's own panic message, got \
         stderr:\n{stderr}"
    );
}
