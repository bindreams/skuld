//! Tests for the `env` fixture (EnvGuard).

use std::sync::atomic::{AtomicU32, Ordering};

use skuld::{env, EnvGuard};

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

// A prior value that is not valid UTF-8 must be restored byte-for-byte.
#[cfg(unix)]
mod non_utf8 {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStrExt;

    pub const SET_VAR: &str = "SKULD_ENV_TEST_NON_UTF8_SET";
    pub const REMOVE_VAR: &str = "SKULD_ENV_TEST_NON_UTF8_REMOVE";
    pub const BYTES: &[u8] = b"a\xFFb";
    pub static RAN: AtomicU32 = AtomicU32::new(0);

    #[skuld::test]
    fn env_restores_a_non_utf8_prior_value(#[fixture] env: &EnvGuard) {
        // SAFETY: the `env` fixture is serial, so no other test touches the environment.
        unsafe {
            std::env::set_var(SET_VAR, OsStr::from_bytes(BYTES));
            std::env::set_var(REMOVE_VAR, OsStr::from_bytes(BYTES));
        }
        env.set(SET_VAR, "utf8");
        env.remove(REMOVE_VAR);
        assert_eq!(std::env::var(SET_VAR).unwrap(), "utf8");
        assert!(std::env::var_os(REMOVE_VAR).is_none());
        RAN.fetch_add(1, Ordering::Relaxed);
    }

    pub fn assert_restored() {
        assert_eq!(RAN.load(Ordering::Relaxed), 1, "the non-UTF-8 env test should have run");
        for var in [SET_VAR, REMOVE_VAR] {
            let restored: Option<OsString> = std::env::var_os(var);
            assert_eq!(
                restored.as_deref().map(OsStr::as_bytes),
                Some(BYTES),
                "EnvGuard must restore a non-UTF-8 prior value of {var} byte-identically"
            );
            // SAFETY: the run is over; nothing else reads these variables.
            unsafe { std::env::remove_var(var) };
        }
    }
}

pub fn assert_all_ran_and_reverted() {
    #[cfg(unix)]
    non_utf8::assert_restored();
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
