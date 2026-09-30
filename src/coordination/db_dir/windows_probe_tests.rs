use super::resolve;
use super::windows_probe::{check_usable, probe, probe_names};
use crate::win_nt::open_dir;
use crate::win_nt::test_support::mark_for_deletion;
use std::cell::{Cell, RefCell};
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::Command;
use windows::core::PWSTR;
use windows::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows::Wdk::Storage::FileSystem::{NtCreateFile, FILE_OPEN};
use windows::Win32::Foundation::{
    HANDLE, NTSTATUS, OBJ_CASE_INSENSITIVE, STATUS_DELETE_PENDING, STATUS_OBJECT_NAME_NOT_FOUND, UNICODE_STRING,
};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAGS_AND_ATTRIBUTES, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows::Win32::System::IO::IO_STATUS_BLOCK;

/// The status of opening the existing entry at `path` (an ordinary drive path) natively.
fn open_status(path: &Path) -> NTSTATUS {
    let mut wide: Vec<u16> = OsStr::new(r"\??\")
        .encode_wide()
        .chain(path.as_os_str().encode_wide())
        .collect();
    let len = u16::try_from(wide.len() * 2).unwrap();
    let name = UNICODE_STRING {
        Length: len,
        MaximumLength: len,
        Buffer: PWSTR(wide.as_mut_ptr()),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        ObjectName: &name,
        Attributes: OBJ_CASE_INSENSITIVE,
        ..Default::default()
    };
    let mut handle = HANDLE::default();
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: every pointer refers to a local that outlives the call.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            FILE_READ_ATTRIBUTES,
            &attributes,
            &mut io_status,
            None,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            Default::default(),
            None,
            0,
        )
    };
    if status.is_ok() {
        // SAFETY: NtCreateFile succeeded, so `handle` is open and ours.
        drop(unsafe { OwnedHandle::from_raw_handle(handle.0) });
    }
    status
}

/// The probe file at `path` is gone: absent, or deleted and waiting only for someone else's handle
/// (an antivirus scanner, say) to close.
fn assert_gone(path: &Path) {
    let status = open_status(path);
    assert!(
        status == STATUS_OBJECT_NAME_NOT_FOUND || status == STATUS_DELETE_PENDING,
        "{path:?} must be absent or delete-pending, but opening it gave NTSTATUS {:#010x}",
        status.0 as u32
    );
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
    let tmp = crate::TempDir::new().unwrap();
    let held = tmp.path().join("held");
    std::fs::write(&held, b"").unwrap();
    let _pending = mark_for_deletion(&held, false);
    assert_eq!(open_status(&held), STATUS_DELETE_PENDING, "precondition");

    let asked = Cell::new(0);
    probe(tmp.path(), names(&["held", "fresh"], &asked)).unwrap();
    assert_eq!(asked.get(), 2);
    assert_gone(&tmp.path().join("fresh"));
}

#[test]
fn a_name_held_by_an_existing_file_is_skipped() {
    let tmp = crate::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("taken"), b"keep").unwrap();

    let asked = Cell::new(0);
    probe(tmp.path(), names(&["taken", "fresh"], &asked)).unwrap();
    assert_eq!(asked.get(), 2);
    assert_eq!(std::fs::read(tmp.path().join("taken")).unwrap(), b"keep");
    assert_gone(&tmp.path().join("fresh"));
}

#[test]
fn a_name_held_by_a_directory_is_skipped() {
    let tmp = crate::TempDir::new().unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();

    let asked = Cell::new(0);
    probe(tmp.path(), names(&["sub", "fresh"], &asked)).unwrap();
    assert_eq!(asked.get(), 2);
    assert!(tmp.path().join("sub").is_dir());
    assert_gone(&tmp.path().join("fresh"));
}

fn assert_says_being_deleted(err: &str) {
    assert!(
        err.contains("being deleted") && !err.contains("Access is denied"),
        "the error must name the deletion, not an access denial: {err}"
    );
}

/// Every name fails in a delete-pending directory, so retrying on its status would never end.
fn a_directory_deleted_mid_probe_is_an_error_not_a_retry(posix: bool) {
    let tmp = crate::TempDir::new().unwrap();
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
    let tmp = crate::TempDir::new().unwrap();
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
        let out = Command::new("icacls")
            .arg(&self.0)
            .args(["/remove:d", "*S-1-1-0"])
            .output();
        if out.as_ref().is_ok_and(|o| o.status.success()) {
            return;
        }
        let msg = format!("could not remove the deny ACE from {:?}: {out:?}", self.0);
        if std::thread::panicking() {
            eprintln!("{msg}");
        } else {
            panic!("{msg}");
        }
    }
}

