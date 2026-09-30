//! A panicking `runtime = ...` builder runs outside `should_panic`'s
//! `catch_unwind`, so it fails the test instead of satisfying `should_panic`.

use std::process::Command;

fn run_trial(name: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_runtime_builder_panic_probe"))
        .args([name, "--exact"])
        .env_remove("SKULD_LABELS")
        .env_remove("SKULD_NEXTEST_METADATA_PATH")
        .output()
        .expect("spawn runtime_builder_panic_probe")
}

#[track_caller]
fn assert_fails_with_builder_panic(name: &str) {
    let out = run_trial(name);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "{name} must fail; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("FAILED") && format!("{stdout}{stderr}").contains("builder exploded"),
        "{name} must fail with the builder's panic; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
fn builder_panic_fails_the_test() {
    assert_fails_with_builder_panic("builder_panics");
}

#[test]
fn builder_panic_is_not_a_satisfied_should_panic() {
    assert_fails_with_builder_panic("builder_panics_under_should_panic");
}

#[test]
fn builder_panic_is_not_a_satisfied_should_panic_message() {
    assert_fails_with_builder_panic("builder_panics_under_should_panic_message");
}
