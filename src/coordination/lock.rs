//! The global mutex of the coordination directory `.skuld/`: a blocking, exclusive advisory lock
//! that every test taking part in coordination holds while it creates or deletes anything in it.
//!
//! It is taken for the coordination directory `coord_dir`, on the path [`lock_path`] returns. Unix
//! locks `coord_dir` itself, opened `O_RDONLY | O_DIRECTORY`: `flock` works on a read-only
//! directory fd, so there is no lock file, and deleting files inside the directory cannot split
//! the lock. Windows locks `coord_dir/global.lock`, opened without `FILE_SHARE_DELETE` so it cannot
//! be deleted or renamed while held. Opening it needs read permission on the directory (Unix) or
//! create permission (Windows).
//!
//! An open that succeeds is already the lock every other caller contends for, as long as the
//! handle stays open, so acquisition is one open and one lock, with no retry loop. What can still
//! go wrong is the target being replaced wholesale while held: the directory renamed away and a
//! fresh one made (Unix), or a symlink or junction ancestor retargeted (both platforms). The
//! holder then excludes nobody a fresh opener would. [`GlobalLockHeld::target_has_split`] compares
//! the identity recorded at acquisition with what the path resolves to now; callers check it
//! before and after they write.
//!
//! Unix goes through [`rustix::fs::flock`] rather than [`std::fs::File::lock`]: std does not
//! implement `lock` on every Unix target it otherwise treats as `flock`-capable (Android is
//! missing), and panics there. Windows uses `File::lock`/`try_lock` (`LockFileEx`).
//!
//! Any failure to open the target (a missing directory, `EMFILE`, permissions) panics at once,
//! naming the path: retrying the same open does not change the outcome. So does a filesystem whose
//! `flock` refuses a directory (some network filesystems): there is no fallback mechanism.

use super::file_identity::FileIdentity;
use super::skuld_debug_eprintln;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// The share mode of every file the coordination directory holds open: no `FILE_SHARE_DELETE`, so
/// none can be deleted or renamed while open.
#[cfg(windows)]
pub(super) const WINDOWS_SHARE_MODE: u32 =
    windows::Win32::Storage::FileSystem::FILE_SHARE_READ.0 | windows::Win32::Storage::FileSystem::FILE_SHARE_WRITE.0;

/// What the mutex of `coord_dir` locks: `coord_dir` itself on Unix, `coord_dir/global.lock` on
/// Windows. The path every message about the lock names.
pub(super) fn lock_path(coord_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        coord_dir.join("global.lock")
    } else {
        coord_dir.to_path_buf()
    }
}

/// Proof that the global mutex is held, carrying the coordination directory it was taken for.
/// Obtainable only inside [`with_global_lock`]'s closure.
pub(super) struct GlobalLockHeld<'a> {
    coord_dir: &'a Path,
    /// The open lock target, which on Unix is `coord_dir` itself: what the holder does inside the
    /// directory goes through it, so a holder whose lock has split cannot touch the replacement.
    #[cfg(unix)]
    target: &'a File,
    /// Of the lock target this token's lock is held on, recorded at acquisition; see
    /// [`Self::target_has_split`].
    target_identity: FileIdentity,
}

impl<'a> GlobalLockHeld<'a> {
    pub(super) fn coord_dir(&self) -> &'a Path {
        self.coord_dir
    }

    /// The open directory `coord_dir` was when the lock was taken, whatever its path names now.
    #[cfg(unix)]
    pub(super) fn dir(&self) -> &'a File {
        self.target
    }

    /// True once the lock target this token holds no longer matches what `coord_dir` resolves to:
    /// the lock has split, so a new opener locks the replacement while this handle only excludes
    /// callers of the detached original. Resolves [`lock_path`] afresh and compares it with the
    /// identity recorded at acquisition. On Windows, withholding `FILE_SHARE_DELETE` stops the lock
    /// file itself being deleted or renamed, but not a symlink or junction ancestor being
    /// retargeted.
    pub(super) fn target_has_split(&self) -> bool {
        let lock = lock_path(self.coord_dir);
        match FileIdentity::try_of(&lock) {
            Ok(fresh) => fresh != self.target_identity,
            Err(e) => {
                skuld_debug_eprintln!("global lock target {lock:?} could not be inspected, treating as split: {e}");
                true
            }
        }
    }
}