#[test]
fn a_write_denied_directory_is_an_error_not_a_retry() {
    let tmp = crate::TempDir::new().unwrap();
    let dir = tmp.path().join("d");
    std::fs::create_dir(&dir).unwrap();
    let _deny = DenyWrite::new(&dir);

    let asked = Cell::new(0);
    let err = probe(&dir, names(&["p"], &asked)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
    assert_eq!(asked.get(), 1);
}

#[test]
fn a_write_denied_directory_fails_resolve_naming_the_variable() {
    let tmp = crate::TempDir::new().unwrap();
    let dir = tmp.path().join("d");
    std::fs::create_dir(&dir).unwrap();
    let _deny = DenyWrite::new(&dir);

    let err = resolve(Some(dir.as_os_str()), || panic!("SKULD_DB_DIR is set")).unwrap_err();
    assert!(
        err.contains("SKULD_DB_DIR") && err.contains(&format!("{dir:?}")),
        "{err}"
    );
    assert!(!err.contains("being deleted"), "{err}");
}

// Production names -----

#[test]
fn probe_names_are_pid_scoped_and_advance() {
    let pid = std::process::id();
    let mut next = probe_names();
    assert_eq!(next(), format!(".skuld-probe-{pid}-1"));
    assert_eq!(next(), format!(".skuld-probe-{pid}-2"));
}

#[test]
fn check_usable_succeeds_and_its_probe_is_gone() {
    let tmp = crate::TempDir::new().unwrap();
    check_usable(tmp.path()).unwrap();
    assert_gone(&tmp.path().join(format!(".skuld-probe-{}-1", std::process::id())));
}

#[test]
fn check_usable_skips_a_leftover_probe_name() {
    let tmp = crate::TempDir::new().unwrap();
    let pid = std::process::id();
    let leftover = tmp.path().join(format!(".skuld-probe-{pid}-1"));
    std::fs::write(&leftover, b"keep").unwrap();

    check_usable(tmp.path()).unwrap();
    assert_eq!(std::fs::read(&leftover).unwrap(), b"keep");
    assert_gone(&tmp.path().join(format!(".skuld-probe-{pid}-2")));
}

// Path shapes -----

#[test]
fn a_relative_path_opens_against_the_working_directory() {
    open_dir(Path::new(".")).unwrap();
}

#[test]
fn a_drive_root_opens_as_a_directory() {
    let tmp = crate::TempDir::new().unwrap();
    let root = tmp.path().ancestors().last().unwrap();
    open_dir(root).unwrap();
}

#[test]
fn a_path_ending_in_parent_dir_probes_the_parent() {
    let tmp = crate::TempDir::new().unwrap();
    std::fs::create_dir(tmp.path().join("a")).unwrap();

    let asked = Cell::new(0);
    probe(&tmp.path().join("a").join(".."), names(&["p"], &asked)).unwrap();
    assert_gone(&tmp.path().join("p"));
    assert_gone(&tmp.path().join("a").join("p"));
}

#[test]
fn a_trailing_separator_is_accepted() {
    let tmp = crate::TempDir::new().unwrap();
    let mut dir = tmp.path().as_os_str().to_owned();
    dir.push("\\");

    let asked = Cell::new(0);
    probe(Path::new(&dir), names(&["p"], &asked)).unwrap();
    assert_gone(&tmp.path().join("p"));
}

#[test]
fn a_verbatim_path_with_a_slash_in_a_component_is_invalid_input() {
    let tmp = crate::TempDir::new().unwrap();
    let dir = PathBuf::from(format!(r"\\?\{}\a/b", tmp.path().display()));

    let err = check_usable(&dir).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
    assert!(err.to_string().contains("a/b"), "{err}");
}

#[test]
fn a_missing_directory_is_not_found_naming_it() {
    let tmp = crate::TempDir::new().unwrap();
    let dir = tmp.path().join("missing");

    let err = check_usable(&dir).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
    assert!(err.to_string().contains(&format!("{dir:?}")), "{err}");
}

#[test]
fn a_device_is_not_a_usable_directory() {
    for dev in [r"\\.\NUL", r"\\.\NUL\x"] {
        let err = resolve(Some(OsStr::new(dev)), || panic!("SKULD_DB_DIR is set")).unwrap_err();
        assert!(err.contains("SKULD_DB_DIR"), "{dev}: {err}");
    }
}

#[test]
fn a_directory_inside_one_being_deleted_says_so() {
    let tmp = crate::TempDir::new().unwrap();
    let parent = tmp.path().join("d");
    std::fs::create_dir(&parent).unwrap();
    let _pending = mark_for_deletion(&parent, false);

    let err = check_usable(&parent.join("sub")).unwrap_err().to_string();
    assert_says_being_deleted(&err);
}
