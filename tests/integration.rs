//! Integration tests for skuld: verifies `#[skuld::test]` macro behavior.

#[path = "integration_support/mod.rs"]
mod support;

fn main() {
    let original_cwd = std::env::current_dir().expect("failed to get initial cwd");

    // `libtest_mimic::run` returns `Conclusion::empty()` — every count zero,
    // no test body ever called — for a `--list` invocation (nextest's own
    // test-discovery step calls every binary this way before running any of
    // them for real). The post-run assertions below check side effects
    // (`AtomicBool`s a body sets), not `Conclusion`'s counts, so they must be
    // skipped for exactly this case; a real run that (impossibly, given the
    // fixed test set below) filtered everything to zero would look
    // identical to list-only through `Conclusion` alone, so the signal has
    // to come from the same argv `run_tests` itself parses, not from the
    // `Conclusion` it returns.
    let list_only = <libtest_mimic::Arguments as clap::Parser>::parse_from(std::env::args()).list;

    let conclusion = skuld::TestRunner::new().run_tests();

    if list_only {
        conclusion.exit();
    }

    // Post-run assertions: verify test bodies and teardowns actually ran.
    support::async_tests::assert_all_ran();
    support::async_tests::assert_runtime_context_probe_ran();
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
