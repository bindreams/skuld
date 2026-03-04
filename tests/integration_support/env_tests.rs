//! Tests for the `env` fixture (EnvGuard).

use std::sync::atomic::{AtomicU32, Ordering};

use skuld::EnvGuard;

const SENTINEL_VAR: &str = "SKULD_ENV_TEST_SENTINEL";

static ENV_SET_RAN: AtomicU32 = AtomicU32::new(0);
static ENV_REMOVE_RAN: AtomicU32 = AtomicU32::new(0);

#[skuld::test]
fn env_set_is_visible(#[fixture] env: &EnvGuard) {
    env.set(SENTINEL_VAR, "hello");
    assert_eq!(
        std::env::var(SENTINEL_VAR).unwrap(),
        "hello",
        "env.set should make the variable visible"
    );
    ENV_SET_RAN.fetch_add(1, Ordering::Relaxed);
}

#[skuld::test]
fn env_remove_works(#[fixture] env: &EnvGuard) {
    env.set(SENTINEL_VAR, "to_be_removed");
    env.remove(SENTINEL_VAR);
    assert!(
        std::env::var(SENTINEL_VAR).is_err(),
        "env.remove should make the variable absent"
    );
    ENV_REMOVE_RAN.fetch_add(1, Ordering::Relaxed);
}

pub fn assert_all_ran_and_reverted() {
    assert_eq!(
        ENV_SET_RAN.load(Ordering::Relaxed),
        1,
        "env_set_is_visible should have run"
    );
    assert_eq!(
        ENV_REMOVE_RAN.load(Ordering::Relaxed),
        1,
        "env_remove_works should have run"
    );
    // Both tests modified SENTINEL_VAR, but after revert it should be absent.
    assert!(
        std::env::var(SENTINEL_VAR).is_err(),
        "EnvGuard should have reverted {SENTINEL_VAR} after each test"
    );
}
