//! Per-test temporary directory fixture, named after the current test.

use std::ops::Deref;
use std::path::{Path, PathBuf};

/// A temporary directory. Created fresh per request (variable scope), removed
/// on drop. The directory name includes the test function name for debugging.
///
/// Implements `Deref<Target = Path>` so it can be used as `&Path` directly
/// via `#[fixture(temp_dir)] dir: &Path`.
pub struct TempDir {
    /// Kept alive for cleanup on drop.
    _inner: tempfile::TempDir,
    /// Canonicalized path (resolves symlinks like macOS `/var` → `/private/var`).
    path: PathBuf,
}

impl Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

use crate::fixtures::test_name::test_name;

#[skuld::fixture(deref)]
pub fn temp_dir(#[fixture(test_name)] name: &str) -> Result<TempDir, String> {
    tempfile::Builder::new()
        .prefix(&format!("{name}-"))
        .tempdir()
        .map_err(|e| format!("failed to create temp dir: {e}"))
        .and_then(|inner| {
            let path = inner
                .path()
                .canonicalize()
                .map_err(|e| format!("failed to canonicalize temp dir: {e}"))?;
            Ok(TempDir { _inner: inner, path })
        })
}
