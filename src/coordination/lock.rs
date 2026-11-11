//! The global mutex of the coordination directory `.skuld/`: a blocking, exclusive advisory lock
//! that every test taking part in coordination holds while it creates or deletes anything in it.
//!
//! It is taken for a base path (`<coord_dir>/global`). Unix locks the base's parent directory
//! itself, opened `O_RDONLY | O_DIRECTORY`: `flock` works on a read-only directory fd, so there is
//! no lock file, and deleting files inside the directory cannot split the lock. Windows locks the
//! sibling file `<base>.lock` ([`lock_path`]), opened without `FILE_SHARE_DELETE` so it cannot be
//! deleted or renamed while held. Opening it needs read permission on the directory (Unix) or
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

use super::skuld_debug_eprintln;
use std::fs::{File, OpenOptions};
use std::path::Path;

#[cfg(windows)]
use std::path::PathBuf;

/// The lock file for `base` on Windows: `base` with `.lock` appended verbatim, so `<dir>/global`
/// gets `<dir>/global.lock`. Unix has no lock file (see the module doc).
#[cfg(windows)]
pub(super) fn lock_path(base: &Path) -> PathBuf {
    let mut name = base.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Proof that the global mutex is held, carrying the base path it was taken for. Obtainable only
/// inside [`with_global_lock`]'s closure.
pub(super) struct GlobalLockHeld<'a> {
    path: &'a Path,
    /// dev+ino (Unix) or volume serial + file index (Windows) of the lock target this token's lock
    /// is held on, recorded at acquisition; see [`Self::target_has_split`].
    #[cfg(unix)]
    target_identity: (u64, u64),
    #[cfg(windows)]
    target_identity: (u32, u64),
}

impl<'a> GlobalLockHeld<'a> {
    /// True once the lock target this token holds no longer matches what the base path
    /// resolves to: the lock has split, so a new opener locks the replacement while this handle
    /// only excludes callers of the detached original. Unix compares the parent directory's
    /// recorded identity against a fresh `stat`.
    #[cfg(unix)]
    pub(super) fn target_has_split(&self) -> bool {
        match self.stat_target() {
            Ok(identity) => identity != self.target_identity,
            Err(e) => {
                skuld_debug_eprintln!(
                    "global lock target of {:?} could not be inspected, treating as split: {e}",
                    self.path
                );
                true
            }
        }
    }

    /// Fresh dev+ino of the lock target directory, for comparison with the identity recorded
    /// at acquisition.
    #[cfg(unix)]
    pub(super) fn stat_target(&self) -> std::io::Result<(u64, u64)> {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(lock_dir(self.path)).map(|m| (m.dev(), m.ino()))
    }

    /// Windows: compares a fresh open of [`lock_path`] against the recorded identity.
    /// Withholding `FILE_SHARE_DELETE` stops the lock file itself being deleted or renamed, but
    /// not a symlink or junction ancestor being retargeted.
    #[cfg(windows)]
    pub(super) fn target_has_split(&self) -> bool {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};

        let fresh = match std::fs::File::open(lock_path(self.path)) {
            Ok(f) => f,
            Err(e) => {
                skuld_debug_eprintln!(
                    "global lock file of {:?} could not be reopened, treating as split: {e}",
                    self.path
                );
                return true;
            }
        };
        let handle = windows::Win32::Foundation::HANDLE(fresh.as_raw_handle());
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // Safety: `handle` is a valid, currently-open handle for as long as
        // `fresh` is alive, which outlives this call; `&mut info` is a
        // valid `*mut BY_HANDLE_FILE_INFORMATION` for the call to write
        // into.
        if let Err(e) = unsafe { GetFileInformationByHandle(handle, &mut info) } {
            skuld_debug_eprintln!(
                "global lock file of {:?} could not be inspected, treating as split: {e}",
                self.path
            );
            return true;
        }
        let current = (
            info.dwVolumeSerialNumber,
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        );
        current != self.target_identity
    }
}

/// Run `f` while holding the global mutex for `base` (see the module doc). Blocks with no
/// timeout. The lock is released when the handle closes, on `f`'s return and on a panic in it.
pub(super) fn with_global_lock<T>(base: &Path, f: impl FnOnce(&GlobalLockHeld<'_>) -> T) -> T {
    let target = open_lock_target(base);
    lock_exclusive(&target).unwrap_or_else(|e| panic!("skuld: failed to acquire the global lock for {base:?}: {e}"));
    #[cfg(unix)]
    let target_identity = {
        use std::os::unix::fs::MetadataExt;
        target
            .metadata()
            .map(|m| (m.dev(), m.ino()))
            .unwrap_or_else(|e| panic!("skuld: failed to inspect the global lock target for {base:?}: {e}"))
    };
    #[cfg(windows)]
    let target_identity = {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};

        let handle = windows::Win32::Foundation::HANDLE(target.as_raw_handle());
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // Safety: `handle` is a valid, currently-open handle for as long as
        // `target` is alive, which outlives this call; `&mut info` is a
        // valid `*mut BY_HANDLE_FILE_INFORMATION` for the call to write
        // into.
        unsafe { GetFileInformationByHandle(handle, &mut info) }
            .unwrap_or_else(|e| panic!("skuld: failed to inspect the global lock target for {base:?}: {e}"));
        (
            info.dwVolumeSerialNumber,
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        )
    };
    f(&GlobalLockHeld {
        path: base,
        target_identity,
    })
}

/// The directory holding `base`, which the Unix lock is taken on. `base` is always absolute (it
/// derives from `coord_dir()`), so it always has a parent.
#[cfg(unix)]
fn lock_dir(base: &Path) -> &Path {
    debug_assert!(base.is_absolute(), "global lock base path must be absolute: {base:?}");
    base.parent().expect("an absolute path has a parent")
}

/// Open `base`'s lock target, ready to be locked or try-locked: `base`'s parent directory on
/// Unix, [`lock_path`]'s file on Windows. Panics if it cannot be opened.
pub(super) fn open_lock_target(base: &Path) -> File {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let dir = lock_dir(base);
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(dir)
            .unwrap_or_else(|e| {
                panic!(
                    "skuld: failed to open the coordination directory {dir:?} for locking: {e} ({})",
                    super::DB_DIR_HINT
                )
            })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

        let path = lock_path(base);
        OpenOptions::new()
            .create(true)
            .write(true)
            // Only the lock's identity matters, never its contents; other callers may be
            // opening the file right now.
            .truncate(false)
            .read(true)
            // No FILE_SHARE_DELETE: keeps the file from being deleted or renamed while held.
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
            .open(&path)
            .unwrap_or_else(|e| {
                panic!(
                    "skuld: failed to open the global lock file {path:?}: {e} ({})",
                    super::DB_DIR_HINT
                )
            })
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
