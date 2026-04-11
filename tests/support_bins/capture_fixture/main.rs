//! Minimal skuld-powered binary used as the subject of subprocess
//! invocations in `tests/capture_cli.rs`. Not a real product binary.
//!
//! Contains a deliberate mix of passing and failing tests that produce
//! well-known sentinel strings on stdout and stderr. The parent test
//! (`capture_cli.rs`) invokes this binary with various CLI flags and
//! inspects the captured output to assert that skuld's per-test FD
//! capture behaves correctly:
//!
//!   * Default (no `--nocapture`): sentinels on passing tests are
//!     suppressed; sentinels on failing tests appear inside the
//!     `---- captured ----` dump.
//!   * `--nocapture`: all sentinels appear live on the subprocess's
//!     stderr stream regardless of pass/fail.
//!   * `SKULD_DEBUG=1`: `[skuld-debug] ...` lines appear around each
//!     test.

use std::io::Write as _;

#[skuld::test]
fn passing_with_noise() {
    println!("fixture-stdout-pass");
    eprintln!("fixture-stderr-pass");
}

#[skuld::test]
fn failing_with_diagnostic() {
    eprintln!("fixture-diagnostic");
    panic!("fixture-expected-failure");
}

#[skuld::test]
fn failing_with_raw_stderr_write() {
    std::io::stderr()
        .write_all(b"fixture-raw-stderr\n")
        .expect("write_all to stderr");
    panic!("fixture-expected-raw-failure");
}

#[skuld::test]
fn passing_spawns_child() {
    let status = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/C", "echo fixture-child-stdout & echo fixture-child-stderr 1>&2"])
            .status()
    } else {
        std::process::Command::new("sh")
            .args(["-c", "echo fixture-child-stdout; echo fixture-child-stderr >&2"])
            .status()
    };
    assert!(status.expect("child status").success());
}

#[skuld::test]
fn failing_spawns_child() {
    let status = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args([
                "/C",
                "echo fixture-child-stdout-fail & echo fixture-child-stderr-fail 1>&2",
            ])
            .status()
    } else {
        std::process::Command::new("sh")
            .args([
                "-c",
                "echo fixture-child-stdout-fail; echo fixture-child-stderr-fail >&2",
            ])
            .status()
    };
    assert!(status.expect("child status").success());
    panic!("fixture-expected-child-failure");
}

fn main() {
    skuld::run_all();
}
