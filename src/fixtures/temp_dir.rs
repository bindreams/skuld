//! Per-test temporary directory fixture, named after the current test.

use std::io;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A temporary directory, removed (with its contents) on drop.
///
/// Implements `Deref<Target = Path>` so it can be used as `&Path` directly
/// via `#[fixture(temp_dir)] dir: &Path`.
pub struct TempDir {
    /// The path it was created at; removed on drop.
    created: PathBuf,
    /// The path handed out: `created`, or its canonical form for the fixture.
    path: PathBuf,
}

impl TempDir {
    /// A new, empty directory in [`std::env::temp_dir`].
    pub fn new() -> io::Result<Self> {
        Self::new_in(std::env::temp_dir())
    }

    /// A new, empty directory in `parent`.
    pub fn new_in(parent: impl AsRef<Path>) -> io::Result<Self> {
        Self::with_prefix_in(".tmp", parent.as_ref())
    }

    fn with_prefix_in(prefix: &str, parent: &Path) -> io::Result<Self> {
        let pid = std::process::id();
        let created = create_in(parent, || {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            format!("{prefix}-{pid}-{n}")
        })?;
        Ok(Self {
            path: created.clone(),
            created,
        })
    }

    /// The directory's path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Numbers this process's temporary directories, so no two share a name.
static NEXT: AtomicU64 = AtomicU64::new(1);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.created);
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

/// Create a directory in `parent`, named by the first name from `next_name` that is not taken.
pub(crate) fn create_in(parent: &Path, mut next_name: impl FnMut() -> String) -> io::Result<PathBuf> {
    loop {
        let path = parent.join(next_name());
        #[cfg(unix)]
        let builder = {
            let mut b = std::fs::DirBuilder::new();
            std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
            b
        };
        #[cfg(not(unix))]
        let builder = std::fs::DirBuilder::new();
        match builder.create(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io::Error::new(e.kind(), format!("{e} at path {path:?}"))),
        }
    }
}

use crate::fixtures::test_name::test_name;

/// A fresh temporary directory whose name starts with the current test's name. Its path is
/// canonical (symlinks such as macOS `/var` → `/private/var` resolved).
#[skuld::fixture(deref)]
pub fn temp_dir(#[fixture(test_name)] name: &str) -> Result<TempDir, String> {
    let mut dir =
        TempDir::with_prefix_in(name, &std::env::temp_dir()).map_err(|e| format!("failed to create temp dir: {e}"))?;
    dir.path = dir
        .created
        .canonicalize()
        .map_err(|e| format!("failed to canonicalize temp dir: {e}"))?;
    Ok(dir)
}
