//! Test helpers for code built on `crate::win_nt`.

use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    FileDispositionInfo, FileDispositionInfoEx, SetFileInformationByHandle, DELETE, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX,
    FILE_DISPOSITION_INFO_EX_FLAGS, FILE_FLAG_BACKUP_SEMANTICS,
};

/// Classic semantics keep the name, delete-pending, until the last handle closes; POSIX
/// semantics unlink it when the returned handle closes.
pub(crate) fn mark_for_deletion(path: &Path, posix: bool) -> File {
    let f = OpenOptions::new()
        .access_mode(DELETE.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(path)
        .unwrap();
    let h = HANDLE(f.as_raw_handle());
    // SAFETY: `h` is a live handle opened with DELETE access; each info struct outlives its call.
    unsafe {
        if posix {
            let info = FILE_DISPOSITION_INFO_EX {
                Flags: FILE_DISPOSITION_INFO_EX_FLAGS(
                    FILE_DISPOSITION_FLAG_DELETE.0 | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0,
                ),
            };
            SetFileInformationByHandle(
                h,
                FileDispositionInfoEx,
                (&raw const info).cast(),
                size_of_val(&info) as u32,
            )
        } else {
            let info = FILE_DISPOSITION_INFO { DeleteFile: true };
            SetFileInformationByHandle(
                h,
                FileDispositionInfo,
                (&raw const info).cast(),
                size_of_val(&info) as u32,
            )
        }
    }
    .unwrap();
    f
}
