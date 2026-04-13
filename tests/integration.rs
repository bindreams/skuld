//! Integration tests for skuld: verifies `#[skuld::test]` macro behavior.

#[path = "integration_support/mod.rs"]
mod support;

fn main() {
    let original_cwd = std::env::current_dir().expect("failed to get initial cwd");

    let conclusion = skuld::TestRunner::new().run_tests();

    // Post-run assertions: verify test bodies and teardowns actually ran.
    support::async_tests::assert_all_ran();
    support::capture_tests::assert_all_ran();
    support::harness_tests::assert_satisfied_test_ran();
    support::harness_tests::assert_result_tests_ran();
    support::fixture_tests::assert_fixture_drop_called();
    support::label_tests::assert_all_ran();
    support::serial_tests::assert_all_ran();
    support::env_tests::assert_all_ran_and_reverted();
    support::cwd_tests::assert_all_ran_and_reverted(&original_cwd);
    support::should_panic_tests::assert_all_ran();
    support::harness_tests::assert_outer_ignore_tests_did_not_run();
    support::async_tests::assert_outer_ignore_did_not_run();

    // Paranoia: if any capture-test regression made the run flaky, or
    // the newly-added tests' `should_panic` mechanism produced real
    // failures, surface that as a failing integration run.
    assert_eq!(
        conclusion.num_failed, 0,
        "integration run had {} failing test(s); capture redesign may be broken",
        conclusion.num_failed
    );

    conclusion.exit();
}
