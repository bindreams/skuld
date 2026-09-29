//! Detection of a coordination database moved out from under a live connection.
//!
//! A connection held open across more than one operation keeps writing to the
//! inode it opened even after its path was deleted, replaced or retargeted, so
//! a write through it can corrupt whatever now lives at the path (`-wal`/`-shm`
//! are found by path, not by inode). [`DbIdentity`] records what a connection
//! opened; every write is guarded by [`DbIdentity::panic_if_moved`] and every
//! failure is described by [`DbIdentity::io_failure_message`].
//!
//! Not detected, by construction: content overwritten in place at the same
//! path, device and inode looks exactly like this connection's own writes.
//! "Moved" means "the path now names a different file, or nothing".
//!
//! # How the recorded identity is tied to the connection's own files
//!
//! Recording `stat(path)` after the open would adopt whatever a retarget or
//! swap put there in the meantime, and every later check would then agree with
//! the wrong value. So each identity is read from a file SQLite itself holds
//! open, and the path is then required to name it:
//!
//! - **Unix, main, `-wal` and `-shm`:** the fds are read from SQLite's `unixFile`
//!   structs (see [`unix_fds`]), and the identity is `fstat(fd)`: exact device
//!   and inode, whatever the path now resolves to. The paths (SQLite's own, the
//!   caller's, and each companion's) must then resolve to that identity, so a
//!   file swapped in before recording is rejected, never adopted.
//! - **Windows main:** SQLite exposes its handle
//!   (`SQLITE_FCNTL_WIN32_GET_HANDLE`); the identity is taken from it and the
//!   caller's path must resolve to it. The companion paths come from that
//!   handle's final path name, which contains no reparse point an ancestor
//!   retarget could redirect.
//! - **Windows `-wal` / `-shm`:** SQLite exposes no handle for either. Windows
//!   withholds `FILE_SHARE_DELETE` on both (pinned by a Windows test), so
//!   neither can be replaced except through a reparse-point ancestor, which the
//!   handle-derived path already excludes.

#[cfg(unix)]
pub(super) mod unix_fds;

use std::path::{Path, PathBuf};

/// A file's identity, following symlinks and reparse points as a fresh
/// `connect`/`open_db` of the same path would.
///
/// Unix: device + inode (`stat`). Windows: volume serial + 64-bit file index
/// (`GetFileInformationByHandle`). Resolving a path fresh, not reusing a held
/// handle, is what lets it observe a retargeted ancestor: that changes what a
/// new resolution reaches, not the file, so `FILE_SHARE_DELETE` being
/// withheld does not cover it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct FileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(windows)]
    volume_serial: u32,
    #[cfg(windows)]
    file_index: u64,
}

impl FileIdentity {
    /// `None` means nothing resolvable exists at `path` right now, which is as
    /// much "moved" as a mismatched identity.
    #[cfg(unix)]
    pub(super) fn of(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| Self {
            dev: m.dev(),
            ino: m.ino(),
        })
    }

    #[cfg(windows)]
    pub(super) fn of(path: &Path) -> Option<Self> {
        use std::os::windows::io::AsRawHandle;

        let file = std::fs::File::open(path).ok()?;
        Self::of_handle(windows::Win32::Foundation::HANDLE(file.as_raw_handle()))
    }

    /// `handle` must be a valid open file handle.
    #[cfg(windows)]
    fn of_handle(handle: windows::Win32::Foundation::HANDLE) -> Option<Self> {
        use windows::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};

        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // Safety: the caller guarantees `handle` is valid; `&mut info` is a
        // valid out-pointer.
        unsafe { GetFileInformationByHandle(handle, &mut info) }.ok()?;
        Some(Self {
            volume_serial: info.dwVolumeSerialNumber,
            file_index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        })
    }
}

/// A companion file and the identity recorded for it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Tracked {
    pub(super) path: PathBuf,
    identity: FileIdentity,
}

impl Tracked {
    pub(super) fn has_moved(&self) -> bool {
        FileIdentity::of(&self.path) != Some(self.identity)
    }
}

/// `-wal` and `-shm`. Deleting only a companion leaves the main file's
/// identity untouched, so the main identity alone cannot catch it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Companions {
    pub(super) wal: Tracked,
    pub(super) shm: Tracked,
}

