//! Subject of subprocess invocations in `tests/lock_contention_regression.rs`.
//! Not a real product binary.
//!
//! Reads `SKULD_LOCK_TRY_PROBE_COORD_DIR` (required: the existing coordination directory whose
//! global mutex to try-lock). Opens a fresh handle on its lock target (through
//! `skuld::__private::probe_try_global_lock`, never one any other probe or a registration holds)
//! and attempts a non-blocking `try_lock`, then reports the outcome on stdout: `b"B"` for
//! `Err(WouldBlock)`, `b"K"` for `Ok(())`. Any other error panics (exit
//! nonzero, message on stderr) — this probe exists to distinguish those two
//! specific outcomes, not to survive an unrelated I/O failure quietly.

use std::io::Write;

fn main() {
    let coord_dir =
        std::env::var("SKULD_LOCK_TRY_PROBE_COORD_DIR").expect("driver must set SKULD_LOCK_TRY_PROBE_COORD_DIR");
    let coord_dir = std::path::Path::new(&coord_dir);

    let result = skuld::__private::probe_try_global_lock(coord_dir);

    let mut out = std::io::stdout();
    match result {
        Ok(()) => out.write_all(b"K"),
        Err(std::fs::TryLockError::WouldBlock) => out.write_all(b"B"),
        Err(std::fs::TryLockError::Error(e)) => panic!("lock_try_probe: try_lock failed: {e}"),
    }
    .expect("lock_try_probe: failed to write result byte");
    out.flush().expect("lock_try_probe: failed to flush result byte");
}
