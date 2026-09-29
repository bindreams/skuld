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

/// A file's identity, following symlinks and reparse points as a fresh
/// `connect`/`open_db` of the same path would.
///
/// Unix: device + inode (`stat`). Windows: volume serial + 64-bit file index
/// (`GetFileInformationByHandle` on a *fresh* open of the path). A held handle
/// would not observe a retargeted ancestor: that changes what a new resolution
/// reaches, not the file, so `FILE_SHARE_DELETE` being withheld does not cover
/// it.
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
    pub(super) fn of(path: &std::path::Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| Self {
            dev: m.dev(),
            ino: m.ino(),
        })
    }

    #[cfg(windows)]
    pub(super) fn of(path: &std::path::Path) -> Option<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};

        let file = std::fs::File::open(path).ok()?;
        let handle = windows::Win32::Foundation::HANDLE(file.as_raw_handle());
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // Safety: `handle` is valid while `file` lives, which outlives this
        // call; `&mut info` is a valid out-pointer.
        unsafe { GetFileInformationByHandle(handle, &mut info) }.ok()?;
        Some(Self {
            volume_serial: info.dwVolumeSerialNumber,
            file_index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        })
    }
}

/// `-wal` and `-shm` identities. Deleting only a companion leaves the main
/// file's identity untouched, so the main identity alone cannot catch it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct CompanionIdentities {
    wal: FileIdentity,
    shm: FileIdentity,
}

/// `path` with `suffix` appended verbatim (not [`std::path::Path::with_extension`],
/// which would replace `.skuld.db`'s `db`).
pub(super) fn companion_path(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    std::path::PathBuf::from(name)
}

/// Everything recorded about a connection's files, checked before and after
/// each write for the rest of its life.
///
/// `companions` is `None` until [`Self::with_companions`] runs: `-wal`/`-shm`
/// do not reliably exist before schema init (SQLite deletes both when the last
/// connection closes; `PRAGMA journal_mode = WAL` recreates them). Every
/// later phase (`coordinate`, `TestRegistration`) holds `Some`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct DbIdentity {
    pub(super) main: FileIdentity,
    pub(super) companions: Option<CompanionIdentities>,
}

impl DbIdentity {
    /// Record `conn`'s main-file identity right after a successful open, while
    /// holding `path`'s init lock.
    ///
    /// Derived from `conn.path()` (what SQLite opened), not from a later
    /// `stat` of `path`, and cross-checked against `path` and the fcntl: any
    /// disagreement means something moved in the open-to-record window, and
    /// panics instead of recording a value that would answer "not moved"
    /// forever.
    pub(super) fn record_main(conn: &rusqlite::Connection, path: &std::path::Path) -> Self {
        let main = record_main_identity(conn, path);
        Self { main, companions: None }
    }

    /// Record `path`'s `-wal`/`-shm` identities. Call after schema init, whose
    /// `PRAGMA journal_mode = WAL` guarantees both exist, so a missing one is a
    /// broken precondition. Their inodes are stable while any connection holds
    /// the database open (checkpoints truncate in place), so one recording
    /// suffices.
    pub(super) fn with_companions(self, path: &std::path::Path) -> Self {
        let missing = |which: &str| -> ! {
            panic!(
                "skuld: coordination DB {path:?}'s {which} companion is missing right after schema \
                 init, where PRAGMA journal_mode=WAL having just run unconditionally should \
                 guarantee it exists"
            )
        };
        let wal = FileIdentity::of(&companion_path(path, "-wal")).unwrap_or_else(|| missing("-wal"));
        let shm = FileIdentity::of(&companion_path(path, "-shm")).unwrap_or_else(|| missing("-shm"));
        Self {
            main: self.main,
            companions: Some(CompanionIdentities { wal, shm }),
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
    pub(super) fn has_moved(&self, conn: &rusqlite::Connection, path: &std::path::Path) -> bool {
        #[cfg(unix)]
        if has_moved_via_fcntl(conn) {
            return true;
        }
        #[cfg(windows)]
        let _ = conn;
        if FileIdentity::of(path) != Some(self.main) {
            return true;
        }
        self.companions.is_some_and(|c| {
            FileIdentity::of(&companion_path(path, "-wal")) != Some(c.wal)
                || FileIdentity::of(&companion_path(path, "-shm")) != Some(c.shm)
        })
    }

    /// Panic, naming `path`, if [`Self::has_moved`]. Call immediately before
    /// and after every write through a long-lived connection.
    pub(super) fn panic_if_moved(&self, conn: &rusqlite::Connection, path: &std::path::Path) {
        if self.has_moved(conn, path) {
            panic!("skuld coordination DB {path:?} was deleted or replaced mid-run");
        }
    }

    /// For an I/O-class `err`, the message to panic with: the "moved" message
    /// if [`Self::has_moved`] confirms it right now, else [`io_error_message`],
    /// so an unrelated I/O failure is not mislabelled as a move. `None` for
    /// every other error class.
    pub(super) fn io_failure_message(
        &self,
        conn: &rusqlite::Connection,
        err: &rusqlite::Error,
        path: &std::path::Path,
    ) -> Option<String> {
        if !is_io_error(err) {
            return None;
        }
        Some(if self.has_moved(conn, path) {
            format!("skuld coordination DB {path:?} was deleted or replaced mid-run: {err}")
        } else {
            io_error_message(conn, err, path)
        })
    }
}

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

fn record_main_identity(conn: &rusqlite::Connection, path: &std::path::Path) -> FileIdentity {
    #[cfg(unix)]
    assert!(
        !has_moved_via_fcntl(conn),
        "skuld: coordination DB {path:?} was already reported moved immediately after being \
         opened — something retargeted it in the open-to-record window"
    );
    let sqlite_path = conn
        .path()
        .unwrap_or_else(|| panic!("skuld: coordination DB connection for {path:?} has no path"));
    let identity = FileIdentity::of(std::path::Path::new(sqlite_path))
        .unwrap_or_else(|| panic!("skuld: coordination DB {path:?} vanished immediately after being opened"));
    assert_eq!(
        FileIdentity::of(path),
        Some(identity),
        "skuld: coordination DB {path:?} disagreed with the connection just opened through it — \
         something retargeted it in the open-to-record window"
    );
    identity
}

/// True for SQLite's "system I/O failed" family (`SQLITE_IOERR` and every
/// extended variant; rusqlite collapses them onto one primary code). A write
/// against a connection whose `-wal`/`-shm` vanished tends to surface this way
/// rather than as "moved", but any I/O failure (`ENOSPC` included) takes the
/// same shape, so this is a reason to check, not evidence of a move.
fn is_io_error(err: &rusqlite::Error) -> bool {
    matches!(err.sqlite_error_code(), Some(rusqlite::ErrorCode::SystemIoFailure))
}

/// `err` reported as-is, naming `path`, SQLite's extended error code and the OS
/// errno behind it (`sqlite3_system_errno`).
fn io_error_message(conn: &rusqlite::Connection, err: &rusqlite::Error, path: &std::path::Path) -> String {
    let extended = err.sqlite_extended_error_code();
    // Safety: `conn.handle()` is a valid `sqlite3*` while `conn` is borrowed.
    let system_errno = unsafe { rusqlite::ffi::sqlite3_system_errno(conn.handle()) };
    format!(
        "skuld: coordination DB I/O error at {path:?}: {err} (extended code: {extended:?}, \
         system errno: {system_errno})"
    )
}
