//! Subject of subprocess invocations in `tests/drop_panic_during_unwind_cli.rs`.
//! Not a real product binary.
//!
//! Reproduces a panic-during-unwind hazard in `TestRegistration::drop`
//! (whose cleanup can itself panic when the coordination directory moved). A
//! second, uncaught panic while the thread is already unwinding from a
//! first one aborts the whole process (`SIGABRT`), not just the one failing
//! test — this must run as a genuine subprocess so that abort, if it
//! happens, doesn't take the driver test binary down with it.
//!
//! Portable: the hazard lives in `Drop`'s `catch_unwind`/`thread::panicking()`
//! logic, which is not platform-gated; the corruption method is documented on
//! `probe_drop_panic_during_unwind`.
//!
//! Reads `SKULD_DROP_PANIC_PROBE_COORD_DIR` (required: an isolated coordination directory
//! path, never the real shared workspace `.skuld/`, whose parent is a directory link). Once
//! registered it writes one `b"R"` byte to stdout and blocks reading one byte from stdin: the
//! driver sabotages the registration in between, by retargeting that link. It then panics. Exits via a single
//! propagated panic (code 101, no signal) once `TestRegistration::drop` is
//! fixed to never let its own panic escape an active unwind; exits via
//! `SIGABRT` before that fix.
use std::io::{Read, Write};

fn main() {
    let coord_dir =
        std::env::var("SKULD_DROP_PANIC_PROBE_COORD_DIR").expect("driver must set SKULD_DROP_PANIC_PROBE_COORD_DIR");

    skuld::__private::probe_drop_panic_during_unwind(std::path::Path::new(&coord_dir), || {
        let mut out = std::io::stdout();
        out.write_all(b"R")
            .expect("drop_panic_during_unwind_probe: failed to signal registered to driver");
        out.flush()
            .expect("drop_panic_during_unwind_probe: failed to flush registered signal");
        let mut release = [0u8; 1];
        std::io::stdin()
            .read_exact(&mut release)
            .expect("drop_panic_during_unwind_probe: failed to read driver's release signal");
    });
}
