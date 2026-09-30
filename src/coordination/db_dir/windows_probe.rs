//! Windows half of `check_usable`. No `access`-style query reflects ACLs, so the probe creates a
//! file in the directory with `FILE_DELETE_ON_CLOSE`: the file goes away when its handle closes,
//! even if the process is killed first.
//!
//! Opens are native (`crate::win_nt`), so `STATUS_DELETE_PENDING` stays distinguishable from an
//! ACL denial. A taken name (`STATUS_OBJECT_NAME_COLLISION`, or
//! `STATUS_DELETE_PENDING` for an entry deleted but still open elsewhere) does not make the
//! directory unusable; the probe moves to the next name. `STATUS_DELETE_PENDING` also means the
//! directory itself is being deleted, which no name gets past, so the directory's own
//! delete-pending flag decides between the two.

use std::fs::File;
use std::io;
use std::os::windows::io::{IntoRawHandle, OwnedHandle};
use std::path::Path;

use windows::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DELETE_ON_CLOSE, FILE_OPEN, FILE_SYNCHRONOUS_IO_NONALERT, NTCREATEFILE_CREATE_OPTIONS,
};
use windows::Win32::Foundation::{
    CloseHandle, HANDLE, STATUS_DELETE_PENDING, STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_NOT_FOUND,
};
use windows::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_TEMPORARY, FILE_FLAGS_AND_ATTRIBUTES, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_NONE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

use crate::coordination::skuld_debug_eprintln;
use crate::win_nt::{nt_create, nt_error, open_dir, standard_info};

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
