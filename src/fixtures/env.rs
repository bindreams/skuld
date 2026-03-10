//! Environment variable fixture with automatic revert on drop.
//!
//! The `env` fixture provides an [`EnvGuard`] that records all modifications
//! and restores the original values when the test scope ends. Because
//! environment variables are process-global, this fixture is marked `serial`
//! so that tests using it never run in parallel.

use std::sync::Mutex;

/// A scoped environment variable modifier. All changes made through this guard
/// are reverted in reverse order when the guard is dropped.
///
/// # Example
///
/// ```ignore
/// #[skuld::test]
/// fn my_test(#[fixture] env: &EnvGuard) {
///     env.set("MY_VAR", "value");
///     assert_eq!(std::env::var("MY_VAR").unwrap(), "value");
///     // MY_VAR is reverted when the test ends.
/// }
/// ```
pub struct EnvGuard {
    /// Stack of (key, original_value). `None` means the variable was not set.
    /// Uses Mutex for Sync bound (never actually contended — serial lock ensures exclusivity).
    original: Mutex<Vec<(String, Option<String>)>>,
}

impl EnvGuard {
    pub(crate) fn new() -> Self {
        Self {
            original: Mutex::new(Vec::new()),
        }
    }

    /// Set an environment variable. The original value (or absence) is recorded
    /// and will be restored when this guard drops.
    pub fn set(&self, key: &str, value: &str) {
        let old = std::env::var(key).ok();
        self.original.lock().unwrap().push((key.to_owned(), old));
        // SAFETY: we hold the serial lock, so no other test is touching env concurrently.
        unsafe { std::env::set_var(key, value) };
    }

    /// Remove an environment variable. The original value (or absence) is
    /// recorded and will be restored when this guard drops.
    pub fn remove(&self, key: &str) {
        let old = std::env::var(key).ok();
        self.original.lock().unwrap().push((key.to_owned(), old));
        // SAFETY: we hold the serial lock, so no other test is touching env concurrently.
        unsafe { std::env::remove_var(key) };
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        let entries = self.original.get_mut().unwrap();
        for (key, old) in entries.iter().rev() {
            match old {
                // SAFETY: we hold the serial lock, so no other test is touching env concurrently.
                Some(val) => unsafe { std::env::set_var(key, val) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

#[skuld::fixture(scope = test, serial)]
pub fn env() -> Result<EnvGuard, String> {
    Ok(EnvGuard::new())
}
