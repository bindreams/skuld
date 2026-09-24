//! End-to-end subprocess test for the panic-during-unwind hazard in
//! `TestRegistration::drop`, via the `drop_panic_during_unwind_probe`
//! binary. `connect()` panics loudly when it can't open the coordination DB
//! (e.g. it's been replaced by a directory), and `Drop::drop` calls
//! `connect()` too — so a test that panics for an unrelated reason, while
//! its coordination DB has independently gone unusable, causes a *second*
//! panic during the first one's unwind. An uncaught panic during an active
//! unwind is Rust's "double panic": `std::process::abort()` (`SIGABRT` on
//! Unix), killing the whole test process rather than just failing the one
//! test. This needs a genuine subprocess: the abort, if it happens, must
//! not take this driver test binary down with it.
//!
//! Runs on every platform: the `Drop::drop` fix it exercises is not
//! platform-gated (Windows shares the hazard via `connect()`'s other panic
//! paths, even though it skips the Unix-only publish step), and Skuld's CI
//! has a Windows lane. The probe's corruption method (a directory in place
//! of the DB file) fails `connect()` via the same underlying
//! `rusqlite::Connection::open` rejection on both platforms (Unix:
//! `ensure_published` sees the path already exists and skips publishing, so
//! the failure surfaces from SQLite's own file open; Windows: skips the
//! Unix-only publish step entirely and hits the same open rejection) — only
//! the OS-level error text SQLite wraps differs, so this test does not
//! assert the exact panic wording — only that the process exits via a
//! single ordinary panic (not an abort), and that the downgraded warning
//! carries a real extracted message rather than the `panic_payload_message`
//! fallback placeholder.

use std::process::Command;

#[test]
fn a_second_panic_during_drop_does_not_abort_the_process() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join(".skuld.db");

    let out = Command::new(env!("CARGO_BIN_EXE_drop_panic_during_unwind_probe"))
        .env("SKULD_DROP_PANIC_PROBE_DB", &db_path)
        .output()
        .expect("spawn drop_panic_during_unwind_probe");

    let stderr = String::from_utf8_lossy(&out.stderr);

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            out.status.signal(),
            None,
            "the process must exit via its single propagated panic, not be killed by a signal \
             (a non-None signal here means Drop's own panic escaped an active unwind and Rust \
             aborted the whole process); stderr:\n{stderr}"
        );
    }
    assert_eq!(
        out.status.code(),
        Some(101),
        "a single propagated panic exits with libstd's standard code 101 on every platform; an \
         abort (SIGABRT on Unix, a runtime-abort exit code on Windows) would not produce 101; \
         stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("probe: artificial panic to trigger unwind"),
        "the original panic's message must reach stderr: {stderr}"
    );
    let warning_line = stderr
        .lines()
        .find(|l| l.contains("coordination DB cleanup panicked while already unwinding"))
        .unwrap_or_else(|| panic!("the downgraded second-panic warning must reach stderr: {stderr}"));
    // The default panic hook prints the second panic's raw message too
    // (before our `catch_unwind` even sees it), so checking `stderr` as a
    // whole for the fallback placeholder's absence would pass even if the
    // downgraded warning's own extracted message were wrong — check the
    // warning line itself. Not asserting the exact wording here (it's the
    // same `rusqlite::Connection::open` failure on both platforms, but the
    // OS-level error text SQLite wraps differs) — getting the message text
    // right is `panic_payload_message`'s job (unit-tested directly in
    // `coordination_tests.rs` against static-str, runtime-formatted-string,
    // and non-string payloads) and `downgraded_warning_message`'s (unit-
    // tested there against the exact `&payload` call convention
    // `TestRegistration::drop` uses).
    assert!(
        !warning_line.contains("<non-string panic payload>"),
        "the downgraded warning must carry the second panic's actual message, not the \
         non-string-payload fallback placeholder (regression guard for passing `&payload` \
         instead of `payload.as_ref()` to `panic_payload_message`, which silently downcasts \
         the wrong type and always falls back): {warning_line:?}"
    );
}
