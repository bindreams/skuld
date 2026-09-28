//! Subject of subprocess invocations in `tests/shared_cache_busy_not_locked_cli.rs`.
//! Not a real product binary.
//!
//! Reads `SKULD_SHARED_CACHE_PROBE_DB` (required: an isolated coordination
//! DB path — never the real shared workspace `.skuld.db`). Calls
//! `sqlite3_enable_shared_cache(1)` — process-global, with no un-set, hence
//! the genuine subprocess — then opens two of Skuld's own connections to
//! that path and proves contention between them still reports
//! `SQLITE_BUSY`, not `SQLITE_LOCKED`, via
//! `skuld::__private::probe_shared_cache_still_reports_busy`. Exits 0 (all
//! assertions inside the probe passed) or panics with a message on stderr
//! (exit 101) if either assertion failed.
fn main() {
    let db_path = std::env::var("SKULD_SHARED_CACHE_PROBE_DB").expect("driver must set SKULD_SHARED_CACHE_PROBE_DB");

    skuld::__private::probe_shared_cache_still_reports_busy(std::path::Path::new(&db_path));
}
