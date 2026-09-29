//! Subject of subprocess invocations in `tests/drop_panic_during_unwind_cli.rs`.
//! Not a real product binary.
//!
//! Reproduces a panic-during-unwind hazard in `TestRegistration::drop`
//! (whose cleanup can itself panic on a genuine, non-retryable DB error). A
//! second, uncaught panic while the thread is already unwinding from a
//! first one aborts the whole process (`SIGABRT`), not just the one failing
//! test — this must run as a genuine subprocess so that abort, if it
//! happens, doesn't take the driver test binary down with it.
//!
//! Portable: the hazard lives in `Drop`'s `catch_unwind`/`thread::panicking()`
//! logic, which is not platform-gated; the corruption method is documented on
//! `probe_drop_panic_during_unwind`.
//!
//! Reads `SKULD_DROP_PANIC_PROBE_DB` (required: an isolated coordination DB
//! path — never the real shared workspace `.skuld.db`). Exits via a single
//! propagated panic (code 101, no signal) once `TestRegistration::drop` is
//! fixed to never let its own panic escape an active unwind; exits via
//! `SIGABRT` before that fix.
fn main() {
    let db_path = std::env::var("SKULD_DROP_PANIC_PROBE_DB").expect("driver must set SKULD_DROP_PANIC_PROBE_DB");

    skuld::__private::probe_drop_panic_during_unwind(std::path::Path::new(&db_path));
}
