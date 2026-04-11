//! Out-of-harness tests for skuld's FD capture CLI behavior.
//!
//! These tests spawn the `capture_fixture` binary as a subprocess and
//! assert on its captured stdout/stderr. They cover behaviors that
//! cannot be verified from within a single skuld run:
//!
//!   * `[skuld] <name>: pass` lines on passing tests do not include
//!     any of the test's own output (it was captured and discarded).
//!   * `[skuld] <name>: fail` dumps include the test's stderr/stdout
//!     inside the `---- captured ----` section.
//!   * `--nocapture` disables the capture and lets all test output
//!     flow through unredirected.
//!   * `SKULD_DEBUG=1` emits `[skuld-debug] ...` lines.
//!   * Subprocess output written by a test's spawned child process is
//!     inherited through the capture pipe.
//!
//! Uses the standard libtest harness (not skuld itself) because skuld's
//! own machinery is the code under test. Harness choice is set in
//! `Cargo.toml`'s `[[test]] name = "capture_cli"` entry.

use std::process::{Command, Output};

/// Path to the compiled `capture_fixture` binary. Cargo sets this env
/// var automatically for the test process when the binary has a
/// `[[bin]]` entry in `Cargo.toml`.
fn fixture_bin() -> &'static str {
    env!("CARGO_BIN_EXE_capture_fixture")
}

/// Spawn the fixture binary with the given args + environment,
/// returning its captured output for inspection.
fn run_fixture(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(fixture_bin());
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    // Propagate a minimal PATH so the fixture's `sh` / `cmd` spawns
    // work on CI (bare Windows runners sometimes strip PATH).
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }
    cmd.output().expect("spawn capture_fixture")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// T1 — Default (capture mode) on passing test ========================================================
//
// `cargo test` default: no --nocapture. Skuld forces test_threads=1
// and installs an FdCapture around each test. The test's own stdout
// and stderr are consumed into the capture buffer; on pass, the
// buffer is discarded.

#[test]
fn t1_default_passing_test_suppresses_output() {
    let out = run_fixture(&["passing_with_noise", "--exact"], &[]);
    assert!(out.status.success(), "fixture should have exited successfully: {out:?}");
    let err = stderr(&out);
    assert!(
        err.contains("[skuld] passing_with_noise: pass"),
        "expected skuld pass line. stderr:\n{err}"
    );
    assert!(
        !err.contains("fixture-stderr-pass"),
        "capture-mode pass should suppress the test's own stderr. stderr:\n{err}"
    );
    let combined = format!("{}{}", err, stdout(&out));
    assert!(
        !combined.contains("fixture-stdout-pass"),
        "capture-mode pass should suppress the test's own stdout. out:\n{combined}"
    );
}

// T2 — Default (capture mode) on failing test ========================================================

#[test]
fn t2_default_failing_test_dumps_captured_output() {
    let out = run_fixture(&["failing_with_diagnostic", "--exact"], &[]);
    assert!(!out.status.success(), "fixture should have exited non-zero: {out:?}");
    let err = stderr(&out);
    assert!(
        err.contains("[skuld] failing_with_diagnostic: fail"),
        "expected skuld fail line. stderr:\n{err}"
    );
    assert!(
        err.contains("---- captured ----"),
        "expected capture dump header. stderr:\n{err}"
    );
    assert!(
        err.contains("fixture-diagnostic"),
        "captured diagnostic should appear in dump. stderr:\n{err}"
    );
    assert!(
        err.contains("---- end capture ----"),
        "expected capture dump footer. stderr:\n{err}"
    );
}

// T3 — Default on failing test with raw io::stderr().write_all ========================================
//
// Proves FD-level capture catches direct writes that bypass the
// print!/eprint! macros — the main thing this design buys over the
// nightly `std::io::set_output_capture` approach.

#[test]
fn t3_default_failing_test_captures_raw_write() {
    let out = run_fixture(&["failing_with_raw_stderr_write", "--exact"], &[]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("fixture-raw-stderr"),
        "raw stderr write should be captured and dumped. stderr:\n{err}"
    );
}

// T4 — --nocapture on passing test =====================================================================
//
// With --nocapture the test body's stdout and stderr flow through
// unredirected, landing on the subprocess's real stderr/stdout
// streams (which we read via Command::output).

#[test]
fn t4_nocapture_passing_test_shows_output_live() {
    let out = run_fixture(&["passing_with_noise", "--exact", "--nocapture"], &[]);
    assert!(out.status.success(), "fixture should pass: {out:?}");
    let err = stderr(&out);
    let outstr = stdout(&out);
    assert!(
        err.contains("fixture-stderr-pass"),
        "nocapture mode should pass stderr through live. stderr:\n{err}"
    );
    assert!(
        outstr.contains("fixture-stdout-pass"),
        "nocapture mode should pass stdout through live. stdout:\n{outstr}"
    );
}

// T5 — SKULD_DEBUG=1 emits debug lines ================================================================

#[test]
fn t5_skuld_debug_env_emits_debug_lines() {
    let out = run_fixture(&["passing_with_noise", "--exact"], &[("SKULD_DEBUG", "1")]);
    assert!(out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("[skuld-debug] passing_with_noise: entering test scope"),
        "expected SKULD_DEBUG line. stderr:\n{err}"
    );
    assert!(
        err.contains("[skuld-debug] passing_with_noise: capture enabled (fd redirect)"),
        "expected capture-enabled SKULD_DEBUG line. stderr:\n{err}"
    );
}

// T6 — Subprocess output inherits capture (failing test) ==============================================
//
// The fixture test spawns a child process that writes sentinels to
// its own stdout/stderr, then panics. On Unix, the child inherits
// fd 1/2 from the parent (which we redirected via dup2), so its
// output flows into the pipe. On Windows, the child's STARTUPINFO
// is populated from GetStdHandle(STD_OUTPUT_HANDLE) at spawn time,
// and we pointed those at the pipe via SetStdHandle.

#[test]
fn t6_subprocess_output_inherits_capture() {
    let out = run_fixture(&["failing_spawns_child", "--exact"], &[]);
    assert!(!out.status.success(), "fixture should have exited non-zero: {out:?}");
    let err = stderr(&out);
    assert!(
        err.contains("---- captured ----"),
        "expected capture dump. stderr:\n{err}"
    );
    assert!(
        err.contains("fixture-child-stdout-fail"),
        "child stdout should be captured through inherited fd. stderr:\n{err}"
    );
    assert!(
        err.contains("fixture-child-stderr-fail"),
        "child stderr should be captured through inherited fd. stderr:\n{err}"
    );
}

// T7 — Subprocess output on a passing test is also suppressed =========================================

#[test]
fn t7_subprocess_output_on_pass_is_suppressed() {
    let out = run_fixture(&["passing_spawns_child", "--exact"], &[]);
    assert!(out.status.success(), "fixture should pass: {out:?}");
    let err = stderr(&out);
    let combined = format!("{}{}", err, stdout(&out));
    assert!(
        !combined.contains("fixture-child-stdout"),
        "passing child stdout should be suppressed. out:\n{combined}"
    );
    assert!(
        !combined.contains("fixture-child-stderr"),
        "passing child stderr should be suppressed. out:\n{combined}"
    );
}
