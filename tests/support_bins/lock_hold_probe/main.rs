//! Subject of subprocess invocations in `tests/lock_contention_regression.rs`.
//! Not a real product binary.
//!
//! Reads `SKULD_LOCK_HOLD_PROBE_COORD_DIR` (required: the existing coordination directory whose
//! global mutex to hold). Calls `skuld::__private::probe_hold_global_lock`, which runs the real
//! `lock::with_global_lock` that registration uses, and inside it: signals `b"R"` on stdout (ready — the lock is genuinely
//! held at this point, not just "spawned"), then blocks reading one byte
//! from stdin before returning, releasing the lock only once the driver has
//! sent that release byte. Mirrors `register_probe`'s
//! `SKULD_REGISTER_PROBE_BARRIER` handshake in `tests/coordination_modes_cli.rs`.

use std::io::{Read, Write};

fn main() {
    let coord_dir =
        std::env::var("SKULD_LOCK_HOLD_PROBE_COORD_DIR").expect("driver must set SKULD_LOCK_HOLD_PROBE_COORD_DIR");
    let coord_dir = std::path::Path::new(&coord_dir);

    skuld::__private::probe_hold_global_lock(coord_dir, || {
        let mut out = std::io::stdout();
        out.write_all(b"R")
            .expect("lock_hold_probe: failed to signal ready to driver");
        out.flush().expect("lock_hold_probe: failed to flush ready signal");

        let mut release = [0u8; 1];
        std::io::stdin()
            .read_exact(&mut release)
            .expect("lock_hold_probe: failed to read driver's release signal");
    });
}
