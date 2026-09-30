//! Native (NT) file opens for Windows. Win32 folds `STATUS_DELETE_PENDING` into
//! `ERROR_ACCESS_DENIED`, indistinguishable from an ACL denial, so callers that must tell a name
//! held by a deleted entry from an unusable directory go through `NtCreateFile`.

#[cfg(test)]
pub(crate) mod test_support;

use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Component, Path, Prefix};

use windows::core::PWSTR;
use windows::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows::Wdk::Storage::FileSystem::{
    NtCreateFile, FILE_DIRECTORY_FILE, FILE_OPEN, NTCREATEFILE_CREATE_DISPOSITION, NTCREATEFILE_CREATE_OPTIONS,
};
use windows::Win32::Foundation::{
    RtlNtStatusToDosError, HANDLE, NTSTATUS, OBJ_CASE_INSENSITIVE, STATUS_DELETE_PENDING, STATUS_NAME_TOO_LONG,
    STATUS_NOT_A_DIRECTORY, UNICODE_STRING,
};
use windows::Win32::Storage::FileSystem::{
    FileStandardInfo, GetFileInformationByHandleEx, FILE_ACCESS_RIGHTS, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO,
};
use windows::Win32::System::IO::IO_STATUS_BLOCK;

/// Open the directory `dir` natively, so that the status of the open is available, and fail
/// unless it is a directory.
pub(crate) fn open_dir(dir: &Path) -> io::Result<File> {
    let full = std::path::absolute(dir).map_err(|e| io::Error::new(e.kind(), format!("{e}: {dir:?}")))?;
    // Without FILE_OPEN_FOR_BACKUP_INTENT, so the directory's ACL applies as it will to later opens.
    let opened = nt_create(
        None,
        &nt_path(&full)?,
        FILE_READ_ATTRIBUTES,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        FILE_OPEN,
        FILE_DIRECTORY_FILE,
    );
    let handle = File::from(opened.map_err(|status| match status {
        STATUS_DELETE_PENDING => io::Error::other(format!(
            "{full:?}, or a directory above it, is being deleted (NTSTATUS {:#010x})",
            status.0 as u32
        )),
        STATUS_NOT_A_DIRECTORY => not_a_directory(&full),
        _ => nt_error(status, &full),
    })?);
    // A device (`\\.\NUL`) can accept FILE_DIRECTORY_FILE without being a directory.
    let info = standard_info(&handle).map_err(|e| io::Error::new(e.kind(), format!("cannot query {full:?}: {e}")))?;
    if !info.Directory {
        return Err(not_a_directory(&full));
    }
    Ok(handle)
}

/// The NT object name Win32 gives the absolute path `full`: `\??\` followed by the path. A
/// verbatim (`\\?\`) path is taken literally, so each of its components must be a plain name.
fn nt_path(full: &Path) -> io::Result<Vec<u16>> {
    let invalid = |why: String| io::Error::new(io::ErrorKind::InvalidInput, format!("{full:?}: {why}"));
    let mut out: Vec<u16> = r"\??\".encode_utf16().collect();
    let mut components = full.components();
    match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => out.extend([u16::from(drive), u16::from(b':')]),
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                out.extend(r"UNC\".encode_utf16());
                out.extend(server.encode_wide());
                out.push(u16::from(b'\\'));
                out.extend(share.encode_wide());
            }
            Prefix::Verbatim(name) | Prefix::DeviceNS(name) => out.extend(name.encode_wide()),
        },
        _ => return Err(invalid("not an absolute path with a prefix".into())),
    }
    let (mut rooted, mut named) = (false, false);
    for component in components {
        match component {
            Component::RootDir => rooted = true,
            Component::Normal(name) => {
                if name.encode_wide().any(|c| c == u16::from(b'/')) {
                    return Err(invalid(format!("{name:?} is not a single path component")));
                }
                out.push(u16::from(b'\\'));
                out.extend(name.encode_wide());
                named = true;
            }
            Component::CurDir | Component::ParentDir => {
                return Err(invalid("a verbatim path cannot contain `.` or `..`".into()))
            }
            Component::Prefix(_) => unreachable!("only a path's first component is a prefix"),
        }
    }
    if rooted && !named {
        out.push(u16::from(b'\\'));
    }
    Ok(out)
}

/// `NtCreateFile` of `name`, relative to the directory `root` is a handle to, or absolute.
pub(crate) fn nt_create(
    root: Option<&File>,
    name: &[u16],
    access: FILE_ACCESS_RIGHTS,
    attributes: FILE_FLAGS_AND_ATTRIBUTES,
    share: FILE_SHARE_MODE,
    disposition: NTCREATEFILE_CREATE_DISPOSITION,
    options: NTCREATEFILE_CREATE_OPTIONS,
) -> Result<OwnedHandle, NTSTATUS> {
    let len = u16::try_from(name.len() * 2).map_err(|_| STATUS_NAME_TOO_LONG)?;
    let object_name = UNICODE_STRING {
        Length: len,
        MaximumLength: len,
        // NtCreateFile does not write through ObjectName.
        Buffer: PWSTR(name.as_ptr().cast_mut()),
    };
    let object_attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: root.map_or(HANDLE::default(), |r| HANDLE(r.as_raw_handle())),
        ObjectName: &object_name,
        // As Win32's CreateFileW does.
        Attributes: OBJ_CASE_INSENSITIVE,
        ..Default::default()
    };
    let mut handle = HANDLE::default();
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: every pointer refers to a local that outlives the call, and `root` is an open handle.
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

pub(crate) fn standard_info(file: &File) -> io::Result<FILE_STANDARD_INFO> {
    let mut info = FILE_STANDARD_INFO::default();
    // SAFETY: `info` is a FILE_STANDARD_INFO, the type FileStandardInfo fills, and outlives the call.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileStandardInfo,
            (&raw mut info).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    }?;
    Ok(info)
}

fn not_a_directory(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::NotADirectory, format!("{path:?} is not a directory"))
}

/// The Win32 error for `status`, naming the NTSTATUS and `path`.
pub(crate) fn nt_error(status: NTSTATUS, path: &Path) -> io::Error {
    // SAFETY: no preconditions; takes any NTSTATUS.
    let e = io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32);
    io::Error::new(
        e.kind(),
        format!("{e} (NTSTATUS {:#010x}) at path {path:?}", status.0 as u32),
    )
}
