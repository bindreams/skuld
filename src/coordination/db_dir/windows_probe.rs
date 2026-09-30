//! Windows half of `check_usable`. No `access`-style query reflects ACLs, so the probe creates a
//! file in the directory, relative to a handle on it, with `FILE_DELETE_ON_CLOSE`: the file goes
//! away when its handle closes, even if the process is killed first.
//!
//! Both opens are native, because Win32 reports `STATUS_DELETE_PENDING` as `ERROR_ACCESS_DENIED`,
//! indistinguishable from an ACL denial. A name can be taken without the directory being unusable,
//! and the probe then moves on to the next one:
//!
//! - `STATUS_OBJECT_NAME_COLLISION`: an entry by that name exists;
//! - `STATUS_DELETE_PENDING`: an entry by that name is deleted but still open somewhere.
//!
//! `STATUS_DELETE_PENDING` also means the directory itself is being deleted, which no name gets
//! past, so the directory's own delete-pending flag decides between the two. Any other status
//! fails the check. Names embed the process id, which spares live processes each other's names.

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
    NtCreateFile, FILE_CREATE, FILE_DELETE_ON_CLOSE, FILE_DIRECTORY_FILE, FILE_OPEN, FILE_SYNCHRONOUS_IO_NONALERT,
    NTCREATEFILE_CREATE_DISPOSITION, NTCREATEFILE_CREATE_OPTIONS,
};
use windows::Win32::Foundation::{
    RtlNtStatusToDosError, HANDLE, NTSTATUS, OBJ_CASE_INSENSITIVE, STATUS_DELETE_PENDING, STATUS_NOT_A_DIRECTORY,
    STATUS_OBJECT_NAME_COLLISION, UNICODE_STRING,
};
use windows::Win32::Storage::FileSystem::{
    FileStandardInfo, GetFileInformationByHandleEx, DELETE, FILE_ACCESS_RIGHTS, FILE_ATTRIBUTE_TEMPORARY,
    FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_NONE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO,
};
use windows::Win32::System::IO::IO_STATUS_BLOCK;

/// Fail unless `dir` is a directory a file can be created in. Listing it is not checked: nothing
/// on Windows lists the coordination directory.
pub(super) fn check_usable(dir: &Path) -> io::Result<()> {
    probe(dir, probe_names())
}

/// This process's probe names: `.skuld-probe-<pid>-<n>` for n = 1, 2, ...
pub(super) fn probe_names() -> impl FnMut() -> String {
    let pid = std::process::id();
    let mut n = 0u64;
    move || {
        n += 1;
        format!(".skuld-probe-{pid}-{n}")
    }
}

/// [`check_usable`] with the probe names drawn from `next_name`, which is called only after `dir`
/// is open.
pub(super) fn probe(dir: &Path, mut next_name: impl FnMut() -> String) -> io::Result<()> {
    let handle = open_dir(dir)?;
    loop {
        let name = next_name();
        let created = nt_create(
            &handle,
            OsStr::new(&name),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
            FILE_ATTRIBUTE_TEMPORARY,
            FILE_SHARE_NONE,
            FILE_CREATE,
            FILE_DELETE_ON_CLOSE | FILE_SYNCHRONOUS_IO_NONALERT,
        );
        match created {
            Ok(file) => {
                drop(file);
                return Ok(());
            }
            Err(status) if status == STATUS_OBJECT_NAME_COLLISION => continue,
            Err(status) if status == STATUS_DELETE_PENDING => match delete_pending(&handle)? {
                false => continue,
                true => return Err(being_deleted(status)),
            },
            Err(status) => return Err(nt_error(status, &dir.join(&name))),
        }
    }
}

/// Open the directory `dir`, relative to its parent so that the status of the open is available.
pub(super) fn open_dir(dir: &Path) -> io::Result<File> {
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
    // Without FILE_OPEN_FOR_BACKUP_INTENT, so the directory's ACL applies as it will to the database.
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

/// `NtCreateFile` of `name` relative to the directory `root` is a handle to.
fn nt_create(
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
    // A creator is granted the access it asks for on a new file, DELETE (which FILE_DELETE_ON_CLOSE
    // requires) included; SYNCHRONIZE (which FILE_SYNCHRONOUS_IO_NONALERT requires) comes with
    // FILE_GENERIC_READ.
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
