//! End-to-end test verifying a fixture setup failure fails a `should_panic`
//! test (FAILED), rather than satisfying it (ok). Out-of-harness because
//! the assertion is on libtest-mimic's process-level report.

use std::process::Command;

#[test]
fn fixture_setup_failure_fails_a_should_panic_test() {
    let out = Command::new(env!("CARGO_BIN_EXE_broken_fixture_should_panic"))
        .output()
        .expect("spawn broken_fixture_should_panic");

    assert!(
        !out.status.success(),
        "expected the binary to report a test failure, got success"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("FAILED"),
        "expected libtest-mimic to report FAILED; stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("uses_broken_fixture ... ok"),
        "fixture setup failure must not satisfy should_panic; stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("uses_broken_fixture"),
        "expected the failing trial name in the summary; stdout:\n{stdout}"
    );
}
