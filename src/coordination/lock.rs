//! Blocking cross-process advisory lock serializing creation, publication,
//! and schema initialization of the coordination database.
//!
//! Two distinct races used to live in [`super::connect`] and
//! [`super::open_db`], each papered over with a retry: a single grace
//! reopen in `connect`, betting that a concurrent publisher's rename would
//! land within one retry; and a sleep-then-retry loop in `open_db`, betting
//! that `SQLITE_READONLY` from `PRAGMA journal_mode = WAL`'s cold-start
//! negotiation over the freshly-created `-shm` file would clear within 50
//! attempts. Both bets can lose — two bad interleavings in a row still
//! panic `connect`, and enough concurrent connections can still exhaust
//! `open_db`'s cap — so both are replaced here with a real primitive: a
//! blocking OS advisory lock (`flock` on Unix, `LockFileEx` on Windows, via
//! [`std::fs::File`]'s own native `lock`/`unlock`, stable since Rust
//! 1.89.0 — no third-party crate needed; see the note on that choice
//! below) on a sibling `<db path>.lock` file. Whoever holds it is
//! the only actor in the whole system allowed to create, publish, or
//! initialize `.skuld.db` at that instant; every other connection blocks
//! until it releases, so by the time any connection runs its own
//! create-or-open sequence, either it's the sole creator (no race to
//! retry through) or the database and its `-shm` companion are already
//! fully set up (no cold-start negotiation left to lose).
//!
//! The lock only ever needs to be held for a handful of local filesystem
//! and SQLite metadata calls — the file lock's own acquisition apart, none
//! of the code that runs under it does any waiting of its own — so serial
//! use imposes no meaningful cost even though every [`super::connect`] and
//! [`super::open_db`] call takes it, not just the very first one for a
//! given path.
//!
//! This uses [`std::fs::File::lock`] directly rather than a crate like
//! `fs4` or `fd-lock`: those crates expose the identical primitive
//! (`flock`/`LockFileEx`) through the same `lock`/`unlock` method names,
//! but `std::fs::File` has had its own inherent methods of the same name
//! and behavior, stable since Rust 1.89.0, and inherent methods always win
//! Rust's method resolution over an identically-named trait method — so
//! adding one of those crates as a dependency here would not actually
//! change which code runs, only which crate's now-dead `use` line sits
//! above it. Nothing in this workspace pins a minimum Rust version below
//! 1.89.0 (no `rust-version` field, no `rust-toolchain.toml`), and CI
//! always installs current `stable`, so the native method is available
//! everywhere this crate is actually built.

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
/// [`lock_path`]. Blocks with no timeout — the same kind of wait
/// `busy_timeout` and SQLite's own locking already do elsewhere in this
/// module, not a poll loop with a chosen interval.
///
/// The lock file is created if absent but never removed or truncated by
/// this module: concurrent `OpenOptions::create(true)` calls against the
/// same path are safe (the lock file's *contents* are never read or
/// written, only its identity as a lockable object matters), and Rust's
/// `File` grants other processes sharing access to it by default on both
/// Unix (no `O_EXCL`) and Windows (`FILE_SHARE_READ | FILE_SHARE_WRITE`),
/// so every caller can always get its own handle to lock.
///
/// Releasing the lock is not a separate step this function performs:
/// `lock_file` going out of scope — on `f`'s normal return *and* on an
/// unwinding panic from `f`, since Rust always runs destructors during
/// unwind — closes the underlying fd/handle, and both `flock` and
/// `LockFileEx` release their lock unconditionally when the last handle to
/// it closes. A panic inside `f` (a genuinely broken DB path, for example)
/// therefore can never leave the lock held.
pub(super) fn with_init_lock<T>(db_path: &Path, f: impl FnOnce() -> T) -> T {
    let path = lock_path(db_path);
    let lock_file = open_lock_file(&path);
    lock_file
        .lock()
        .unwrap_or_else(|e| panic!("skuld: failed to acquire coordination DB init lock at {path:?}: {e}"));
    f()
}

fn open_lock_file(path: &Path) -> File {
    OpenOptions::new()
        .create(true)
        // Explicit, not the default: the lock file's contents are never read
        // or written by this module (only its identity as a lockable object
        // matters), so truncating it on open would just be pointless I/O
        // against a file every other concurrent caller may also be opening
        // right now.
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .unwrap_or_else(|e| panic!("skuld: failed to open coordination DB init lock file {path:?}: {e}"))
}
