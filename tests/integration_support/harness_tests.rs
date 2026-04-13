//! Tests for the `#[skuld::test]` macro and the skuld harness.

use std::sync::atomic::{AtomicBool, Ordering};

fn always_ok() -> Result<(), String> {
    Ok(())
}

fn always_fail() -> Result<(), String> {
    Err("intentionally unavailable".into())
}

static SATISFIED_TEST_RAN: AtomicBool = AtomicBool::new(false);

#[skuld::test(requires = [always_ok])]
fn requires_satisfied_runs_body() {
    SATISFIED_TEST_RAN.store(true, Ordering::Relaxed);
}

/// Called after `run_all()` to verify the body actually executed.
pub fn assert_satisfied_test_ran() {
    assert!(
        SATISFIED_TEST_RAN.load(Ordering::Relaxed),
        "requires_satisfied_runs_body should have executed"
    );
}

#[skuld::test(requires = [always_fail])]
fn requires_unsatisfied_skips_body() {
    panic!("this body should never execute");
}

#[skuld::test(requires = [always_ok, always_fail])]
fn requires_partial_failure_skips_body() {
    panic!("this body should never execute when any requirement fails");
}

// Result return type tests ----------------------------------------------------------------------------

static SYNC_RESULT_OK_RAN: AtomicBool = AtomicBool::new(false);
static SYNC_RESULT_ERR_RAN: AtomicBool = AtomicBool::new(false);

#[skuld::test]
fn sync_result_ok() -> Result<(), String> {
    SYNC_RESULT_OK_RAN.store(true, Ordering::Relaxed);
    Ok(())
}

/// Returning Err from a sync test should fail via IntoTestResult.
#[skuld::test(should_panic = "test returned an error")]
fn sync_result_err_fails() -> Result<(), String> {
    SYNC_RESULT_ERR_RAN.store(true, Ordering::Relaxed);
    Err("intentional error".into())
}

pub fn assert_result_tests_ran() {
    assert!(
        SYNC_RESULT_OK_RAN.load(Ordering::Relaxed),
        "sync_result_ok should have executed"
    );
    assert!(
        SYNC_RESULT_ERR_RAN.load(Ordering::Relaxed),
        "sync_result_err_fails should have executed"
    );
}

// Outer #[ignore] attribute tests ----------------------------------------------------------------

static OUTER_IGNORE_BARE_RAN: AtomicBool = AtomicBool::new(false);

#[skuld::test]
#[ignore]
fn outer_ignore_bare() {
    OUTER_IGNORE_BARE_RAN.store(true, Ordering::Relaxed);
}

static OUTER_IGNORE_REASON_RAN: AtomicBool = AtomicBool::new(false);

#[skuld::test]
#[ignore = "not yet implemented"]
fn outer_ignore_with_reason() {
    OUTER_IGNORE_REASON_RAN.store(true, Ordering::Relaxed);
}

pub fn assert_outer_ignore_tests_did_not_run() {
    assert!(
        !OUTER_IGNORE_BARE_RAN.load(Ordering::Relaxed),
        "outer_ignore_bare should NOT have run"
    );
    assert!(
        !OUTER_IGNORE_REASON_RAN.load(Ordering::Relaxed),
        "outer_ignore_with_reason should NOT have run"
    );
}
