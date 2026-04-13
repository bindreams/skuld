//! Tests for `#[skuld::test(should_panic)]` behavior.

use std::sync::atomic::{AtomicBool, Ordering};

use skuld::fixtures::metadata::metadata;
use skuld::metadata::TestMetadata;

static BARE_RAN: AtomicBool = AtomicBool::new(false);
static SUBSTRING_RAN: AtomicBool = AtomicBool::new(false);

#[skuld::test(should_panic)]
fn bare_should_panic() {
    BARE_RAN.store(true, Ordering::Relaxed);
    panic!("this test is supposed to panic");
}

#[skuld::test(should_panic = "expected substring")]
fn should_panic_with_message() {
    SUBSTRING_RAN.store(true, Ordering::Relaxed);
    panic!("failure: expected substring found");
}

#[skuld::test(should_panic)]
fn metadata_reports_should_panic(#[fixture(metadata)] meta: &TestMetadata) {
    assert_eq!(meta.should_panic, "yes");
    panic!("verifying metadata then panicking");
}

#[skuld::test(should_panic = "msg")]
fn metadata_reports_should_panic_message(#[fixture(metadata)] meta: &TestMetadata) {
    assert_eq!(meta.should_panic, "yes: msg");
    panic!("msg");
}

// Outer #[should_panic] attribute tests ----------------------------------------------------------

static OUTER_SHOULD_PANIC_RAN: AtomicBool = AtomicBool::new(false);

#[skuld::test]
#[should_panic]
fn outer_should_panic() {
    OUTER_SHOULD_PANIC_RAN.store(true, Ordering::Relaxed);
    panic!("expected panic via outer attr");
}

static OUTER_SHOULD_PANIC_MSG_RAN: AtomicBool = AtomicBool::new(false);

#[skuld::test]
#[should_panic(expected = "outer expected")]
fn outer_should_panic_with_expected() {
    OUTER_SHOULD_PANIC_MSG_RAN.store(true, Ordering::Relaxed);
    panic!("failure: outer expected message");
}

static OUTER_SHOULD_PANIC_FIXTURE_RAN: AtomicBool = AtomicBool::new(false);

#[skuld::test]
#[should_panic]
fn outer_should_panic_with_fixture(#[fixture(metadata)] meta: &TestMetadata) {
    OUTER_SHOULD_PANIC_FIXTURE_RAN.store(true, Ordering::Relaxed);
    assert_eq!(meta.should_panic, "yes");
    panic!("fixture + outer should_panic");
}

pub fn assert_all_ran() {
    assert!(
        BARE_RAN.load(Ordering::Relaxed),
        "bare_should_panic should have executed"
    );
    assert!(
        SUBSTRING_RAN.load(Ordering::Relaxed),
        "should_panic_with_message should have executed"
    );
    assert!(
        OUTER_SHOULD_PANIC_RAN.load(Ordering::Relaxed),
        "outer_should_panic should have executed"
    );
    assert!(
        OUTER_SHOULD_PANIC_MSG_RAN.load(Ordering::Relaxed),
        "outer_should_panic_with_expected should have executed"
    );
    assert!(
        OUTER_SHOULD_PANIC_FIXTURE_RAN.load(Ordering::Relaxed),
        "outer_should_panic_with_fixture should have executed"
    );
}
