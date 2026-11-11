//! Subject of subprocess invocations in `tests/coordination_modes_cli.rs`. Not a real product
//! binary.
//!
//! Spawned as a genuine subprocess rather than run in-process — see that file's module doc for
//! why (`umask` is process-global).
//!
//! Reads `SKULD_REGISTER_PROBE_COORD_DIR` (required: the coordination directory to register in,
//! created if missing), `SKULD_REGISTER_PROBE_UMASK` (optional, Unix: an octal umask to set, via
//! `libc::umask`, before registering) and `SKULD_REGISTER_PROBE_BARRIER` (optional: if set to any
//! value, write one `b"R"` byte to stdout and block reading one byte from stdin before
//! registering — see the comment inside `concurrent_registrations_converge_on_one_open_directory`
//! for why this is a real synchronization point, not a sleep).
//!
//! After registering, always signals `b"C"` on stdout and blocks reading one more byte from stdin
//! before unregistering and exiting 0 — the driver must inspect the registration's files during
//! this window, not after the process exits, which deletes them. A failed registration panics,
//! which `main` lets propagate as a nonzero exit with the panic message on stderr (the driver
//! never gets to send the release byte in that case, but the read simply errors instead of
//! hanging — the pipe's write end closed with the process).

use std::io::{Read, Write};

fn main() {
    let coord_dir =
        std::env::var("SKULD_REGISTER_PROBE_COORD_DIR").expect("driver must set SKULD_REGISTER_PROBE_COORD_DIR");

    #[cfg(unix)]
    if let Ok(mask) = std::env::var("SKULD_REGISTER_PROBE_UMASK") {
        let mask = u32::from_str_radix(&mask, 8)
            .unwrap_or_else(|e| panic!("SKULD_REGISTER_PROBE_UMASK {mask:?} is not valid octal: {e}"));
        // Safety: umask() has no preconditions and this process has no other threads yet — the
        // env var lookups above don't spawn any, and nothing earlier in main() does either.
        unsafe {
            libc::umask(mask as libc::mode_t);
        }
    }

    if std::env::var_os("SKULD_REGISTER_PROBE_BARRIER").is_some() {
        // Signal readiness, then block for the driver's release: a real blocking-I/O handshake,
        // not a sleep, so the driver can hold every child at the same starting line.
        let mut out = std::io::stdout();
        out.write_all(b"R")
            .expect("register_probe: failed to signal ready to driver");
        out.flush().expect("register_probe: failed to flush ready signal");
        let mut release = [0u8; 1];
        std::io::stdin()
            .read_exact(&mut release)
            .expect("register_probe: failed to read driver's release signal");
    }

    let registration = skuld::__private::probe_register(std::path::Path::new(&coord_dir));

    let mut out = std::io::stdout();
    out.write_all(b"C")
        .expect("register_probe: failed to signal registered to driver");
    out.flush().expect("register_probe: failed to flush registered signal");
    let mut release = [0u8; 1];
    std::io::stdin()
        .read_exact(&mut release)
        .expect("register_probe: failed to read driver's release signal");

    drop(registration);
}
