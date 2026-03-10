//! Per-test temporary directory fixture, named after the current test.

use std::ops::Deref;
use std::path::Path;

/// A temporary directory. Created fresh per request (variable scope), removed
/// on drop. The directory name includes the test function name for debugging.
///
/// Implements `Deref<Target = Path>` so it can be used as `&Path` directly
/// via `#[fixture(temp_dir)] dir: &Path`.
pub struct TempDir {
    inner: tempfile::TempDir,
}

impl Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        self.inner.path()
    }
}

use crate::fixtures::test_name::test_name;

#[skuld::fixture(deref)]
pub fn temp_dir(#[fixture(test_name)] name: &str) -> Result<TempDir, String> {
    tempfile::Builder::new()
        .prefix(&format!("{name}-"))
        .tempdir()
        .map(|inner| TempDir { inner })
        .map_err(|e| format!("failed to create temp dir: {e}"))
}
