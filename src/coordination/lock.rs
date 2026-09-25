//! Blocking cross-process advisory lock serializing creation, publication,
//! and schema initialization of the coordination database.
//!
//! Whoever holds a blocking, exclusive advisory lock (`flock` on Unix,
//! `LockFileEx` on Windows, via [`std::fs::File`]'s own native
//! `lock`/`unlock`, stable since Rust 1.89.0 — `fs4`/`fd-lock` expose the
//! identical primitive through the same method names, but an inherent
//! method always wins Rust's resolution over an identically-named trait
//! method, so depending on either crate here would only add a dead `use`
//! line) on the lock target below is the only actor in the whole system
//! allowed to create, publish, or initialize `.skuld.db` at that instant;
//! every other [`super::connect`]/[`super::open_db`] call blocks until it
//! releases. That removes the specific create/publish and
//! cold-start-negotiation races those two functions document, not all
//! waiting: `busy_timeout` and SQLite's own locking still apply to work
//! done *while* this lock is held.
//!
//! **The lock target itself can never be deleted or replaced out from under
//! a holder.** That is what lets [`with_init_lock`] be a single
//! open-then-lock with no retry loop and no check that the locked handle
//! still matches whatever is on disk: an open that succeeds is already the
//! lock every other caller will contend, permanently, for as long as this
//! handle stays open.
//!
//! - **Unix** locks `db_path`'s parent directory — the profile directory
//!   holding `.skuld.db` — opened `O_RDONLY | O_DIRECTORY | O_CLOEXEC` (the
//!   `CLOEXEC` bit is also `std`'s own default for every `File::open`; it's
//!   named explicitly here only because the module doc for this design
//!   calls it out, not because it adds anything `std` doesn't already do).
//!   `flock` works directly on a read-only directory fd, so there is no
//!   separate lock file, nothing here ever needs write access to anything,
//!   and no uid needs more than the ordinary read+search access it already
//!   needs to reach `.skuld.db` itself. The directory can't be `rmdir`'d
//!   while `.skuld.db` (or its `-wal`/`-shm` companions) still live inside
//!   it — `ENOTEMPTY` — and nothing in this crate ever removes the
//!   directory itself, only files inside it, so the locked directory's
//!   identity can't change out from under a holder.
//! - **Windows** locks a sibling [`lock_path`] file, opened with
//!   `share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)` and, deliberately, no
//!   `FILE_SHARE_DELETE`. Windows refuses to delete or rename a file out
//!   from under any handle that didn't grant that share flag, so the lock
//!   file can't be split out from under a holder either.
//!
//! Any failure to open the lock target — a missing parent directory,
//! file-descriptor-table exhaustion (`EMFILE`), a permissions problem,
//! anything at all — panics immediately, naming the path. None of those
//! conditions resolve themselves by trying the same open again, so there is
//! nothing to retry.

use std::fs::{File, OpenOptions};
use std::path::Path;

#[cfg(windows)]
use std::path::PathBuf;

/// The sibling lock file guarding `db_path`'s creation, publication, and
/// schema initialization on Windows: `db_path` with `.lock` appended
/// verbatim (not [`Path::with_extension`], which would replace
/// `.skuld.db`'s existing `db` extension instead of appending), so
/// `.skuld.db` gets `.skuld.db.lock`. Unix has no lock file — it locks
/// `db_path`'s parent directory directly instead, see the module doc — so
/// this only exists on Windows.
#[cfg(windows)]
pub(super) fn lock_path(db_path: &Path) -> PathBuf {
    let mut name = db_path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Run `f` while holding a blocking, exclusive advisory lock on `db_path`'s
/// lock target (see the module doc). Blocks with no timeout on the
/// `flock`/`LockFileEx` call itself.
///
/// Releasing the lock is not a separate step this function performs: the
/// held `File` going out of scope — on `f`'s normal return *and* on an
/// unwinding panic from `f`, since Rust always runs destructors during
/// unwind — closes the underlying fd/handle, and both `flock` and
/// `LockFileEx` release their lock unconditionally when the last handle to
/// it closes. A panic inside `f` (a genuinely broken DB path, for example)
/// therefore can never leave the lock held.
pub(super) fn with_init_lock<T>(db_path: &Path, f: impl FnOnce() -> T) -> T {
    let target = open_lock_target(db_path);
    target
        .lock()
        .unwrap_or_else(|e| panic!("skuld: failed to acquire coordination DB init lock for {db_path:?}: {e}"));
    f()
}

/// Open `db_path`'s lock target, ready to be `lock()`ed or `try_lock()`ed:
/// `db_path`'s parent directory on Unix, [`lock_path`]'s file on Windows
/// (see the module doc for why either one is safe to open once and never
/// recheck). A single open, never retried: any failure here is either
/// external interference (a missing parent directory) or resource
/// exhaustion (`EMFILE`), and neither resolves by trying again, so this
/// panics immediately instead of looping.
pub(super) fn open_lock_target(db_path: &Path) -> File {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // `db_path.parent()` is `Some("")` (not `None`) for a bare
        // relative filename with no directory component — fall back to
        // `.` the same way `super::publish::ensure_published_with`'s own
        // temp-file placement does, rather than treating that as an error
        // `db_path()`'s own callers never actually produce.
        let dir = db_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(dir)
            .unwrap_or_else(|e| {
                panic!("skuld: failed to open coordination DB profile directory {dir:?} for locking: {e}")
            })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

        let path = lock_path(db_path);
        OpenOptions::new()
            .create(true)
            .write(true)
            // Explicit, not the default: the lock file's contents are never
            // read or written by this module (only its identity as a
            // lockable object matters), so truncating it on open would just
            // be pointless I/O against a file every other concurrent caller
            // may also be opening right now.
            .truncate(false)
            .read(true)
            // Deliberately excludes FILE_SHARE_DELETE: see the module doc
            // for why that's what keeps this lock target from being
            // deleted or renamed out from under a holder.
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
            .open(&path)
            .unwrap_or_else(|e| panic!("skuld: failed to open coordination DB init lock file {path:?}: {e}"))
    }
}