/// `path` with `suffix` appended verbatim (not [`Path::with_extension`], which
/// would replace `.skuld.db`'s `db`).
pub(super) fn companion_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Everything recorded about a connection's files, checked before and after
/// each write for the rest of its life.
///
/// `companions` is `None` until [`Self::with_companions`] runs: `-wal`/`-shm`
/// do not reliably exist before schema init (SQLite deletes both when the last
/// connection closes; `PRAGMA journal_mode = WAL` recreates them). Every
/// later phase (`coordinate`, `TestRegistration`) holds `Some`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct DbIdentity {
    pub(super) main: FileIdentity,
    pub(super) companions: Option<Companions>,
}

impl DbIdentity {
    /// Record `conn`'s main-file identity right after a successful open, while
    /// holding `path`'s init lock. Panics, naming `path`, on any disagreement
    /// between the connection's own file and what `path` resolves to; see the
    /// module doc for how each platform ties the identity to the connection.
    pub(super) fn record_main(conn: &rusqlite::Connection, path: &Path) -> Self {
        Self {
            main: record_main_identity(conn, path),
            companions: None,
        }
    }

    /// Record the `-wal`/`-shm` this connection uses. Call after schema init,
    /// whose `PRAGMA journal_mode = WAL` guarantees both exist, so a missing
    /// one is a broken precondition. Their inodes are stable while any
    /// connection holds the database open (checkpoints truncate in place), so
    /// one recording suffices.
    pub(super) fn with_companions(self, conn: &rusqlite::Connection, path: &Path) -> Self {
        let base = companion_base(conn, path);
        let track = |suffix: &str, fd_identity: Option<FileIdentity>| -> Tracked {
            let companion = companion_path(&base, suffix);
            let on_disk = FileIdentity::of(&companion).unwrap_or_else(|| {
                panic!(
                    "skuld: coordination DB {path:?}'s {suffix} companion is missing right after \
                     schema init, where PRAGMA journal_mode=WAL having just run unconditionally \
                     should guarantee it exists"
                )
            });
            // Unix: the identity is the connection's own fd, and the file at
            // the path must be it, or the companion was replaced between the
            // connection opening it and now.
            if let Some(own) = fd_identity {
                assert_eq!(
                    on_disk, own,
                    "skuld: coordination DB {path:?}'s {suffix} companion at {companion:?} is not the \
                     file this connection has open — it was replaced before being recorded, so the \
                     -wal/-shm files at that path are not the ones this connection is using"
                );
            }
            Tracked {
                path: companion,
                identity: fd_identity.unwrap_or(on_disk),
            }
        };
        #[cfg(unix)]
        let (wal_id, shm_id) = (
            Some(own_fd_identity(unix_fds::wal_fd(conn), path, "-wal")),
            Some(own_fd_identity(unix_fds::shm_fd(conn), path, "-shm")),
        );
        #[cfg(windows)]
        let (wal_id, shm_id) = (None, None);
        let wal = track("-wal", wal_id);
        let shm = track("-shm", shm_id);
        Self {
            main: self.main,
            companions: Some(Companions { wal, shm }),
        }
    }

    /// True once `conn`'s files were deleted, renamed or replaced since this
    /// was recorded for `path`.
    ///
    /// Two independent checks on the main file, either sufficient. Unix:
    /// `SQLITE_FCNTL_HAS_MOVED` re-`stat`s the path string SQLite itself
    /// recorded and compares inode only, so it misses a retargeted symlink
    /// ancestor (SQLite's own string is untouched) and a different device with
    /// the same inode number. The fresh [`FileIdentity::of`] of the caller's
    /// `path` compares device and inode and closes both. Windows has no
    /// `HAS_MOVED` (`winFileControl` answers `SQLITE_NOTFOUND`), so only the
    /// fresh identity applies; `winOpen` withholds `FILE_SHARE_DELETE`, so
    /// nothing can delete or rename an open file, and an ancestor retarget
    /// changes what `path` resolves to and is caught by the identity check.
    pub(super) fn has_moved(&self, conn: &rusqlite::Connection, path: &Path) -> bool {
        #[cfg(unix)]
        if has_moved_via_fcntl(conn) {
            return true;
        }
        #[cfg(windows)]
        let _ = conn;
        FileIdentity::of(path) != Some(self.main)
            || self
                .companions
                .as_ref()
                .is_some_and(|c| c.wal.has_moved() || c.shm.has_moved())
    }

