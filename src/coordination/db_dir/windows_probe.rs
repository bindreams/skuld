//! Windows half of `check_usable`. No `access`-style query reflects ACLs, so the probe creates a
//! file in the directory with `FILE_DELETE_ON_CLOSE`: the file goes away when its handle closes,
//! even if the process is killed first.
//!
//! Both opens are native, because Win32 folds `STATUS_DELETE_PENDING` into `ERROR_ACCESS_DENIED`,
//! indistinguishable from an ACL denial. A taken name (`STATUS_OBJECT_NAME_COLLISION`, or
//! `STATUS_DELETE_PENDING` for an entry deleted but still open elsewhere) does not make the
//! directory unusable; the probe moves to the next name. `STATUS_DELETE_PENDING` also means the
//! directory itself is being deleted, which no name gets past, so the directory's own
//! delete-pending flag decides between the two.

use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::path::{Component, Path, Prefix};

use windows::core::PWSTR;
use windows::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows::Wdk::Storage::FileSystem::{
    NtCreateFile, FILE_CREATE, FILE_DELETE_ON_CLOSE, FILE_DIRECTORY_FILE, FILE_OPEN, FILE_SYNCHRONOUS_IO_NONALERT,
    NTCREATEFILE_CREATE_DISPOSITION, NTCREATEFILE_CREATE_OPTIONS,
};
use windows::Win32::Foundation::{
    CloseHandle, RtlNtStatusToDosError, HANDLE, NTSTATUS, OBJ_CASE_INSENSITIVE, STATUS_DELETE_PENDING,
    STATUS_NAME_TOO_LONG, STATUS_NOT_A_DIRECTORY, STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_NOT_FOUND,
    UNICODE_STRING,
};
use windows::Win32::Storage::FileSystem::{
    FileStandardInfo, GetFileInformationByHandleEx, DELETE, FILE_ACCESS_RIGHTS, FILE_ATTRIBUTE_TEMPORARY,
    FILE_FLAGS_AND_ATTRIBUTES, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_MODE, FILE_SHARE_NONE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO,
};
use windows::Win32::System::IO::IO_STATUS_BLOCK;

use crate::coordination::skuld_debug_eprintln;

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
        debug_assert!(
            !name.is_empty() && !name.contains(['\\', '/']),
            "probe name {name:?} must be a single path component"
        );
        let wide: Vec<u16> = name.encode_utf16().collect();
        // A creator is granted the access it asks for on a new file, DELETE (which
        // FILE_DELETE_ON_CLOSE requires) included; SYNCHRONIZE (which FILE_SYNCHRONOUS_IO_NONALERT
        // requires) comes with FILE_GENERIC_READ.
        let created = nt_create(
            Some(&handle),
            &wide,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
            FILE_ATTRIBUTE_TEMPORARY,
            FILE_SHARE_NONE,
            FILE_CREATE,
            FILE_DELETE_ON_CLOSE | FILE_SYNCHRONOUS_IO_NONALERT,
        );
        let status = match created {
            Ok(file) => {
                close_probe(file, &handle, &wide, &dir.join(&name));
                return Ok(());
            }
            Err(status) => status,
        };
        if status == STATUS_OBJECT_NAME_COLLISION {
            continue;
        }
        if status != STATUS_DELETE_PENDING {
            return Err(nt_error(status, &dir.join(&name)));
        }
        match standard_info(&handle) {
            Ok(info) if !info.DeletePending => continue,
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "the directory is being deleted (NTSTATUS {:#010x})",
                    status.0 as u32
                )))
            }
            Err(e) => {
                return Err(io::Error::new(
                    e.kind(),
                    format!(
                        "creating {:?} gave NTSTATUS {:#010x}, and asking whether {dir:?} is being deleted failed: {e}",
                        dir.join(&name),
                        status.0 as u32
                    ),
                ))
            }
        }
    }
}

/// Close the probe file, which deletes it. Neither step fails for a file this process just
/// created in a directory it holds open, so a failure is logged and debug-asserted, not returned.
fn close_probe(file: OwnedHandle, dir: &File, name: &[u16], path: &Path) {
    // SAFETY: `file` is open, and ownership of the handle passes to CloseHandle.
    if let Err(e) = unsafe { CloseHandle(HANDLE(file.into_raw_handle())) } {
        skuld_debug_eprintln!("closing probe file {path:?} failed: {e}");
        debug_assert!(false, "closing probe file {path:?} failed: {e}");
        return;
    }
    // Deleted means absent, or delete-pending while someone else (an antivirus scanner, say) still
    // holds it.
    let reopened = nt_create(
        Some(dir),
        name,
        FILE_READ_ATTRIBUTES,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        FILE_OPEN,
        NTCREATEFILE_CREATE_OPTIONS(0),
    );
    match reopened {
        Err(status) if status == STATUS_OBJECT_NAME_NOT_FOUND || status == STATUS_DELETE_PENDING => {}
        other => {
            let seen = other.map(drop);
            skuld_debug_eprintln!("probe file {path:?} was not deleted on close: {seen:?}");
            debug_assert!(false, "probe file {path:?} was not deleted on close: {seen:?}");
        }
    }
}

/// Open the directory `dir` natively, so that the status of the open is available, and fail
/// unless it is a directory.
pub(super) fn open_dir(dir: &Path) -> io::Result<File> {
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
        STATUS_NOT_A_DIRECTORY => not_a_directory(),
        _ => nt_error(status, &full),
    })?);
    // A device (`\\.\NUL`) can accept FILE_DIRECTORY_FILE without being a directory.
    let info = standard_info(&handle).map_err(|e| io::Error::new(e.kind(), format!("cannot query {full:?}: {e}")))?;
    if !info.Directory {
        return Err(not_a_directory());
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
fn nt_create(
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

fn standard_info(file: &File) -> io::Result<FILE_STANDARD_INFO> {
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

fn not_a_directory() -> io::Error {
    io::Error::new(io::ErrorKind::NotADirectory, "not a directory")
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
