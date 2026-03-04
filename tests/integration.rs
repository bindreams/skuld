//! Integration tests for skuld: verifies `#[skuld::test]` macro behavior.

#[path = "integration_support/mod.rs"]
mod support;

fn main() {
    let conclusion = skuld::TestRunner::new().run_tests();

    // Post-run assertions: verify test bodies and teardowns actually ran.
    support::harness_tests::assert_satisfied_test_ran();
    support::fixture_tests::assert_fixture_drop_called();
    support::label_tests::assert_all_ran();

    conclusion.exit();
}