    /// Panic, naming `path`, if [`Self::has_moved`]. Call immediately before
    /// and after every write through a long-lived connection.
    pub(super) fn panic_if_moved(&self, conn: &rusqlite::Connection, path: &Path) {
        if self.has_moved(conn, path) {
            panic!("skuld coordination DB {path:?} was deleted or replaced mid-run");
        }
    }

    /// The message to panic with for `failure`, whatever its error class:
    /// the "moved" message if [`Self::has_moved`] confirms a move right now
    /// (any class can be a symptom of one: `CANTOPEN`, `READONLY`, `NOTADB`,
    /// `CORRUPT` from a swapped `-wal`/`-shm`), else `context` at `path`. Both
    /// name the SQLite error, its extended code and the errno behind it, so an
    /// unrelated failure (`ENOSPC`, say) is never mislabelled as a move.
    pub(super) fn failure_message(
        &self,
        conn: &rusqlite::Connection,
        failure: &Failure,
        path: &Path,
        context: &str,
    ) -> String {
        let detail = failure.detail();
        if self.has_moved(conn, path) {
            format!("skuld coordination DB {path:?} was deleted or replaced mid-run: {detail}")
        } else {
            format!("skuld: {context} at {path:?}: {detail}")
        }
    }
}

/// A SQLite error with the OS errno behind it, captured when it happened.
///
/// `sqlite3_system_errno` is overwritten by every later `CANTOPEN`/`IOERR`, so
/// it must be read before anything else (a best-effort `ROLLBACK`, say) runs on
/// the connection.
pub(crate) struct Failure {
    err: rusqlite::Error,
    errno: Option<i32>,
}

impl Failure {
    /// Capture `err` and, for the error classes SQLite records one for, the
    /// errno. SQLite records none for `SQLITE_FULL` (its errno would be stale),
    /// so a full disk reports the error alone.
    pub(crate) fn capture(conn: &rusqlite::Connection, err: rusqlite::Error) -> Self {
        let records_errno = matches!(
            err.sqlite_error_code(),
            Some(rusqlite::ErrorCode::CannotOpen | rusqlite::ErrorCode::SystemIoFailure)
        );
        // Safety: `conn.handle()` is a valid `sqlite3*` while `conn` is borrowed.
        let errno = records_errno.then(|| unsafe { rusqlite::ffi::sqlite3_system_errno(conn.handle()) });
        Self { err, errno }
    }

    pub(crate) fn error(&self) -> &rusqlite::Error {
        &self.err
    }

    fn detail(&self) -> String {
        let extended = self.err.sqlite_extended_error_code();
        let errno = self
            .errno
            .map_or_else(|| "none recorded for this error class".to_owned(), |n| n.to_string());
        format!("{} (extended code: {extended:?}, system errno: {errno})", self.err)
    }
}

/// The path the `-wal`/`-shm` names are appended to: the one SQLite itself
/// derives them from. Unix: `conn.path()`, SQLite's own resolved filename.
/// Windows: the main handle's final path name, which contains no reparse point.
fn companion_base(conn: &rusqlite::Connection, path: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from(
            conn.path()
                .unwrap_or_else(|| panic!("skuld: coordination DB connection for {path:?} has no path")),
        )
    }
    #[cfg(windows)]
    {
        let _ = (conn, final_path);
        path.to_owned()
    }
}

// Unix =====

/// `SQLITE_FCNTL_HAS_MOVED` alone.
#[cfg(unix)]
pub(super) fn has_moved_via_fcntl(conn: &rusqlite::Connection) -> bool {
    let mut has_moved: std::os::raw::c_int = 0;
    let main = c"main";
    // Safety: `conn.handle()` is a valid `sqlite3*` while `conn` is borrowed;
    // `main` is NUL-terminated; `&mut has_moved` is a valid `*mut c_int` for
    // SQLite's 0-or-1 answer.
    let rc = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            conn.handle(),
            main.as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
            (&raw mut has_moved).cast(),
        )
    };
    assert_eq!(
        rc,
        rusqlite::ffi::SQLITE_OK,
        "skuld: SQLITE_FCNTL_HAS_MOVED file-control failed with code {rc} — this unix SQLite VFS \
         was expected to implement it, which means Skuld can no longer tell a moved database \
         apart from a live one and must not be trusted to keep writing"
    );
    has_moved != 0
}

