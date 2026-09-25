//! Subject of subprocess invocations in `tests/wal_cold_start_race_regression.rs`.
//! Not a real product binary.
//!
//! Reads `SKULD_WAL_RACE_PROBE_DB` (required: the coordination DB path to
//! open and write through). Signals `b"R"` on stdout, then blocks reading
//! one byte from stdin before doing anything else: the driver holds every
//! probe at this barrier until all have signaled ready, then releases them
//! together, maximizing the chance they genuinely contend `open_db`'s WAL
//! cold-start negotiation instead of depending on process-launch scheduling
//! to overlap it — mirrors `publish_probe`'s `SKULD_PUBLISH_PROBE_BARRIER`
//! handshake in `tests/coordination_publish_cli.rs`, for the same reason.
//!
//! After release, calls `skuld::__private::probe_coordination_write`, which
//! panics loudly (exit nonzero, message on stderr) if the connection
//! `open_db` hands back can't actually be written to.

use std::io::{Read, Write};

fn main() {
    let db_path = std::env::var("SKULD_WAL_RACE_PROBE_DB").expect("driver must set SKULD_WAL_RACE_PROBE_DB");

    let mut out = std::io::stdout();
    out.write_all(b"R")
        .expect("wal_race_probe: failed to signal ready to driver");
    out.flush().expect("wal_race_probe: failed to flush ready signal");

    let mut release = [0u8; 1];
    std::io::stdin()
        .read_exact(&mut release)
        .expect("wal_race_probe: failed to read driver's release signal");

    skuld::__private::probe_coordination_write(std::path::Path::new(&db_path));
}
