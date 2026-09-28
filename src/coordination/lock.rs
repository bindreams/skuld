//! Blocking cross-process advisory lock serializing creation, publication,
//! and schema initialization of the coordination database.
//!
//! Whoever holds a blocking, exclusive advisory lock on the lock target
//! below is the only actor in the whole system allowed to create, publish,
//! or initialize `.skuld.db` at that instant; every other
//! [`super::connect`]/[`super::open_db`] call blocks until it releases. That
//! removes the specific create/publish and cold-start-negotiation races
//! those two functions document, not all waiting: SQLite's own locking still
//! applies to work done *while* this lock is held, against connections that
//! never take this lock at all (e.g. another process's `BEGIN EXCLUSIVE`
//! inside `coordinate`) — that residual contention is handled by
//! `super::retry_busy`'s uncapped, error-code-gated retry, not rusqlite's own
//! default 5 s `busy_timeout` (every connection [`super::connect_locked`]
//! returns has that disabled — see its own doc).
//!
//! The lock itself is `flock` on Unix, `LockFileEx` on Windows — but the two
//! platforms reach it through different code. Windows goes through
//! [`std::fs::File`]'s own native `lock`/`try_lock` (stable since Rust
//! 1.89.0). Unix goes through [`rustix::fs::flock`] instead of that same
//! `std::fs::File` API: std only implements `lock`/`try_lock` on a subset of
//! the Unix targets it otherwise treats as `flock`-capable, and Android is
//! missing from that subset — calling `std`'s version there panics
//! `"lock() not supported"` on every `open_db` call instead of ever
//! acquiring anything (`TestRegistration::drop` no longer takes this lock
//! at all — see its own doc). `rustix::fs::flock` calls the same underlying
//! syscall directly on every Unix this crate supports, Android included.
//!
//! **The lock target can't be deleted or replaced by anything short of
//! recreating the directory (Unix) or the file (Windows) it lives at.** That
//! is what lets [`with_init_lock`]'s own acquisition be a single
//! open-then-lock with no retry loop: an open that succeeds is already the
//! lock every other caller will contend, permanently, for as long as this
//! handle stays open — *unless* something outside this crate replaces the
//! lock target's directory entry wholesale while that handle is open (see
//! the Unix bullet below), which does still need checking for, and does get
//! checked — see [`InitLockHeld::target_has_split`] — everywhere a caller
//! goes on to do uncapped-retry writes while holding this lock, not at
//! acquisition alone.
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
//!   it** — the same "path now means something else" hazard [`super::connect`]'s
//!   doc covers for `.skuld.db` itself, not a risk this crate accepts:
//!   renaming the directory aside and `mkdir`ing a fresh one at the same
//!   path, or emptying it, `rmdir`ing it, and `mkdir`ing it again, both
//!   leave the holder locking its old (now-detached) directory while every
//!   new opener locks the fresh one instead. [`InitLockHeld::target_has_split`]
//!   is what tells the two groups apart: it records the held directory's
//!   own `fstat` identity at acquisition and compares it against a fresh
//!   `stat` of the same path on each check, the same dev+ino comparison
//!   `super::FileIdentity` does for the DB file itself — a mismatch means
//!   this handle no longer locks what a fresh opener would.
//! - **Windows** locks a sibling [`lock_path`] file, opened with
//!   `share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)` and, deliberately, no
//!   `FILE_SHARE_DELETE`. Windows refuses to delete or rename the lock file
//!   *itself* out from under any handle that didn't grant that share flag —
//!   but a symlink or junction ancestor of `db_path` being retargeted
//!   changes what [`lock_path`] resolves to for a *new* opener without
//!   touching the held file at all, the same split as the Unix bullet
//!   above, by a different mechanism. [`InitLockHeld::target_has_split`]
//!   checks for it on Windows too, the same dev/file-index comparison
//!   technique.
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