/// Run `f` while holding the global mutex of `coord_dir` (see the module doc). Blocks with no
/// timeout. The lock is released when the handle closes, on `f`'s return and on a panic in it.
pub(super) fn with_global_lock<T>(coord_dir: &Path, f: impl FnOnce(&GlobalLockHeld<'_>) -> T) -> T {
    with_locked_target(coord_dir, &open_lock_target(coord_dir), f)
}

/// [`with_global_lock`] on a `target` the caller already opened with [`open_lock_target`] and
/// keeps: the mutex is released by unlocking `target`, not closing it, when `f` returns or
/// panics, so the caller can lock it again later without opening anything. A registration holds
/// its `target` for that, because a test that has used up its file descriptors must still be able
/// to unregister. `target` must not be locked already.
pub(super) fn with_locked_target<T>(coord_dir: &Path, target: &File, f: impl FnOnce(&GlobalLockHeld<'_>) -> T) -> T {
    lock_exclusive(target).unwrap_or_else(|e| {
        panic!(
            "skuld: failed to acquire the global lock {:?}: {e}",
            lock_path(coord_dir)
        )
    });
    /// Unlocks on drop, so a panic in `f` does not leave the mutex held by a handle that outlives it.
    struct Unlock<'a>(&'a File);
    impl Drop for Unlock<'_> {
        fn drop(&mut self) {
            if let Err(e) = unlock(self.0) {
                skuld_debug_eprintln!("failed to unlock the global lock: {e}");
                debug_assert!(false, "failed to unlock the global lock: {e}");
            }
        }
    }
    let _release = Unlock(target);
    let target_identity = FileIdentity::of_file(target).unwrap_or_else(|e| {
        panic!(
            "skuld: failed to inspect the global lock {:?}: {e}",
            lock_path(coord_dir)
        )
    });
    f(&GlobalLockHeld {
        coord_dir,
        #[cfg(unix)]
        target,
        target_identity,
    })
}

/// Open `coord_dir`'s lock target ([`lock_path`]), ready to be locked or try-locked. Panics if it
/// cannot be opened.
pub(super) fn open_lock_target(coord_dir: &Path) -> File {
    open_target(coord_dir).unwrap_or_else(|e| open_failed(coord_dir, &e))
}

/// [`open_lock_target`], except that `None` means the target does not exist (its directory is
/// missing), which a caller that creates the directory can retry.
pub(super) fn open_lock_target_if_present(coord_dir: &Path) -> Option<File> {
    match open_target(coord_dir) {
        Ok(target) => Some(target),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => open_failed(coord_dir, &e),
    }
}

fn open_failed(coord_dir: &Path, e: &std::io::Error) -> ! {
    let what = if cfg!(windows) {
        "the global lock file"
    } else {
        "the coordination directory"
    };
    panic!(
        "skuld: failed to open {what} {:?} for locking: {e} ({})",
        lock_path(coord_dir),
        super::DB_DIR_HINT
    )
}

fn open_target(coord_dir: &Path) -> std::io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        debug_assert!(
            coord_dir.is_absolute(),
            "coordination directory must be absolute: {coord_dir:?}"
        );
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(coord_dir)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        OpenOptions::new()
            .create(true)
            .write(true)
            // Only the lock's identity matters, never its contents; other callers may be
            // opening the file right now.
            .truncate(false)
            .read(true)
            // No FILE_SHARE_DELETE: keeps the file from being deleted or renamed while held.
            .share_mode(WINDOWS_SHARE_MODE)
            .open(lock_path(coord_dir))
    }
}

/// Block until `target`'s exclusive advisory lock is acquired. See the module doc for why Unix
/// uses [`rustix::fs::flock`] instead of `std::fs::File::lock`.
///
/// Retries on `EINTR` alone, uncapped: a signal handler registered without `SA_RESTART` makes the
/// kernel return `EINTR` from a blocking `flock` that neither acquired nor denied the lock, as
/// std's own `File::lock` handles it. Nothing else `flock` returns is retryable.
#[cfg(unix)]
pub(super) fn lock_exclusive(target: &File) -> std::io::Result<()> {
    loop {
        match rustix::fs::flock(target, rustix::fs::FlockOperation::LockExclusive) {
            Ok(()) => return Ok(()),
            Err(rustix::io::Errno::INTR) => {
                // Lets `lock_tests.rs` observe that a real EINTR was retried, not just that the
                // call eventually succeeded.
                #[cfg(test)]
                EINTR_RETRIES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                continue;
            }
            Err(errno) => return Err(errno.into()),
        }
    }
}

/// Count of `EINTR` retries `lock_exclusive` has performed, process-wide.
#[cfg(all(unix, test))]
pub(super) static EINTR_RETRIES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Block until `target`'s exclusive advisory lock is acquired (`LockFileEx`).
#[cfg(windows)]
pub(super) fn lock_exclusive(target: &File) -> std::io::Result<()> {
    target.lock()
}

/// Release `target`'s lock now, ahead of closing it. Closing alone releases it too, but Windows
/// documents the release on close as taking "available system resources" to happen, so a holder
/// whose release others wait on unlocks first.
#[cfg(unix)]
pub(super) fn unlock(target: &File) -> std::io::Result<()> {
    rustix::fs::flock(target, rustix::fs::FlockOperation::Unlock).map_err(Into::into)
}

/// See the Unix variant.
#[cfg(windows)]
pub(super) fn unlock(target: &File) -> std::io::Result<()> {
    target.unlock()
}

/// Attempt to acquire `target`'s exclusive advisory lock without blocking, reporting
/// [`std::fs::TryLockError::WouldBlock`] if another handle holds it. The registry's scan uses it
/// to test whether a test's lock is still held.
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

/// See the Unix variant (`LockFileEx`).
#[cfg(windows)]
pub(super) fn try_lock_exclusive(target: &File) -> Result<(), std::fs::TryLockError> {
    target.try_lock()
}
