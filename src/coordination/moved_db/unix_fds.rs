//! Unix: the file descriptors SQLite itself holds open for a connection.
//!
//! SQLite has no file-control that returns an fd, but the unix VFS's file
//! objects are `unixFile` structs whose `h` field is the fd, and
//! `SQLITE_FCNTL_FILE_POINTER` / `SQLITE_FCNTL_JOURNAL_POINTER` hand out the
//! `sqlite3_file*` of the main and WAL files. The `-shm` fd is reachable from
//! the main file through `unixFile.pShm -> unixShm.pShmNode -> unixShmNode.hShm`.
//! Reading them means mirroring the leading fields of three private structs of
//! the bundled SQLite (3.53.2, `src/os_unix.c`): the fields mirrored here are
//! preceded by no conditionally-compiled field, so the layout does not depend
//! on build options.
//!
//! Every read is validated before it is trusted, so a SQLite upgrade that moves
//! a field fails loudly instead of returning garbage:
//! - the VFS must be one of the `unix*` VFSes, whose files are `unixFile`;
//! - each file's own name field (`zPath`, `zFilename`) must equal the name
//!   SQLite reports for it (`conn.path()`, plus `-wal` / `-shm`);
//! - `bundled_sqlite_is_the_version_whose_unix_layout_is_mirrored` pins the
//!   version, and `the_connections_fds_are_the_files_sqlite_created` pins that
//!   each fd is the file at its path.

use std::ffi::{c_char, c_int, c_void, CStr};

/// `unixFile`, up to `pShm`.
#[repr(C)]
struct UnixFile {
    p_method: *const c_void,
    p_vfs: *const c_void,
    p_inode: *const c_void,
    h: c_int,
    e_file_lock: u8,
    ctrl_flags: u16,
    last_errno: c_int,
    locking_context: *const c_void,
    p_preallocated_unused: *const c_void,
    z_path: *const c_char,
    p_shm: *const UnixShm,
}

/// `unixShm`, up to `pShmNode`.
#[repr(C)]
struct UnixShm {
    p_shm_node: *const UnixShmNode,
}

/// `unixShmNode`, up to `hShm`.
#[repr(C)]
struct UnixShmNode {
    p_inode: *const c_void,
    p_shm_mutex: *const c_void,
    z_filename: *const c_char,
    h_shm: c_int,
}

/// The fd of `conn`'s main database file.
pub(in crate::coordination) fn main_fd(conn: &rusqlite::Connection) -> c_int {
    let file = file_pointer(conn, rusqlite::ffi::SQLITE_FCNTL_FILE_POINTER, "");
    file.h
}

/// The fd of `conn`'s `-wal` file. The connection must be in WAL mode with the
/// WAL open (any statement after `PRAGMA journal_mode = WAL` has run).
pub(in crate::coordination) fn wal_fd(conn: &rusqlite::Connection) -> c_int {
    let file = file_pointer(conn, rusqlite::ffi::SQLITE_FCNTL_JOURNAL_POINTER, "-wal");
    file.h
}

/// The fd of the `-shm` file `conn` uses. In-process connections to one
/// database share a single `-shm` fd, opened by whichever came first.
pub(in crate::coordination) fn shm_fd(conn: &rusqlite::Connection) -> c_int {
    let file = file_pointer(conn, rusqlite::ffi::SQLITE_FCNTL_FILE_POINTER, "");
    assert!(
        !file.p_shm.is_null(),
        "skuld: the connection has no shared-memory file open — expected once WAL mode is active"
    );
    // Safety: `pShm` is non-null and points to the connection's live `unixShm`.
    let shm = unsafe { &*file.p_shm };
    assert!(!shm.p_shm_node.is_null(), "skuld: unixShm has no unixShmNode");
    // Safety: as above; the node outlives every `unixShm` pointing at it.
    let node = unsafe { &*shm.p_shm_node };
    check_name(node.z_filename, conn, "-shm", "unixShmNode.zFilename");
    node.h_shm
}

/// The `unixFile` behind `op`, validated against the name `conn` reports plus
/// `suffix`.
fn file_pointer<'c>(conn: &'c rusqlite::Connection, op: c_int, suffix: &str) -> &'c UnixFile {
    assert_unix_vfs(conn);
    let mut file: *mut rusqlite::ffi::sqlite3_file = std::ptr::null_mut();
    let main = c"main";
    // Safety: `conn.handle()` is a valid `sqlite3*` while `conn` is borrowed;
    // `main` is NUL-terminated; `&mut file` is a valid `*mut *mut sqlite3_file`,
    // which is what these opcodes write through.
    let rc = unsafe { rusqlite::ffi::sqlite3_file_control(conn.handle(), main.as_ptr(), op, (&raw mut file).cast()) };
    assert_eq!(
        rc,
        rusqlite::ffi::SQLITE_OK,
        "skuld: file-control {op} failed with code {rc}"
    );
    assert!(
        !file.is_null(),
        "skuld: file-control {op} returned no file — the WAL must be open"
    );
    // Safety: non-null `sqlite3_file*` owned by `conn`, valid while it is borrowed.
    assert!(
        !unsafe { (*file).pMethods }.is_null(),
        "skuld: file-control {op} returned a file that is not open"
    );
    // Safety: the unix VFS (checked above) allocates every file as a `unixFile`
    // and `sqlite3_file` is its first member.
    let unix_file = unsafe { &*(file as *const UnixFile) };
    check_name(unix_file.z_path, conn, suffix, "unixFile.zPath");
    unix_file
}

/// Panic unless `conn` uses a VFS whose files are `unixFile`s.
fn assert_unix_vfs(conn: &rusqlite::Connection) {
    let mut name: *mut c_char = std::ptr::null_mut();
    let main = c"main";
    // Safety: as in `file_pointer`; `SQLITE_FCNTL_VFSNAME` writes a
    // `sqlite3_malloc`ed string (or null) through `&mut name`.
    let rc = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            conn.handle(),
            main.as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_VFSNAME,
            (&raw mut name).cast(),
        )
    };
    assert_eq!(
        rc,
        rusqlite::ffi::SQLITE_OK,
        "skuld: SQLITE_FCNTL_VFSNAME failed with code {rc}"
    );
    assert!(!name.is_null(), "skuld: SQLITE_FCNTL_VFSNAME returned no name");
    // Safety: a NUL-terminated string that SQLite allocated.
    let vfs = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
    // Safety: `name` came from `sqlite3_malloc` and is not used again.
    unsafe { rusqlite::ffi::sqlite3_free(name.cast()) };
    assert!(
        vfs.starts_with("unix"),
        "skuld: the coordination DB connection uses the {vfs:?} VFS, not a unix one, so its file \
         descriptors cannot be read"
    );
}

/// Panic unless the name field `z` equals `conn.path()` plus `suffix`: the
/// canary that the mirrored layout still matches SQLite's.
fn check_name(z: *const c_char, conn: &rusqlite::Connection, suffix: &str, field: &str) {
    let expected = format!("{}{suffix}", conn.path().unwrap_or("<no path>"));
    assert!(
        !z.is_null(),
        "skuld: {field} is null: the mirrored SQLite unix layout no longer matches"
    );
    // Safety: a non-null NUL-terminated string owned by SQLite, per the struct
    // definition; a wrong layout is what the comparison below catches.
    let actual = unsafe { CStr::from_ptr(z) }.to_string_lossy();
    assert_eq!(
        actual, expected,
        "skuld: {field} is {actual:?}, expected {expected:?}: the bundled SQLite's unix file layout no \
         longer matches the one mirrored in unix_fds.rs"
    );
}
