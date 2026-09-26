//! Blocking cross-process advisory lock serializing creation, publication,
//! and schema initialization of the coordination database.
//!
//! Whoever holds a blocking, exclusive advisory lock on the lock target
//! below is the only actor in the whole system allowed to create, publish,
//! or initialize `.skuld.db` at that instant; every other
//! [`super::connect`]/[`super::open_db`] call blocks until it releases. That
//! removes the specific create/publish and cold-start-negotiation races
//! those two functions document, not all waiting: `busy_timeout` and
//! SQLite's own locking still apply to work done *while* this lock is held.
//!
//! The lock itself is `flock` on Unix, `LockFileEx` on Windows — but the two
//! platforms reach it through different code. Windows goes through
//! [`std::fs::File`]'s own native `lock`/`try_lock` (stable since Rust
//! 1.89.0). Unix goes through [`rustix::fs::flock`] instead of that same
//! `std::fs::File` API: std only implements `lock`/`try_lock` on a subset of
//! the Unix targets it otherwise treats as `flock`-capable, and Android is
//! missing from that subset — calling `std`'s version there panics
//! `"lock() not supported"` on every `open_db`/`TestRegistration::drop`
//! instead of ever acquiring anything. `rustix::fs::flock` calls the same
//! underlying syscall directly on every Unix this crate supports, Android
//! included.
//!
//! **The lock target can't be deleted or replaced by anything short of
//! recreating the directory (Unix) or the file (Windows) it lives at.** That
//! is what lets [`with_init_lock`] be a single open-then-lock with no retry
//! loop and no check that the locked handle still matches whatever is on
//! disk: an open that succeeds is already the lock every other caller will
//! contend, permanently, for as long as this handle stays open — *unless*
//! something outside this crate replaces the lock target's directory entry
//! wholesale while that handle is open (see the Unix bullet below); this
//! crate itself never does that.
//!
//! - **Unix** locks `db_path`'s parent directory — the profile directory
//!   holding `.skuld.db` — opened `O_RDONLY | O_DIRECTORY | O_CLOEXEC` (the
//!   `CLOEXEC` bit is redundant here — `std` already sets it on every
//!   `File::open` regardless — and is listed only for parity with
//!   `O_DIRECTORY`, since both are passed through the same `custom_flags`
//!   call).
//!   `flock` works directly on a directory fd opened read-only, so there is
//!   no separate lock file and nothing here ever needs write access to
//!   anything; opening the directory at all still needs ordinary *read*
//!   permission on it, not just the search (execute) permission that's
//!   enough to merely reach `.skuld.db` inside it by name. An ordinary
//!   delete of `.skuld.db` (or its `-wal`/`-shm` companions) can't split the
//!   lock: the directory itself is untouched by deleting a file inside it,
//!   `ENOTEMPTY` still blocks a plain `rmdir` while any of them remain, and
//!   nothing in this crate ever removes the directory itself, only files
//!   inside it. **Wholesale replacement of the directory does still split
//!   it**, the same accepted risk class as deleting `.skuld.db` itself
//!   mid-run (see [`super::connect`]'s doc): renaming the directory aside
//!   and `mkdir`ing a fresh one at the same path, or emptying it,
//!   `rmdir`ing it, and `mkdir`ing it again, both leave the holder locking
//!   its old (now-detached) directory while every new opener locks the
//!   fresh one instead — this module verifies nothing about what's still at
//!   the path beyond a successful open, so nothing here would notice the
//!   swap to tell the two groups apart.
//! - **Windows** locks a sibling [`lock_path`] file, opened with
//!   `share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)` and, deliberately, no
//!   `FILE_SHARE_DELETE`. Windows refuses to delete or rename a file out
//!   from under any handle that didn't grant that share flag, so the lock
//!   file can't be split out from under a holder at all, not even by
//!   wholesale replacement.
//!
//! Any failure to open the lock target — a missing parent directory,
//! file-descriptor-table exhaustion (`EMFILE`), a permissions problem,
//! anything at all — panics immediately, naming the path. None of those
//! conditions resolve themselves by trying the same open again, so there is
//! nothing to retry. On Unix specifically, a filesystem whose `flock` refuses
//! to operate on a directory at all (some network filesystems' emulated
//! `flock`, NFS's included) surfaces the same way: the `open` above still
//! succeeds, but the subsequent acquire in [`with_init_lock`] fails and
//! panics, naming the path — there is no fallback to a different locking
//! mechanism.

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
    lock_exclusive(&target)
        .unwrap_or_else(|e| panic!("skuld: failed to acquire coordination DB init lock for {db_path:?}: {e}"));
    f()
}