/// Compile-time proof that the calling stack frame currently holds *some*
/// init lock, obtained only from inside [`with_init_lock`]'s own closure —
/// carrying the [`Path`] that lock was acquired for, so callers can check
/// (via [`InitLockHeld::path`]) that it's actually *their* path's lock, not
/// merely a token proving some unspecified lock is held. That check is a
/// `debug_assert_eq!` at each real call site (`super::connect_locked`,
/// `super::ensure_schema_locked`), not something this type enforces by
/// construction: nothing here stops a caller from holding path A's lock and
/// passing its token alongside path B — the type system only proves *a*
/// lock is held, never *which* one matches the path in hand. Exists so
/// functions that must only ever run while the right lock is held don't
/// rely purely on a doc comment and callers remembering to nest correctly:
/// a caller with no lock at all has no `&InitLockHeld` to pass, which is a
/// compile error; a caller holding the *wrong* path's lock still compiles,
/// but fails loudly in debug builds instead of silently. Not constructible
/// outside this module, and carries no data beyond the path — it's a
/// marker, not a capability that could itself be smuggled out and reused
/// after the lock releases (there's nothing about holding a `&InitLockHeld`
/// past `with_init_lock`'s call that the borrow checker doesn't already
/// rule out, since the reference can't outlive the closure it was handed
/// into).
pub(super) struct InitLockHeld<'a> {
    path: &'a Path,
    /// `fstat`-derived dev+ino (Unix) or `GetFileInformationByHandle`-derived
    /// volume serial + file index (Windows) of the lock target `File` this
    /// token's lock is actually held on, recorded once at acquisition —
    /// see [`Self::target_has_split`].
    #[cfg(unix)]
    target_identity: (u64, u64),
    #[cfg(windows)]
    target_identity: (u32, u64),
}

impl<'a> InitLockHeld<'a> {
    /// The path [`with_init_lock`] acquired this token's lock for.
    pub(super) fn path(&self) -> &Path {
        self.path
    }

    /// True once the directory this token's lock is actually held on (its
    /// own `fstat`, recorded at acquisition) no longer matches what
    /// [`Self::path`]'s parent directory currently resolves to on disk —
    /// the lock has been "split": something replaced the lock target
    /// wholesale (rename-aside + fresh `mkdir`, or equivalent) while this
    /// lock was held, so a *new* opener now locks the fresh directory while
    /// this handle still only excludes callers of the old, now-detached
    /// one. See the module doc's "Wholesale replacement" bullet.
    ///
    #[cfg(unix)]
    pub(super) fn target_has_split(&self) -> bool {
        use std::os::unix::fs::MetadataExt;
        let dir = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        match std::fs::metadata(dir) {
            Ok(m) => (m.dev(), m.ino()) != self.target_identity,
            Err(_) => true,
        }
    }

    /// `FILE_SHARE_DELETE` being withheld (see the module doc's Windows
    /// bullet) rules out the lock *file itself* being deleted or renamed
    /// while held — it says nothing about an ancestor directory of `path`
    /// being retargeted via a symlink or junction, which changes what
    /// [`lock_path`] resolves to for a *new* opener without touching the
    /// held file at all (the same gap `FileIdentity`'s own doc describes
    /// for the main DB file — confirmed real on Windows by a throwaway CI
    /// probe before that type had a real Windows implementation). So this
    /// checks for real here too: a fresh open of [`lock_path`] plus
    /// `GetFileInformationByHandle`, compared against the identity
    /// recorded at acquisition.
    #[cfg(windows)]
    pub(super) fn target_has_split(&self) -> bool {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};

        let Ok(fresh) = std::fs::File::open(lock_path(self.path)) else {
            return true;
        };
        let handle = windows::Win32::Foundation::HANDLE(fresh.as_raw_handle());
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // Safety: `handle` is a valid, currently-open handle for as long as
        // `fresh` is alive, which outlives this call; `&mut info` is a
        // valid `*mut BY_HANDLE_FILE_INFORMATION` for the call to write
        // into.
        if unsafe { GetFileInformationByHandle(handle, &mut info) }.is_err() {
            return true;
        }
        let current = (
            info.dwVolumeSerialNumber,
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        );
        current != self.target_identity
    }
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
pub(super) fn with_init_lock<T>(db_path: &Path, f: impl FnOnce(&InitLockHeld<'_>) -> T) -> T {
    let target = open_lock_target(db_path);
    lock_exclusive(&target)
        .unwrap_or_else(|e| panic!("skuld: failed to acquire coordination DB init lock for {db_path:?}: {e}"));
    #[cfg(unix)]
    let target_identity = {
        use std::os::unix::fs::MetadataExt;
        target.metadata().map(|m| (m.dev(), m.ino())).unwrap_or_else(|e| {
            panic!("skuld: failed to inspect coordination DB init lock target for {db_path:?}: {e}")
        })
    };
    #[cfg(windows)]
    let target_identity = {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};

        let handle = windows::Win32::Foundation::HANDLE(target.as_raw_handle());
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // Safety: `handle` is a valid, currently-open handle for as long as
        // `target` is alive, which outlives this call; `&mut info` is a
        // valid `*mut BY_HANDLE_FILE_INFORMATION` for the call to write
        // into.
        unsafe { GetFileInformationByHandle(handle, &mut info) }.unwrap_or_else(|e| {
            panic!("skuld: failed to inspect coordination DB init lock target for {db_path:?}: {e}")
        });
        (
            info.dwVolumeSerialNumber,
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        )
    };
    f(&InitLockHeld {
        path: db_path,
        target_identity,
    })
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