/// `None` if `fd` is not open.
#[cfg(unix)]
pub(super) fn fd_identity(fd: i32) -> Option<FileIdentity> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // Safety: `st` is a valid out-pointer; `fstat` on a closed fd just fails.
    if unsafe { libc::fstat(fd, st.as_mut_ptr()) } != 0 {
        return None;
    }
    // Safety: `fstat` succeeded, so it initialised `st`.
    let st = unsafe { st.assume_init() };
    // The same widening `std::os::unix::fs::MetadataExt::{dev, ino}` does.
    #[allow(clippy::unnecessary_cast)]
    Some(FileIdentity {
        dev: st.st_dev as u64,
        ino: st.st_ino as u64,
    })
}

/// The identity of `fd`'s file, panicking if it is not open: a companion or
/// main fd read from SQLite must be.
#[cfg(unix)]
fn own_fd_identity(fd: i32, path: &Path, what: &str) -> FileIdentity {
    fd_identity(fd).unwrap_or_else(|| {
        panic!("skuld: coordination DB {path:?}: SQLite's {what:?} file descriptor {fd} is not open")
    })
}

#[cfg(unix)]
fn record_main_identity(conn: &rusqlite::Connection, path: &Path) -> FileIdentity {
    let sqlite_path = Path::new(
        conn.path()
            .unwrap_or_else(|| panic!("skuld: coordination DB connection for {path:?} has no path")),
    );
    // The identity is the connection's own fd; the paths must still name it.
    let identity = own_fd_identity(unix_fds::main_fd(conn), path, "main");
    #[cfg(test)]
    super::test_hooks::run_seam(super::test_hooks::Seam::Fd);
    assert_eq!(
        FileIdentity::of(sqlite_path),
        Some(identity),
        "skuld: coordination DB {path:?}: SQLite's own path {sqlite_path:?} no longer names the file \
         the connection has open — something replaced it in the open-to-record window"
    );
    assert_eq!(
        FileIdentity::of(path),
        Some(identity),
        "skuld: coordination DB {path:?} disagreed with the connection just opened through it — \
         something retargeted it in the open-to-record window"
    );
    identity
}

// Windows =====

/// The connection's own main-file handle (`SQLITE_FCNTL_WIN32_GET_HANDLE`).
#[cfg(windows)]
fn main_handle(conn: &rusqlite::Connection) -> windows::Win32::Foundation::HANDLE {
    let mut handle = windows::Win32::Foundation::HANDLE::default();
    let main = c"main";
    // Safety: `conn.handle()` is a valid `sqlite3*` while `conn` is borrowed;
    // `main` is NUL-terminated; `&mut handle` is a valid `*mut HANDLE`, which
    // is what `SQLITE_FCNTL_WIN32_GET_HANDLE` writes through.
    let rc = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            conn.handle(),
            main.as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_WIN32_GET_HANDLE,
            (&raw mut handle).cast(),
        )
    };
    assert_eq!(
        rc,
        rusqlite::ffi::SQLITE_OK,
        "skuld: SQLITE_FCNTL_WIN32_GET_HANDLE file-control failed with code {rc}"
    );
    handle
}

/// The final path name of `handle`'s file: symlinks and junctions resolved.
#[cfg(windows)]
fn final_path(handle: windows::Win32::Foundation::HANDLE) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::Storage::FileSystem::{GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED};

    let mut buf = vec![0u16; 512];
    loop {
        // Safety: `handle` is valid (it is the connection's own open handle);
        // `buf` is a valid buffer of the length passed.
        let n = unsafe { GetFinalPathNameByHandleW(handle, &mut buf, FILE_NAME_NORMALIZED) } as usize;
        assert!(
            n != 0,
            "skuld: GetFinalPathNameByHandleW failed: {}",
            std::io::Error::last_os_error()
        );
        // `n` is the length written, or, when the buffer was too small, the
        // length needed including the NUL.
        if n < buf.len() {
            return PathBuf::from(std::ffi::OsString::from_wide(&buf[..n]));
        }
        buf.resize(n, 0);
    }
}

#[cfg(windows)]
fn record_main_identity(conn: &rusqlite::Connection, path: &Path) -> FileIdentity {
    let _ = main_handle;
    let identity = FileIdentity::of(path).or_else(|| FileIdentity::of_handle(main_handle(conn))).unwrap_or_else(|| {
        panic!("skuld: coordination DB {path:?}: could not read the connection's own file identity")
    });
    assert_eq!(
        FileIdentity::of(path),
        Some(identity),
        "skuld: coordination DB {path:?} disagreed with the connection just opened through it — \
         something retargeted it in the open-to-record window"
    );
    identity
}
