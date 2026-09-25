//! Blocking cross-process advisory lock serializing creation, publication,
//! and schema initialization of the coordination database.
//!
//! Whoever holds a blocking, exclusive advisory lock (`flock` on Unix,
//! `LockFileEx` on Windows, via [`std::fs::File`]'s own native
//! `lock`/`unlock`, stable since Rust 1.89.0 — `fs4`/`fd-lock` expose the
//! identical primitive through the same method names, but an inherent
//! method always wins Rust's resolution over an identically-named trait
//! method, so depending on either crate here would only add a dead `use`
//! line) on a sibling `<db path>.lock` file is the only actor in the whole
//! system allowed to create, publish, or initialize `.skuld.db` at that
//! instant; every other [`super::connect`]/[`super::open_db`] call blocks
//! until it releases. That removes the specific create/publish and
//! cold-start-negotiation races those two functions document, not all
//! waiting: `busy_timeout` and SQLite's own locking still apply to work done
//! *while* this lock is held.
//!
//! The lock file is published at 0666 the same way `.skuld.db` itself is
//! (see [`super::publish`]) and then opened read-only: both `flock` and
//! `LockFileEx` only need read access on the handle, and this module never
//! reads or writes the lock file's contents, only locks it — so opening it
//! read-write, which [`super::publish`] exists specifically to avoid needing
//! for `.skuld.db`, would reintroduce the same umask lockout here for no
//! reason.
//!
//! `flock`/`LockFileEx` lock a file's identity (inode on Unix, file object
//! on Windows), not a path: if whatever is at [`lock_path`] gets deleted and
//! recreated while a handle holds the lock, a second caller opening the path
//! afterward gets a lock on the new, different identity, and the two
//! callers no longer exclude each other. [`with_init_lock`] guards against
//! this by comparing the locked handle's identity against a fresh stat of
//! the path right after locking, and retrying — reopen, relock, recheck —
//! if they differ or the path is gone. That retry has no attempt cap: each
//! repeat means the file was genuinely replaced out from under the previous
//! handle, by something outside this process's control, not a fixed number
//! of tries to exhaust.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// The sibling lock file guarding `db_path`'s creation, publication, and
/// schema initialization: `db_path` with `.lock` appended verbatim (not
/// [`Path::with_extension`], which would replace `.skuld.db`'s existing
/// `db` extension instead of appending), so `.skuld.db` gets
/// `.skuld.db.lock`.
pub(super) fn lock_path(db_path: &Path) -> PathBuf {
    let mut name = db_path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Run `f` while holding a blocking, exclusive advisory lock on `db_path`'s
/// [`lock_path`]. Blocks with no timeout on the `flock`/`LockFileEx` call
/// itself; the loop around it (see [`acquire_lock`]) is gated on the lock
/// file having been replaced out from under the handle, not on a timer — not
/// a poll loop with a chosen interval.
///
/// Releasing the lock is not a separate step this function performs: the
/// held `File` going out of scope — on `f`'s normal return *and* on an
/// unwinding panic from `f`, since Rust always runs destructors during
/// unwind — closes the underlying fd/handle, and both `flock` and
/// `LockFileEx` release their lock unconditionally when the last handle to
/// it closes. A panic inside `f` (a genuinely broken DB path, for example)
/// therefore can never leave the lock held.
pub(super) fn with_init_lock<T>(db_path: &Path, f: impl FnOnce() -> T) -> T {
    let path = lock_path(db_path);
    let _lock_file = acquire_lock(&path);
    f()
}

/// Open, lock, and confirm `path`'s lock file is still the one now on disk —
/// looping (uncapped) if it was deleted or replaced between opening and
/// locking, or between publishing and opening. See the module doc for why
/// identity, not just a successful `lock()`, is what this needs to prove.
fn acquire_lock(path: &Path) -> File {
    loop {
        let file = match open_lock_file(path) {
            Ok(file) => file,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound && path_is_absent(path) {
                    // Genuinely absent, not a broken entry an `open` simply
                    // can't traverse (e.g. a dangling symlink, which still
                    // reports "something is there" via `symlink_metadata`
                    // below) — something outside this call raced the
                    // publish step. Go around again.
                    continue;
                }
                panic!("skuld: failed to open coordination DB init lock file {path:?}: {e}");
            }
        };
        file.lock()
            .unwrap_or_else(|e| panic!("skuld: failed to acquire coordination DB init lock at {path:?}: {e}"));
        if lock_file_still_identifies_path(&file, path) {
            return file;
        }
        // `path` was replaced between opening and locking (or is gone
        // again already): this handle's lock now excludes nobody who opens
        // `path` fresh. Drop it here — releasing the now-pointless lock —
        // and retry from scratch against whatever is at `path` now.
    }
}

