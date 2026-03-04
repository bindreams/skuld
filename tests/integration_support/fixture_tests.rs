//! Tests for the fixture system.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

// Fixtures ========================================================================================

// Variable-scoped counter (default scope). Tracks setup/drop calls.

static COUNTER_SETUP_COUNT: AtomicU32 = AtomicU32::new(0);
static COUNTER_DROP_CALLED: AtomicBool = AtomicBool::new(false);

pub struct Counter {
    pub value: u32,
}

impl Drop for Counter {
    fn drop(&mut self) {
        COUNTER_DROP_CALLED.store(true, Ordering::Relaxed);
    }
}

#[skuld::fixture]
fn c1() -> Result<Counter, String> {
    let n = COUNTER_SETUP_COUNT.fetch_add(1, Ordering::Relaxed);
    Ok(Counter { value: n + 1 })
}

#[skuld::fixture]
fn c2() -> Result<Counter, String> {
    let n = COUNTER_SETUP_COUNT.fetch_add(1, Ordering::Relaxed);
    Ok(Counter { value: n + 1 })
}

// A fixture with a requirement that always fails.

fn unavailable_dep() -> Result<(), String> {
    Err("dep not available".into())
}

pub struct UnavailableFixture;

#[skuld::fixture(requires = [unavailable_dep])]
fn unavailable_fixture() -> Result<UnavailableFixture, String> {
    panic!("setup should never be called for unavailable fixture");
}

// Variable scope tests --------------------------------------------------------------------------------

#[skuld::test]
fn variable_fixture_fresh_each_request(#[fixture] c1: &Counter, #[fixture] c2: &Counter) {
    // c1 and c2 are different fixtures (different names), each variable-scoped,
    // so they get different instances.
    assert_ne!(
        c1.value, c2.value,
        "different fixtures should produce different instances"
    );
}

// Requirement propagation tests -------------------------------------------------------------------

/// Fixture requirements propagate: a test using `UnavailableFixture` should be
/// skipped (not panicked) even without listing `unavailable_dep` in `#[requires]`.
#[skuld::test]
fn fixture_requirement_propagation(#[fixture(unavailable_fixture)] _f: &UnavailableFixture) {
    panic!("should never run — UnavailableFixture has an unmet requirement");
}

// TestName fixture ------------------------------------------------------------------------------------

#[skuld::test]
fn test_name_returns_function_name(#[fixture(test_name)] name: &skuld::TestName) {
    assert_eq!(
        &**name, "test_name_returns_function_name",
        "TestName should return the test function name"
    );
}

#[skuld::test]
fn test_name_deref_to_str(#[fixture(test_name)] name: &str) {
    assert_eq!(
        name, "test_name_deref_to_str",
        "TestName with deref coercion should work as &str"
    );
}

// TempDir fixture -------------------------------------------------------------------------------------

#[skuld::test]
fn temp_dir_exists(#[fixture(temp_dir)] dir: &Path) {
    assert!(dir.exists(), "temp dir should exist");
    assert!(dir.is_dir(), "temp dir should be a directory");
}

#[skuld::test]
fn temp_dir_contains_test_name(#[fixture(temp_dir)] dir: &Path) {
    let dir_name = dir.file_name().unwrap().to_string_lossy();
    assert!(
        dir_name.starts_with("temp_dir_contains_test_name-"),
        "temp dir name should contain the test name, got: {dir_name}"
    );
}

#[skuld::test]
fn temp_dir_is_unique(#[fixture(temp_dir)] d1: &Path, #[fixture(temp_dir)] d2: &Path) {
    // TempDir is variable-scoped, so two requests with the same name still
    // produce fresh instances (each #[fixture] param is a separate request).
    assert_ne!(
        d1, d2,
        "variable-scoped fixture should create fresh instances per request"
    );
}

// Post-run assertion ----------------------------------------------------------------------------------

pub fn assert_fixture_drop_called() {
    assert!(
        COUNTER_DROP_CALLED.load(Ordering::Relaxed),
        "Counter::drop should have been called during tests"
    );
}
