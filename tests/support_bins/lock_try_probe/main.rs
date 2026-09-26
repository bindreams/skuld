//! Subject of subprocess invocations in `tests/lock_contention_regression.rs`.
//! Not a real product binary.
//!
//! Reads `SKULD_LOCK_TRY_PROBE_DB` (required: the coordination DB path whose
//! init lock file to try-lock). Opens a fresh handle on that lock file
//! (through `skuld::__private::probe_try_init_lock`, never one any other
//! probe or a real `connect`/`open_db` call holds) and attempts a
//! non-blocking `try_lock`, then reports the outcome on stdout: `b"B"` for
//! `Err(WouldBlock)`, `b"K"` for `Ok(())`. Any other error panics (exit
//! nonzero, message on stderr) — this probe exists to distinguish those two
//! specific outcomes, not to survive an unrelated I/O failure quietly.

use std::io::Write;

fn main() {
    let db_path = std::env::var("SKULD_LOCK_TRY_PROBE_DB").expect("driver must set SKULD_LOCK_TRY_PROBE_DB");
    let db_path = std::path::Path::new(&db_path);

    let result = skuld::__private::probe_try_init_lock(db_path);

    let mut out = std::io::stdout();
    match result {
        Ok(()) => out.write_all(b"K"),
        Err(std::fs::TryLockError::WouldBlock) => out.write_all(b"B"),
        Err(std::fs::TryLockError::Error(e)) => panic!("lock_try_probe: try_lock failed: {e}"),
    }
    .expect("lock_try_probe: failed to write result byte");
    out.flush().expect("lock_try_probe: failed to flush result byte");
}