/// Publish (Unix) and open `path` — the lock file, not `.skuld.db` — read
/// only. Never creates it directly with a raw `open(O_CREAT)`: that would
/// hand back a file at `0666 & ~umask`, the exact lockout
/// [`super::publish::ensure_published`] exists to avoid, so on Unix
/// publishing is always the thing that creates it. Windows has no
/// uid-mixing hazard to guard against (see [`super::connect`]'s doc), so
/// `create(true)` there creates-or-opens atomically in the one call, the
/// same as it always has — `write(true)` alongside it is not optional
/// there: `OpenOptions` rejects `create(true)` outright, before ever
/// touching the filesystem, unless `write` or `append` is also set
/// ("creating or truncating a file requires write or append access"), so
/// this isn't `flock`/`LockFileEx` needing write access, only `std`'s own
/// precondition for the flag combination that creates a file.
fn open_lock_file(path: &Path) -> std::io::Result<File> {
    #[cfg(unix)]
    {
        super::publish::ensure_published(path);
        OpenOptions::new().read(true).open(path)
    }
    #[cfg(not(unix))]
    {
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
            .open(path)
    }
}

/// True when nothing exists at `path` (`symlink_metadata`, not `exists()` —
/// a dangling symlink still counts as "something is there," not absence,
/// mirroring `super::path_is_absent`/`super::publish::ensure_published`'s
/// own fast path — duplicated here, rather than shared, because both are a
/// single `symlink_metadata` call and `super::path_is_absent` is
/// `#[cfg(unix)]`-only while this needs to run on every platform).
fn path_is_absent(path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    )
}

/// True when `file`'s own identity (what it was opened against) still
/// matches whatever a fresh `open` of `path` would resolve to right now.
/// False means `path` was deleted or replaced after `file` was opened —
/// `file`'s lock no longer excludes a caller that opens `path` fresh.
///
/// The fresh side opens `path` itself (via [`file_identity_at_path`]) rather
/// than only `stat`-ing it, on purpose: `OpenOptions::open` follows
/// symlinks, so this has to compare against what a fresh *open* would see,
/// not what a raw `lstat` of the path would — otherwise a lock path that
/// happens to be a symlink would permanently mismatch and this would never
/// terminate. On Windows this doubles as the only way to get a comparable
/// identity at all: see [`file_identity`]'s doc.
fn lock_file_still_identifies_path(file: &File, path: &Path) -> bool {
    let Some(current) = file_identity_at_path(path) else {
        return false;
    };
    file_identity(file) == current
}

/// Open `path` fresh (read-only, following symlinks like every real caller's
/// `open` does) and return its identity, or `None` if it can't be opened —
/// treated by [`lock_file_still_identifies_path`] as "not the same file" the
/// same as an identity mismatch would be, since either way a fresh caller
/// reaching `path` right now would not land on the handle this lock covers.
fn file_identity_at_path(path: &Path) -> Option<(IdentityA, IdentityB)> {
    OpenOptions::new().read(true).open(path).ok().map(|f| file_identity(&f))
}

#[cfg(unix)]
type IdentityA = u64;
#[cfg(unix)]
type IdentityB = u64;
#[cfg(windows)]
type IdentityA = u32;
#[cfg(windows)]
type IdentityB = u64;

#[cfg(unix)]
fn file_identity(file: &File) -> (IdentityA, IdentityB) {
    use std::os::unix::fs::MetadataExt;
    let meta = file
        .metadata()
        .unwrap_or_else(|e| panic!("skuld: failed to stat coordination DB init lock file handle: {e}"));
    (meta.dev(), meta.ino())
}

/// `std::os::windows::fs::MetadataExt`'s `volume_serial_number`/`file_index`
/// are still gated behind the unstable `windows_by_handle` feature
/// (rust-lang/rust#63010), so this calls `GetFileInformationByHandle`
/// directly through the `windows` crate instead of waiting on
/// stabilization — the same two fields
/// (`dwVolumeSerialNumber`/`nFileIndexHigh`+`nFileIndexLow`), just reached
/// through the Win32 API rather than `std`'s not-yet-stable wrapper around
/// it.
#[cfg(windows)]
fn file_identity(file: &File) -> (IdentityA, IdentityB) {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};

    let handle = HANDLE(file.as_raw_handle());
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle, &mut info) }
        .unwrap_or_else(|e| panic!("skuld: GetFileInformationByHandle failed for coordination DB init lock file: {e}"));
    let file_index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    (info.dwVolumeSerialNumber, file_index)
}
