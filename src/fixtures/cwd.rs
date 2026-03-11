//! Current working directory fixture with automatic revert on drop.
//!
//! The `cwd` fixture provides a [`CwdGuard`] that changes the process working
//! directory and restores it when the test scope ends. Because the working
//! directory is process-global, this fixture is marked `serial`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A scoped working directory override. Maintains a stack of directories so
/// that [`back`](Self::back) can return to the previous one. The original
/// working directory is restored when the guard is dropped.
///
/// # Example
///
/// ```ignore
/// #[skuld::test]
/// fn my_test(#[fixture] cwd: &CwdGuard, #[fixture(temp_dir)] dir: &Path) {
///     cwd.set(dir);
///     assert_eq!(std::env::current_dir().unwrap(), dir);
///     cwd.back();  // back to the original directory
///     // Working directory is reverted when the test ends regardless.
/// }
/// ```
pub struct CwdGuard {
    /// Stack of directories: first entry is the original, subsequent entries
    /// are pushed by `set`. Uses Mutex for the Sync bound required by fixtures.
    stack: Mutex<Vec<PathBuf>>,
}

impl CwdGuard {
    pub(crate) fn new() -> Result<Self, String> {
        let original = std::env::current_dir().map_err(|e| format!("failed to get current dir: {e}"))?;
        Ok(Self {
            stack: Mutex::new(vec![original]),
        })
    }

    /// Change the working directory. The previous directory is pushed onto the
    /// stack and can be restored with [`back`](Self::back).
    pub fn set(&self, path: &Path) {
        let current = std::env::current_dir().expect("failed to get current dir");
        self.stack.lock().unwrap().push(current);
        std::env::set_current_dir(path)
            .unwrap_or_else(|e| panic!("failed to set current dir to {}: {e}", path.display()));
    }

    /// Return to the previous working directory (like `cd -`).
    ///
    /// # Panics
    ///
    /// If there is no previous directory (i.e. `set` was never called).
    pub fn back(&self) {
        let prev = self
            .stack
            .lock()
            .unwrap()
            .pop()
            .expect("CwdGuard::back called with no previous directory");
        std::env::set_current_dir(&prev)
            .unwrap_or_else(|e| panic!("failed to set current dir to {}: {e}", prev.display()));
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let stack = self.stack.get_mut().unwrap();
        // The first entry is always the original directory.
        if let Some(original) = stack.first() {
            let _ = std::env::set_current_dir(original);
        }
    }
}

#[skuld::fixture(scope = test, serial)]
pub fn cwd() -> Result<CwdGuard, String> {
    CwdGuard::new()
}
