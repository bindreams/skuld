//! Windows half of `check_usable`. No `access`-style query reflects ACLs, so the probe creates a
//! file in the directory with `FILE_DELETE_ON_CLOSE`: the file goes away when its handle closes,
//! even if the process is killed first. A taken name moves the probe on to the next one (see
//! `crate::win_nt`). Names embed the process id, which spares live processes each other's names.

use std::io;
use std::path::Path;

use windows::Wdk::Storage::FileSystem::{FILE_CREATE, FILE_DELETE_ON_CLOSE, FILE_SYNCHRONOUS_IO_NONALERT};
use windows::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_TEMPORARY, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_NONE,
};

use crate::win_nt::{create_unique, nt_create, open_dir};

/// Fail unless `dir` is a directory a file can be created in. Listing it is not checked: nothing
/// on Windows lists the coordination directory.
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
pub(super) fn probe(dir: &Path, next_name: impl FnMut() -> String) -> io::Result<()> {
    let handle = open_dir(dir)?;
    // A creator is granted the access it asks for on a new file, DELETE (which
    // FILE_DELETE_ON_CLOSE requires) included; SYNCHRONIZE (which FILE_SYNCHRONOUS_IO_NONALERT
    // requires) comes with FILE_GENERIC_READ. Dropping the handle deletes the file.
    create_unique(dir, &handle, next_name, |dir, name| {
        nt_create(
            dir,
            name,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
            FILE_ATTRIBUTE_TEMPORARY,
            FILE_SHARE_NONE,
            FILE_CREATE,
            FILE_DELETE_ON_CLOSE | FILE_SYNCHRONOUS_IO_NONALERT,
        )
    })
    .map(drop)
}
