use super::resolve;
use super::windows_probe::probe;
use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::Command;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    FileDispositionInfo, FileDispositionInfoEx, SetFileInformationByHandle, DELETE, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX,
    FILE_DISPOSITION_INFO_EX_FLAGS, FILE_FLAG_BACKUP_SEMANTICS,
};

/// Opens `path` (a file or an empty directory) for DELETE and marks it for deletion on the returned
/// handle. Classic semantics keep the name, delete-pending, until the last handle closes; POSIX
/// semantics unlink it when the returned handle closes.
fn mark_for_deletion(path: &Path, posix: bool) -> File {
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

fn listing(dir: &Path) -> Vec<OsString> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect()
}

/// Hands out `names` in order and counts the calls; asking for more is a test failure, so a probe
/// that keeps retrying fails here instead of hanging.
fn names<'a>(names: &'a [&'a str], asked: &'a Cell<usize>) -> impl FnMut() -> String + 'a {
    move || {
        let i = asked.get();
        asked.set(i + 1);
        let Some(name) = names.get(i) else {
            panic!("the probe asked for name #{} after {names:?}", i + 1)
        };
        name.to_string()
    }
}

#[test]
fn a_name_held_by_a_delete_pending_file_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let held = tmp.path().join("held");
    std::fs::write(&held, b"").unwrap();
    let _pending = mark_for_deletion(&held, false);
    assert_eq!(
        listing(tmp.path()),
        ["held"],
        "precondition: the delete-pending name is still listed"
    );

    let asked = Cell::new(0);
    probe(tmp.path(), names(&["held", "fresh"], &asked)).unwrap();
    assert_eq!(asked.get(), 2);
    assert_eq!(
        listing(tmp.path()),
        ["held"],
        "the probe file must be gone once the probe returns"
    );
}

#[test]
fn a_name_held_by_an_existing_file_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("taken"), b"keep").unwrap();

    let asked = Cell::new(0);
    probe(tmp.path(), names(&["taken", "fresh"], &asked)).unwrap();
    assert_eq!(asked.get(), 2);
    assert_eq!(std::fs::read(tmp.path().join("taken")).unwrap(), b"keep");
    assert_eq!(
        listing(tmp.path()),
        ["taken"],
        "the probe file must be gone once the probe returns"
    );
}

#[test]
fn a_name_held_by_a_directory_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();

    let asked = Cell::new(0);
    probe(tmp.path(), names(&["sub", "fresh"], &asked)).unwrap();
    assert_eq!(asked.get(), 2);
    assert_eq!(listing(tmp.path()), ["sub"]);
}

fn assert_says_being_deleted(err: &str) {
    assert!(
        err.contains("being deleted") && !err.contains("Access is denied"),
        "the error must name the deletion, not an access denial: {err}"
    );
}

/// Every name fails in a delete-pending directory, so retrying on its status would never end.
fn a_directory_deleted_mid_probe_is_an_error_not_a_retry(posix: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("d");
    std::fs::create_dir(&dir).unwrap();

    let asked = Cell::new(0);
    let marker = RefCell::new(None);
    let mut next = names(&["p"], &asked);
    let err = probe(&dir, || {
        if asked.get() == 0 {
            let handle = mark_for_deletion(&dir, posix);
            if !posix {
                *marker.borrow_mut() = Some(handle);
            }
        }
        next()
    })
    .unwrap_err();
    assert_eq!(asked.get(), 1, "{err}");
    assert_says_being_deleted(&err.to_string());
}

#[test]
fn a_delete_pending_directory_is_an_error_not_a_retry() {
    a_directory_deleted_mid_probe_is_an_error_not_a_retry(false);
}

#[test]
fn a_posix_deleted_directory_is_an_error_not_a_retry() {
    a_directory_deleted_mid_probe_is_an_error_not_a_retry(true);
}

#[test]
fn a_directory_already_being_deleted_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("d");
    std::fs::create_dir(&dir).unwrap();
    let _pending = mark_for_deletion(&dir, false);

    let err = resolve(Some(dir.as_os_str()), || panic!("SKULD_DB_DIR is set")).unwrap_err();
    assert!(err.contains("SKULD_DB_DIR"), "{err}");
    assert_says_being_deleted(&err);
}

/// Denies file and subdirectory creation to Everyone; the deny ACE is removed on drop.
struct DenyWrite(PathBuf);
impl DenyWrite {
    fn new(p: &Path) -> Self {
        let out = Command::new("icacls")
            .arg(p)
            .args(["/deny", "*S-1-1-0:(OI)(CI)(WD,AD)"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        Self(p.to_path_buf())
    }
}
impl Drop for DenyWrite {
    fn drop(&mut self) {
        let _ = Command::new("icacls")
            .arg(&self.0)
            .args(["/remove:d", "*S-1-1-0"])
            .output();
    }
}

#[test]
fn a_write_denied_directory_is_an_error_not_a_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("d");
    std::fs::create_dir(&dir).unwrap();
    let _deny = DenyWrite::new(&dir);

    let asked = Cell::new(0);
    let err = probe(&dir, names(&["p"], &asked)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
    assert_eq!(asked.get(), 1);
}