/// Open `db_path`'s lock target, ready to be locked or try-locked (via
/// [`lock_exclusive`]/[`try_lock_exclusive`]): `db_path`'s parent directory
/// on Unix, [`lock_path`]'s file on Windows (see the module doc for why
/// either one is safe to open once and never recheck, and why a failed open
/// here panics immediately instead of retrying).
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

/// Block until `target`'s exclusive advisory lock is acquired. See the
/// module doc for why Unix goes through [`rustix::fs::flock`] here instead
/// of `std::fs::File::lock` — std's version isn't implemented on every Unix
/// target this crate supports.
///
/// Retries on `EINTR` alone, uncapped: a blocking `flock` is interruptible
/// by any signal delivered to this thread, including ones this crate has no
/// control over — a test process installs its own handlers for all sorts of
/// reasons — and a handler registered without `SA_RESTART` makes the kernel
/// hand back `EINTR` instead of resuming the wait. That is not a real
/// failure (the lock is neither held nor denied), so this loops back into
/// the same blocking call rather than surfacing it as one; std's own
/// `File::lock` makes the identical choice for the targets it supports.
/// Nothing else this function can receive from `flock` is retryable.
#[cfg(unix)]
pub(super) fn lock_exclusive(target: &File) -> std::io::Result<()> {
    loop {
        match rustix::fs::flock(target, rustix::fs::FlockOperation::LockExclusive) {
            Ok(()) => return Ok(()),
            Err(rustix::io::Errno::INTR) => {
                // Test-only: lets `lock_tests.rs` observe that a real EINTR
                // was retried here, rather than just asserting the overall
                // call eventually succeeded — which could pass even if the
                // signal never actually landed inside this syscall. No
                // effect on the retry itself; compiled out entirely in
                // non-test builds.
                #[cfg(test)]
                EINTR_RETRIES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                continue;
            }
            Err(errno) => return Err(errno.into()),
        }
    }
}

/// Count of `EINTR` retries `lock_exclusive` has performed, process-wide.
/// Test-only instrumentation — see the `#[cfg(test)]` increment site inside
/// `lock_exclusive` above.
#[cfg(all(unix, test))]
pub(super) static EINTR_RETRIES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Block until `target`'s exclusive advisory lock is acquired, via
/// `std::fs::File::lock` (`LockFileEx` under the hood) — Windows has no gap
/// for this to work around, see the module doc.
#[cfg(windows)]
fn lock_exclusive(target: &File) -> std::io::Result<()> {
    target.lock()
}

/// Attempt to acquire `target`'s exclusive advisory lock without blocking,
/// reporting [`std::fs::TryLockError::WouldBlock`] if another handle
/// already holds it rather than waiting. Used only by this crate's own test
/// probes (`super::probe_try_init_lock`, and the in-process `try_lock` tests
/// in `lock_tests.rs`) — `with_init_lock` itself always blocks via
/// [`lock_exclusive`].
#[cfg(unix)]
pub(super) fn try_lock_exclusive(target: &File) -> Result<(), std::fs::TryLockError> {
    match rustix::fs::flock(target, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(()),
        Err(errno) => {
            let err: std::io::Error = errno.into();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                Err(std::fs::TryLockError::WouldBlock)
            } else {
                Err(std::fs::TryLockError::Error(err))
            }
        }
    }
}

/// Attempt to acquire `target`'s exclusive advisory lock without blocking,
/// via `std::fs::File::try_lock` (`LockFileEx` under the hood) — Windows has
/// no gap for this to work around, see the module doc.
#[cfg(windows)]
pub(super) fn try_lock_exclusive(target: &File) -> Result<(), std::fs::TryLockError> {
    target.try_lock()
}
