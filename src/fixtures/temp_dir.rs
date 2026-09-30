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
/// On Windows that includes names held by entries that are deleted but still open (see
/// `crate::win_nt`), which Win32 would report as access denied.
pub(crate) fn create_in(parent: &Path, next_name: impl FnMut() -> String) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        let mut next_name = next_name;
        loop {
            let path = parent.join(next_name());
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(path),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io::Error::new(e.kind(), format!("{e} at path {path:?}"))),
            }
        }
    }
    #[cfg(windows)]
    {
        use crate::win_nt::{create_unique, nt_create, open_dir};
        use windows::Wdk::Storage::FileSystem::{FILE_CREATE, FILE_DIRECTORY_FILE};
        use windows::Win32::Storage::FileSystem::{
            FILE_FLAGS_AND_ATTRIBUTES, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        let handle = open_dir(parent)?;
        let (name, _created) = create_unique(parent, &handle, next_name, |dir, name| {
            nt_create(
                dir,
                name,
                FILE_READ_ATTRIBUTES,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_CREATE,
                FILE_DIRECTORY_FILE,
            )
        })?;
        Ok(parent.join(name))
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

#[cfg(test)]
mod adv_probe {
    //! THROWAWAY adversarial-review probes for #97. Every test prints `MEASURE` lines and passes.
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    fn show<T: std::fmt::Debug>(label: &str, f: impl FnOnce() -> T) {
        match catch_unwind(AssertUnwindSafe(f)) {
            Ok(v) => println!("MEASURE {label}: returned {v:?}"),
            Err(p) => {
                let msg = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()));
                println!("MEASURE {label}: PANICKED {msg:?}")
            }
        }
    }

    fn ls(dir: &Path) -> Vec<String> {
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .map(|r| r.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_else(|e| vec![format!("<read_dir: {e}>")]);
        v.sort();
        v
    }

    #[test]
    fn adv_prefix_shapes() {
        let root = TempDir::new().unwrap();
        for (i, prefix) in ["a/b", "a\\b", "a:b", "..", "x".repeat(40000).as_str()]
            .iter()
            .enumerate()
        {
            let parent = root.join(format!("p{i}"));
            std::fs::create_dir(&parent).unwrap();
            let shown = if prefix.len() > 20 { "x*40000".to_string() } else { prefix.to_string() };
            show(&format!("skuld with_prefix_in({shown:?})"), || {
                TempDir::with_prefix_in(prefix, &parent).map(|d| d.created.clone())
            });
            println!("MEASURE   listing after skuld: {:?}", ls(&parent));
            show(&format!("tempfile prefix({shown:?})"), || {
                tempfile::Builder::new()
                    .prefix(&format!("{prefix}-"))
                    .tempdir_in(&parent)
                    .map(|d| d.path().to_path_buf())
            });
            println!("MEASURE   listing after tempfile: {:?}", ls(&parent));
        }
    }

    #[test]
    fn adv_missing_or_bad_parent() {
        let root = TempDir::new().unwrap();
        std::fs::write(root.join("file"), b"").unwrap();
        for p in [
            root.join("nope"),
            root.join("nope").join("deeper"),
            root.join("file"),
        ] {
            show(&format!("new_in({p:?})"), || TempDir::new_in(&p).map(|d| d.created.clone()).map_err(|e| e.to_string()));
            show(&format!("tempfile tempdir_in({p:?})"), || tempfile::tempdir_in(&p).map(|d| d.path().to_path_buf()).map_err(|e| e.to_string()));
        }
    }

    #[test]
    fn adv_relative_parent_then_chdir() {
        let root = TempDir::new().unwrap();
        let a = root.join("a");
        let b = root.join("b");
        std::fs::create_dir_all(a.join("rel")).unwrap();
        std::fs::create_dir_all(b.join("rel")).unwrap();
        let old = std::env::current_dir().unwrap();
        std::env::set_current_dir(&a).unwrap();
        let d = TempDir::new_in("rel").unwrap();
        let t = tempfile::tempdir_in("rel").unwrap();
        println!("MEASURE skuld path={:?} abs={}", d.path(), d.path().is_absolute());
        println!("MEASURE tempfile path={:?} abs={}", t.path(), t.path().is_absolute());
        std::env::set_current_dir(&b).unwrap();
        println!("MEASURE after chdir, skuld path().exists()={}", d.path().exists());
        drop(d);
        drop(t);
        std::env::set_current_dir(&old).unwrap();
        println!("MEASURE a/rel after drops: {:?}", ls(&a.join("rel")));
    }

    #[cfg(unix)]
    #[test]
    fn adv_unix_mode_and_planted() {
        use std::os::unix::fs::{MetadataExt, symlink};
        let root = TempDir::new().unwrap();
        let d = TempDir::new_in(&root).unwrap();
        let t = tempfile::tempdir_in(&root).unwrap();
        println!(
            "MEASURE mode skuld={:o} tempfile={:o}",
            std::fs::metadata(d.path()).unwrap().mode() & 0o7777,
            std::fs::metadata(t.path()).unwrap().mode() & 0o7777
        );
        // Plant the next names this process will use: a symlink to a victim, then K dirs.
        let victim = root.join("victim");
        std::fs::create_dir(&victim).unwrap();
        let pid = std::process::id();
        let next = NEXT.load(Ordering::Relaxed);
        let parent = root.join("shared");
        std::fs::create_dir(&parent).unwrap();
        symlink(&victim, parent.join(format!(".tmp-{pid}-{next}"))).unwrap();
        let k = 1000u64;
        for n in next + 1..=next + k {
            std::fs::create_dir(parent.join(format!(".tmp-{pid}-{n}"))).unwrap();
        }
        let got = TempDir::new_in(&parent).unwrap();
        println!(
            "MEASURE planted symlink + {k} dirs; got {:?}; victim listing {:?}",
            got.path().file_name().unwrap(),
            ls(&victim)
        );
    }

    #[cfg(windows)]
    #[test]
    fn adv_windows_drop_with_held_children() {
        use crate::win_nt::test_support::mark_for_deletion;
        use std::os::windows::fs::OpenOptionsExt;
        let root = TempDir::new().unwrap();
        // (a) child file open by a foreign handle, default std share mode (READ|WRITE|DELETE)
        {
            let d = TempDir::new_in(&root).unwrap();
            let t = tempfile::tempdir_in(&root).unwrap();
            let fa = std::fs::File::create(d.join("f")).unwrap();
            let fb = std::fs::File::create(t.path().join("f")).unwrap();
            let (pd, pt) = (d.path().to_path_buf(), t.path().to_path_buf());
            drop(d);
            drop(t);
            println!("MEASURE (a) share-all child: skuld dir exists={} tempfile dir exists={}", pd.exists(), pt.exists());
            drop((fa, fb));
            println!("MEASURE (a) after close: skuld={} tempfile={}", pd.exists(), pt.exists());
        }
        // (b) child file open with share_mode(0)
        {
            let d = TempDir::new_in(&root).unwrap();
            let t = tempfile::tempdir_in(&root).unwrap();
            std::fs::write(d.join("f"), b"").unwrap();
            std::fs::write(t.path().join("f"), b"").unwrap();
            let fa = std::fs::OpenOptions::new().read(true).share_mode(0).open(d.join("f")).unwrap();
            let fb = std::fs::OpenOptions::new().read(true).share_mode(0).open(t.path().join("f")).unwrap();
            let (pd, pt) = (d.path().to_path_buf(), t.path().to_path_buf());
            drop(d);
            drop(t);
            println!("MEASURE (b) share-none child: skuld dir exists={} tempfile dir exists={}", pd.exists(), pt.exists());
            drop((fa, fb));
            println!("MEASURE (b) after close: skuld={} tempfile={} (leaked: {:?} {:?})", pd.exists(), pt.exists(), ls(&pd), ls(&pt));
        }
        // (c) child directory held delete-pending (classic) by a foreign handle
        {
            let d = TempDir::new_in(&root).unwrap();
            let t = tempfile::tempdir_in(&root).unwrap();
            std::fs::create_dir(d.join("c")).unwrap();
            std::fs::create_dir(t.path().join("c")).unwrap();
            let ha = mark_for_deletion(&d.join("c"), false);
            let hb = mark_for_deletion(&t.path().join("c"), false);
            let (pd, pt) = (d.path().to_path_buf(), t.path().to_path_buf());
            drop(d);
            drop(t);
            println!("MEASURE (c) delete-pending child: skuld dir exists={} tempfile dir exists={}", pd.exists(), pt.exists());
            drop((ha, hb));
            println!("MEASURE (c) after close: skuld={} tempfile={} (listing {:?} {:?})", pd.exists(), pt.exists(), ls(&pd), ls(&pt));
        }
    }
}
