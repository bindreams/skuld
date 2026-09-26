//! Subject of subprocess invocations in `tests/coordination_publish_cli.rs`.
//! Not a real product binary.
//!
//! Spawned as a genuine subprocess rather than run in-process — see
//! `tests/coordination_publish_cli.rs`'s module doc for why (`umask` is
//! process-global).
//!
//! Reads `SKULD_PUBLISH_PROBE_DB` (required: the coordination DB path to
//! connect to), `SKULD_PUBLISH_PROBE_UMASK` (optional: an octal umask to
//! set, via `libc::umask`, before connecting), and
//! `SKULD_PUBLISH_PROBE_BARRIER` (optional: if set to any value, write one
//! `b"R"` byte to stdout and block reading one byte from stdin before
//! connecting — see the comment inside
//! `concurrent_publishers_all_converge_on_one_0666_file` for why this is a
//! real synchronization point, not a sleep).
//!
//! After connecting, always signals `b"C"` on stdout and blocks reading one
//! more byte from stdin before dropping the connection and exiting 0 — the
//! driver must stat the companion files during this window, not after the
//! process exits: SQLite deletes `-wal`/`-shm` when the connection holding
//! them closes, and this process's connection is the only one open once it
//! exits. A failed publish panics, which `main` lets propagate as a nonzero
//! exit with the panic message on stderr (the driver never gets to send the
//! release byte in that case, but the read simply errors instead of hanging
//! — the pipe's write end closed with the process).

// The atomic-publish step this binary probes is Unix-only;
// `skuld::__private::probe_coordination_connect` doesn't exist on other
// platforms. Keep this binary buildable everywhere so `cargo build
// --workspace` never breaks on Windows, but only the Unix half does
// anything — the driver test file gates its subprocess calls to
// `#[cfg(unix)]` too, so the fallback below is never exercised.
#[cfg(unix)]
fn main() {
    let db_path = std::env::var("SKULD_PUBLISH_PROBE_DB").expect("driver must set SKULD_PUBLISH_PROBE_DB");

    if let Ok(mask) = std::env::var("SKULD_PUBLISH_PROBE_UMASK") {
        let mask = u32::from_str_radix(&mask, 8)
            .unwrap_or_else(|e| panic!("SKULD_PUBLISH_PROBE_UMASK {mask:?} is not valid octal: {e}"));
        // Safety: umask() has no preconditions and this process has no
        // other threads yet — the env var lookups above don't spawn any,
        // and nothing earlier in main() does either.
        unsafe {
            libc::umask(mask as libc::mode_t);
        }
    }

    use std::io::{Read, Write};

    if std::env::var_os("SKULD_PUBLISH_PROBE_BARRIER").is_some() {
        // Signal readiness, then block for the driver's release: a real
        // blocking-I/O handshake, not a sleep, so the driver can hold every
        // publisher at the same starting line, maximising the chance that
        // `probe_coordination_connect`'s publish race is genuinely
        // contended instead of relying on process-launch scheduling to
        // overlap it. Releasing all children together does not *guarantee*
        // the EEXIST path is hit on any given run — the driver still
        // releases them one write() at a time, and `connect_with` tries the
        // open before ever publishing, so a child whose open lands after the
        // winner's rename has already landed just opens the now-published
        // file directly and never calls `ensure_published` at all; only a
        // child whose open lands during the initial absence window attempts
        // its own publish, and if it loses the race that rename fails
        // `EEXIST` against the winner's file and no-ops before it loops back
        // and reopens successfully — so this test's assertions don't depend
        // on which timing is hit; the deterministic EEXIST case is
        // `a_lost_publish_race_uses_the_winners_file` in
        // `src/coordination/publish_tests.rs`.
        let mut out = std::io::stdout();
        out.write_all(b"R")
            .expect("publish_probe: failed to signal ready to driver");
        out.flush().expect("publish_probe: failed to flush ready signal");
        let mut release = [0u8; 1];
        std::io::stdin()
            .read_exact(&mut release)
            .expect("publish_probe: failed to read driver's release signal");
    }

    let conn = skuld::__private::probe_coordination_connect(std::path::Path::new(&db_path));

    // Hold the connection open and tell the driver so: it must inspect the
    // `-wal`/`-shm` companions before releasing us, not after we exit (see
    // the module doc above).
    let mut out = std::io::stdout();
    out.write_all(b"C")
        .expect("publish_probe: failed to signal connected to driver");
    out.flush().expect("publish_probe: failed to flush connected signal");
    let mut release = [0u8; 1];
    std::io::stdin()
        .read_exact(&mut release)
        .expect("publish_probe: failed to read driver's release signal");

    drop(conn);
}

#[cfg(not(unix))]
fn main() {
    eprintln!("publish_probe is Unix-only (the atomic-publish step it probes doesn't run elsewhere)");
    std::process::exit(1);
}
