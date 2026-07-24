//! End-to-end tests for coordinate()'s first-genuine-wait debug
//! diagnostic, via `coordination_debug_fixture` (two globally-serial
//! dynamic tests — see that fixture's doc comment for why this guarantees
//! deterministic, non-racy contention).

use std::process::Command;

#[test]
fn first_wait_logs_a_debug_diagnostic() {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_coordination_debug_fixture"));
    cmd.args(["--nocapture", "--test-threads", "2"]);
    cmd.env("SKULD_DEBUG", "1");
    cmd.env_remove("SKULD_LABELS");
    let out = cmd.output().expect("spawn coordination_debug_fixture");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("coordination:") && stderr.contains("blocked on a serial constraint"),
        "expected the first-wait debug diagnostic in stderr; got:\n{stderr}"
    );
}

#[test]
fn no_diagnostic_without_skuld_debug() {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_coordination_debug_fixture"));
    cmd.args(["--nocapture", "--test-threads", "2"]);
    cmd.env_remove("SKULD_DEBUG");
    cmd.env_remove("SKULD_LABELS");
    let out = cmd.output().expect("spawn coordination_debug_fixture");
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("blocked on a serial constraint"),
        "diagnostic must be gated behind SKULD_DEBUG; got:\n{stderr}"
    );
}
