//! Guards `SQLITE_OPEN_PRIVATE_CACHE`: with shared-cache mode enabled
//! process-wide, contention between Skuld's own connections must still report
//! `SQLITE_BUSY`, not `SQLITE_LOCKED` (see `is_retryable`). A subprocess,
//! because the toggle cannot be unset and would leak into every other test in
//! this process (the same hazard as `umask` in `tests/coordination_publish_cli.rs`).

use std::process::Command;

#[test]
fn shared_cache_mode_enabled_elsewhere_in_the_process_still_reports_busy_not_locked() {
    let dir = skuld::TempDir::new().unwrap();
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
