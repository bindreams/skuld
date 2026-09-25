//! Subject of subprocess invocations in `tests/lock_contention_regression.rs`.
//! Not a real product binary.
//!
//! Reads `SKULD_LOCK_HOLD_PROBE_DB` (required: the coordination DB path
//! whose init lock to hold). Calls `skuld::__private::probe_hold_init_lock`,
//! which runs the real `lock::with_init_lock` that `connect`/`open_db` use,
//! and inside it: signals `b"R"` on stdout (ready — the lock is genuinely
//! held at this point, not just "spawned"), then blocks reading one byte
//! from stdin before returning, releasing the lock only once the driver has
//! sent that release byte. Mirrors `publish_probe`'s
//! `SKULD_PUBLISH_PROBE_BARRIER` handshake in `tests/coordination_publish_cli.rs`.

use std::io::{Read, Write};

fn main() {
    let db_path = std::env::var("SKULD_LOCK_HOLD_PROBE_DB").expect("driver must set SKULD_LOCK_HOLD_PROBE_DB");
    let db_path = std::path::Path::new(&db_path);

    skuld::__private::probe_hold_init_lock(db_path, || {
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
