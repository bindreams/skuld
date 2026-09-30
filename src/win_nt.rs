//! Native (NT) file creation for Windows, for callers that must tell a taken name from an unusable
//! directory. Win32 reports `STATUS_DELETE_PENDING` as `ERROR_ACCESS_DENIED`, indistinguishable
//! from an ACL denial, so these go through `NtCreateFile`, relative to a directory handle.
//!
//! [`create_unique`] moves on to the next name when one is taken without the directory being
//! unusable:
//!
//! - `STATUS_OBJECT_NAME_COLLISION`: an entry by that name exists;
//! - `STATUS_DELETE_PENDING`: an entry by that name is deleted but still open somewhere.
//!
//! `STATUS_DELETE_PENDING` also means the directory itself is being deleted, which no name gets
//! past, so the directory's own delete-pending flag decides between the two. Any other status is an
//! error.

#[cfg(test)]
pub(crate) mod test_support;

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows::core::PWSTR;
use windows::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows::Wdk::Storage::FileSystem::{
    NtCreateFile, FILE_DIRECTORY_FILE, FILE_OPEN, NTCREATEFILE_CREATE_DISPOSITION, NTCREATEFILE_CREATE_OPTIONS,
};
use windows::Win32::Foundation::{
    RtlNtStatusToDosError, HANDLE, NTSTATUS, OBJ_CASE_INSENSITIVE, STATUS_DELETE_PENDING, STATUS_NOT_A_DIRECTORY,
    STATUS_OBJECT_NAME_COLLISION, UNICODE_STRING,
};
use windows::Win32::Storage::FileSystem::{
    FileStandardInfo, GetFileInformationByHandleEx, FILE_ACCESS_RIGHTS, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_STANDARD_INFO,
};
use windows::Win32::System::IO::IO_STATUS_BLOCK;

/// Open the directory `dir`, relative to its parent so that the status of the open is available.
pub(crate) fn open_dir(dir: &Path) -> io::Result<File> {
    let full = std::path::absolute(dir)?;
    let (Some(parent), Some(name)) = (full.parent(), full.file_name()) else {
        // A root has no parent to open relative to, and cannot be deleted.
        return std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES.0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
            .open(&full);
    };
    // Access 0: an open relative to a handle needs no access to it.
    let parent = std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(parent)?;
    // Without FILE_OPEN_FOR_BACKUP_INTENT, so the directory's ACL applies as it will to later opens.
    let opened = nt_create(
        &parent,
        name,
        FILE_READ_ATTRIBUTES,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        FILE_OPEN,
        FILE_DIRECTORY_FILE,
    );
    opened.map(File::from).map_err(|status| match status {
        STATUS_DELETE_PENDING => being_deleted(status),
        STATUS_NOT_A_DIRECTORY => io::Error::new(io::ErrorKind::NotADirectory, "not a directory"),
        _ => nt_error(status, &full),
    })
}

/// Call `create` in the directory `dir` (at `dir_path`, a handle from [`open_dir`]) with names from
/// `next_name` until one is not taken (module doc), and return that name and what `create` made.
pub(crate) fn create_unique<T>(
    dir_path: &Path,
    dir: &File,
    mut next_name: impl FnMut() -> String,
    mut create: impl FnMut(&File, &OsStr) -> Result<T, NTSTATUS>,
) -> io::Result<(String, T)> {
    loop {
        let name = next_name();
        match create(dir, OsStr::new(&name)) {
            Ok(made) => return Ok((name, made)),
            Err(status) if status == STATUS_OBJECT_NAME_COLLISION => continue,
            Err(status) if status == STATUS_DELETE_PENDING => match delete_pending(dir)? {
                false => continue,
                true => return Err(being_deleted(status)),
            },
            Err(status) => return Err(nt_error(status, &dir_path.join(&name))),
        }
    }
}

/// `NtCreateFile` of `name` relative to the directory `root` is a handle to.
pub(crate) fn nt_create(
    root: &File,
    name: &OsStr,
    access: FILE_ACCESS_RIGHTS,
    attributes: FILE_FLAGS_AND_ATTRIBUTES,
    share: FILE_SHARE_MODE,
    disposition: NTCREATEFILE_CREATE_DISPOSITION,
    options: NTCREATEFILE_CREATE_OPTIONS,
) -> Result<OwnedHandle, NTSTATUS> {
    let mut wide: Vec<u16> = name.encode_wide().collect();
    debug_assert!(
        !wide.is_empty() && !wide.iter().any(|&c| c == u16::from(b'\\') || c == u16::from(b'/')),
        "{name:?} must be a single path component"
    );
    let len = u16::try_from(wide.len() * 2).expect("a path component fits a UNICODE_STRING");
    let object_name = UNICODE_STRING {
        Length: len,
        MaximumLength: len,
        Buffer: PWSTR(wide.as_mut_ptr()),
    };
    let object_attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: HANDLE(root.as_raw_handle()),
        ObjectName: &object_name,
        // As Win32's CreateFileW does.
        Attributes: OBJ_CASE_INSENSITIVE,
        ..Default::default()
    };
    let mut handle = HANDLE::default();
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: every pointer refers to a local that outlives the call, and `root` is an open handle.
    // The access and options are the caller's to pair (e.g. FILE_SYNCHRONOUS_IO_NONALERT needs
    // SYNCHRONIZE); a mismatch is an error status, not undefined behaviour.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &object_attributes,
            &mut io_status,
            None,
            attributes,
            share,
            disposition,
            options,
            None,
            0,
        )
    };
    if status.is_err() {
        return Err(status);
    }
    // SAFETY: NtCreateFile succeeded, so `handle` is open and owned by nobody else.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
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

fn being_deleted(status: NTSTATUS) -> io::Error {
    io::Error::other(format!(
        "the directory is being deleted (NTSTATUS {:#010x})",
        status.0 as u32
    ))
}

/// The Win32 error for `status`, naming the NTSTATUS and `path`.
fn nt_error(status: NTSTATUS, path: &Path) -> io::Error {
    // SAFETY: no preconditions; takes any NTSTATUS.
    let e = io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32);
    io::Error::new(
        e.kind(),
        format!("{e} (NTSTATUS {:#010x}) at path {path:?}", status.0 as u32),
    )
}
