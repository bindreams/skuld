//! Tests for async `#[skuld::test]` support.

use std::sync::atomic::{AtomicBool, Ordering};

use skuld::test_name;

static BASIC_ASYNC_RAN: AtomicBool = AtomicBool::new(false);
static ASYNC_FIXTURE_RAN: AtomicBool = AtomicBool::new(false);
static ASYNC_SHOULD_PANIC_RAN: AtomicBool = AtomicBool::new(false);
static ASYNC_SHOULD_PANIC_MSG_RAN: AtomicBool = AtomicBool::new(false);
static ASYNC_RESULT_OK_RAN: AtomicBool = AtomicBool::new(false);
static ASYNC_RESULT_ERR_RAN: AtomicBool = AtomicBool::new(false);

#[skuld::test]
async fn basic_async_test() {
    BASIC_ASYNC_RAN.store(true, Ordering::Relaxed);
    tokio::task::yield_now().await; // exercises the runtime
}

#[skuld::test]
async fn async_with_fixture(#[fixture(test_name)] name: &str) {
    ASYNC_FIXTURE_RAN.store(true, Ordering::Relaxed);
    assert_eq!(name, "async_with_fixture");
    tokio::task::yield_now().await;
}

#[skuld::test(should_panic)]
async fn async_should_panic() {
    ASYNC_SHOULD_PANIC_RAN.store(true, Ordering::Relaxed);
    tokio::task::yield_now().await;
    panic!("expected async panic");
}

#[skuld::test(should_panic = "expected message")]
async fn async_should_panic_with_message() {
    ASYNC_SHOULD_PANIC_MSG_RAN.store(true, Ordering::Relaxed);
    tokio::task::yield_now().await;
    panic!("failure: expected message");
}

#[skuld::test]
async fn async_result_ok() -> Result<(), String> {
    ASYNC_RESULT_OK_RAN.store(true, Ordering::Relaxed);
    tokio::task::yield_now().await;
    Ok(())
}

/// Returning Err from an async test should fail via IntoTestResult.
#[skuld::test(should_panic = "test returned an error")]
async fn async_result_err_fails() -> Result<(), String> {
    ASYNC_RESULT_ERR_RAN.store(true, Ordering::Relaxed);
    tokio::task::yield_now().await;
    Err("something went wrong".into())
}

// Outer attribute tests --------------------------------------------------------------------------

static ASYNC_OUTER_IGNORE_RAN: AtomicBool = AtomicBool::new(false);

#[skuld::test]
#[ignore]
async fn async_outer_ignore() {
    ASYNC_OUTER_IGNORE_RAN.store(true, Ordering::Relaxed);
}

pub fn assert_outer_ignore_did_not_run() {
    assert!(
        !ASYNC_OUTER_IGNORE_RAN.load(Ordering::Relaxed),
        "async_outer_ignore should NOT have run"
    );
}

pub fn assert_all_ran() {
    assert!(
        BASIC_ASYNC_RAN.load(Ordering::Relaxed),
        "basic_async_test should have executed"
    );
    assert!(
        ASYNC_FIXTURE_RAN.load(Ordering::Relaxed),
        "async_with_fixture should have executed"
    );
    assert!(
        ASYNC_SHOULD_PANIC_RAN.load(Ordering::Relaxed),
        "async_should_panic should have executed"
    );
    assert!(
        ASYNC_SHOULD_PANIC_MSG_RAN.load(Ordering::Relaxed),
        "async_should_panic_with_message should have executed"
    );
    assert!(
        ASYNC_RESULT_OK_RAN.load(Ordering::Relaxed),
        "async_result_ok should have executed"
    );
    assert!(
        ASYNC_RESULT_ERR_RAN.load(Ordering::Relaxed),
        "async_result_err_fails should have executed"
    );
}
