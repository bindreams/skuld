//! End-to-end test pinning `__scope`'s drop timing for a satisfied
//! `should_panic` test: a Test-scoped fixture's `Drop` impl that checks
//! `std::thread::panicking()` must see `true`. Out-of-harness because the
//! read has to survive past the subprocess that took it — a subprocess is
//! also required so a would-be regression (a real double panic, if the
//! probe's own `Drop` ever panicked) can't take this driver down with it.

use std::process::Command;

#[test]
fn should_panic_satisfied_reports_panicking_during_scope_drop() {
    let out_file = tempfile::NamedTempFile::new().expect("create temp file for the probe's output");
    let out_path = out_file.path();

    let out = Command::new(env!("CARGO_BIN_EXE_panicking_at_drop_probe"))
        .env("SKULD_PANICKING_AT_DROP_PROBE_OUT", out_path)
        .output()
        .expect("spawn panicking_at_drop_probe");

    assert!(
        out.status.success(),
        "expected the should_panic test to be satisfied (process success); stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let recorded =
        std::fs::read_to_string(out_path).unwrap_or_else(|e| panic!("probe did not write its output file: {e}"));
    assert_eq!(
        recorded, "true",
        "the tracked fixture's Drop must run while the test's own panic is still unwinding \
         through __scope, so std::thread::panicking() must read true, not {recorded:?}"
    );
}
