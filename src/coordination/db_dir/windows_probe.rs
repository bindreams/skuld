//! Windows half of `check_usable`. No `access`-style query reflects ACLs, so the probe creates a
//! file in the directory, relative to a handle on it, with `FILE_DELETE_ON_CLOSE`: the file goes
//! away when its handle closes, even if the process is killed first.
//!
//! A name can be taken without the directory being unusable, and the probe then moves on to the
//! next one:
//!
//! - `STATUS_OBJECT_NAME_COLLISION`: a file by that name exists;
//! - `STATUS_DELETE_PENDING`: a file by that name is deleted but still open somewhere. Win32 reports
//!   this as `ERROR_ACCESS_DENIED`, indistinguishable from an ACL denial, hence the native call.
//!
//! `STATUS_DELETE_PENDING` also means the directory itself is being deleted, which no name gets
//! past, so the directory's own delete-pending flag decides between the two. Any other status
//! fails the check. Names embed the process id, so no two live processes probe the same name.

use std::fs::File;
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows::core::PWSTR;
use windows::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows::Wdk::Storage::FileSystem::{
    NtCreateFile, FILE_CREATE, FILE_DELETE_ON_CLOSE, FILE_NON_DIRECTORY_FILE, FILE_SYNCHRONOUS_IO_NONALERT,
};
use windows::Win32::Foundation::{
    RtlNtStatusToDosError, HANDLE, NTSTATUS, OBJ_CASE_INSENSITIVE, STATUS_DELETE_PENDING, STATUS_OBJECT_NAME_COLLISION,
    UNICODE_STRING,
};
use windows::Win32::Storage::FileSystem::{
    FileStandardInfo, GetFileInformationByHandleEx, DELETE, FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY, FILE_SHARE_NONE, FILE_STANDARD_INFO, SYNCHRONIZE,
};
use windows::Win32::System::IO::IO_STATUS_BLOCK;

/// Fail unless `dir` can be listed and a file can be created in it.
pub(super) fn check_usable(dir: &Path) -> io::Result<()> {
    let pid = std::process::id();
    let mut n = 0u64;
    probe(dir, || {
        n += 1;
        format!(".skuld-probe-{pid}-{n}")
    })
}

/// [`check_usable`] with the probe names drawn from `next_name`, which is called only after `dir`
/// is open.
pub(super) fn probe(dir: &Path, mut next_name: impl FnMut() -> String) -> io::Result<()> {
    // The same access `read_dir` asks for; FILE_FLAG_BACKUP_SEMANTICS is how Win32 opens a directory.
    let handle = std::fs::OpenOptions::new()
        .access_mode((FILE_LIST_DIRECTORY | SYNCHRONIZE).0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(dir)?;
    loop {
        let name = next_name();
        match create_delete_on_close(&handle, &name) {
            Ok(()) => return Ok(()),
            Err(status) if status == STATUS_OBJECT_NAME_COLLISION => continue,
            Err(status) if status == STATUS_DELETE_PENDING && !delete_pending(&handle)? => continue,
            Err(status) => return Err(nt_error(status, &dir.join(&name))),
        }
    }
}

/// Create `name` in the directory `dir` is a handle to, and close it again, which deletes it.
fn create_delete_on_close(dir: &File, name: &str) -> Result<(), NTSTATUS> {
    debug_assert!(
        !name.is_empty() && !name.contains(['\\', '/']),
        "{name:?} must be a bare file name"
    );
    let mut wide: Vec<u16> = name.encode_utf16().collect();
    let len = u16::try_from(wide.len() * 2).expect("a probe name fits a UNICODE_STRING");
    let object_name = UNICODE_STRING {
        Length: len,
        MaximumLength: len,
        Buffer: PWSTR(wide.as_mut_ptr()),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: HANDLE(dir.as_raw_handle()),
        ObjectName: &object_name,
        // As Win32's CreateFileW does.
        Attributes: OBJ_CASE_INSENSITIVE,
        ..Default::default()
    };
    let mut file = HANDLE::default();
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: every pointer refers to a local that outlives the call; `dir` is an open directory
    // handle. The creator is granted DELETE, which FILE_DELETE_ON_CLOSE requires, on a new file.
    let status = unsafe {
        NtCreateFile(
            &mut file,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
            &attributes,
            &mut io_status,
            None,
            FILE_ATTRIBUTE_TEMPORARY,
            FILE_SHARE_NONE,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE | FILE_DELETE_ON_CLOSE | FILE_SYNCHRONOUS_IO_NONALERT,
            None,
            0,
        )
    };
    if status.is_err() {
        return Err(status);
    }
    // SAFETY: NtCreateFile succeeded, so `file` is a handle this function owns.
    drop(unsafe { OwnedHandle::from_raw_handle(file.0) });
    Ok(())
}

/// Whether the directory `dir` is a handle to is marked for deletion.
fn delete_pending(dir: &File) -> io::Result<bool> {
    let mut info = FILE_STANDARD_INFO::default();
    // SAFETY: `info` is a FILE_STANDARD_INFO, the type FileStandardInfo fills, and outlives the call.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(dir.as_raw_handle()),
            FileStandardInfo,
            (&raw mut info).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    }?;
    Ok(info.DeletePending)
}

/// The Win32 error for `status`, naming the NTSTATUS and the probe path.
fn nt_error(status: NTSTATUS, path: &Path) -> io::Error {
    // SAFETY: a pure table lookup.
    let e = io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32);
    io::Error::new(
        e.kind(),
        format!("{e} (NTSTATUS {:#010x}) at path {path:?}", status.0 as u32),
    )
}
