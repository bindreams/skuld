//! Regression guard: `sqlite3_enable_shared_cache` is process-global, with
//! no un-set, and this process runs arbitrary user test code Skuld doesn't
//! control — so something else in the process turning shared-cache mode on
//! is a real scenario, not a hypothetical. `is_retryable`'s doc (and the
//! `debug_assert!`s in `retry_busy` and `coordinate` built on it) depend on
//! no connection Skuld opens ever joining shared-cache mode, specifically
//! so that a `SQLITE_LOCKED` reaching those assertions can only mean a
//! same-connection self-conflict — a bug, never a legitimate condition to
//! retry past. `SQLITE_OPEN_PRIVATE_CACHE` on every connection Skuld opens
//! is what keeps that true regardless of the process-wide toggle; this test
//! proves it end to end via `shared_cache_probe`, rather than only trusting
//! the flag is set.
//!
//! Runs the probe as a genuine subprocess: `sqlite3_enable_shared_cache` has
//! no way to be un-set, so enabling it in-process would leak into every
//! other test sharing this binary's process, the same class of hazard
//! `tests/coordination_publish_cli.rs`'s module doc documents for `umask`.

use std::process::Command;

#[test]
fn shared_cache_mode_enabled_elsewhere_in_the_process_still_reports_busy_not_locked() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join(".skuld.db");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_shared_cache_probe"));
    cmd.env("SKULD_SHARED_CACHE_PROBE_DB", &db_path);
    let output = cmd.output().unwrap_or_else(|e| panic!("spawn shared_cache_probe: {e}"));

    assert!(
        output.status.success(),
        "shared_cache_probe must succeed: contention between two of Skuld's own connections \
         must report SQLITE_BUSY, not SQLITE_LOCKED, even with shared-cache mode enabled \
         process-wide; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
