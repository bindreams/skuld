//! THROWAWAY: adversarial-review measurements for PR #92. Prints, does not gate.
use super::windows_probe::probe;
use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::process::Command;
use windows::core::PWSTR;
use windows::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows::Wdk::Storage::FileSystem::{
    NtCreateFile, FILE_CREATE, FILE_DELETE_ON_CLOSE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
    FILE_OPEN_FOR_BACKUP_INTENT, FILE_SYNCHRONOUS_IO_NONALERT, NTCREATEFILE_CREATE_DISPOSITION,
    NTCREATEFILE_CREATE_OPTIONS,
};
use windows::Win32::Foundation::{HANDLE, OBJ_CASE_INSENSITIVE, UNICODE_STRING};
use windows::Win32::Storage::FileSystem::{
    FileDispositionInfo, FileDispositionInfoEx, FileStandardInfo, GetFileInformationByHandleEx,
    SetFileInformationByHandle, DELETE, FILE_ACCESS_RIGHTS, FILE_ATTRIBUTE_TEMPORARY, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX,
    FILE_DISPOSITION_INFO_EX_FLAGS, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_STANDARD_INFO, SYNCHRONIZE,
};
use windows::Win32::System::IO::IO_STATUS_BLOCK;

fn open_dir(p: &Path, access: u32) -> File {
    OpenOptions::new()
        .access_mode(access)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(p)
        .unwrap()
}

fn mark(path: &Path, posix: bool) -> File {
    let f = open_dir(path, DELETE.0);
    let h = HANDLE(f.as_raw_handle());
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

/// Same call as the PR's create_delete_on_close, with the access mask as a parameter.
fn nt_create(dir: &File, name: &str, access: FILE_ACCESS_RIGHTS) -> u32 {
    nt_open(
        dir,
        name,
        access,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE | FILE_DELETE_ON_CLOSE | FILE_SYNCHRONOUS_IO_NONALERT,
    )
}

fn nt_open(
    dir: &File,
    name: &str,
    access: FILE_ACCESS_RIGHTS,
    disposition: NTCREATEFILE_CREATE_DISPOSITION,
    options: NTCREATEFILE_CREATE_OPTIONS,
) -> u32 {
    let mut wide: Vec<u16> = name.encode_utf16().collect();
    let len = (wide.len() * 2) as u16;
    let object_name = UNICODE_STRING {
        Length: len,
        MaximumLength: len,
        Buffer: PWSTR(wide.as_mut_ptr()),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: HANDLE(dir.as_raw_handle()),
        ObjectName: &object_name,
        Attributes: OBJ_CASE_INSENSITIVE,
        ..Default::default()
    };
    let mut file = HANDLE::default();
    let mut io_status = IO_STATUS_BLOCK::default();
    let status = unsafe {
        NtCreateFile(
            &mut file,
            access,
            &attributes,
            &mut io_status,
            None,
            FILE_ATTRIBUTE_TEMPORARY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            disposition,
            options,
            None,
            0,
        )
    };
    if !status.is_err() {
        drop(unsafe { OwnedHandle::from_raw_handle(file.0) });
    }
    status.0 as u32
}

fn std_access() -> FILE_ACCESS_RIGHTS {
    FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE
}

fn pending(dir: &File) -> String {
    let mut info = FILE_STANDARD_INFO::default();
    let r = unsafe {
        GetFileInformationByHandleEx(
            HANDLE(dir.as_raw_handle()),
            FileStandardInfo,
            (&raw mut info).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    format!("{:?} DeletePending={}", r, info.DeletePending)
}

fn listing(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn adv92_measure() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dl = (FILE_LIST_DIRECTORY | SYNCHRONIZE).0;

    // S1: POSIX-deleted directory, probe handle open first.
    {
        let d = root.join("s1");
        std::fs::create_dir(&d).unwrap();
        let h = open_dir(&d, dl);
        drop(mark(&d, true));
        println!(
            "ADV92 S1 posix-deleted dir: create={:#010x} {} exists={}",
            nt_create(&h, "p", std_access()),
            pending(&h),
            d.exists()
        );
        let mut n = 0;
        let r = probe(&d, || {
            n += 1;
            assert!(n < 3, "S1 probe retried");
            format!("q{n}")
        });
        println!("ADV92 S1 probe -> {r:?} after {n} names");
    }
    // S2: classic delete-pending directory.
    {
        let d = root.join("s2");
        std::fs::create_dir(&d).unwrap();
        let h = open_dir(&d, dl);
        let m = mark(&d, false);
        println!(
            "ADV92 S2 classic-pending dir: create={:#010x} {}",
            nt_create(&h, "p", std_access()),
            pending(&h)
        );
        let mut n = 0;
        let r = probe(&d, || {
            n += 1;
            assert!(n < 3, "S2 probe retried");
            format!("q{n}")
        });
        println!("ADV92 S2 probe (dir pending before open) -> {r:?} after {n} names");
        drop(m);
    }
    // S3: name held by an existing directory / a delete-pending directory / a posix-deleted-but-open file.
    {
        let d = root.join("s3");
        std::fs::create_dir(&d).unwrap();
        std::fs::create_dir(d.join("sub")).unwrap();
        std::fs::create_dir(d.join("subp")).unwrap();
        let _m = mark(&d.join("subp"), false);
        let h = open_dir(&d, dl);
        println!(
            "ADV92 S3 name=existing dir: create={:#010x}",
            nt_create(&h, "sub", std_access())
        );
        println!(
            "ADV92 S3 name=delete-pending dir: create={:#010x}",
            nt_create(&h, "subp", std_access())
        );
        std::fs::write(d.join("ro"), b"").unwrap();
        let mut perm = std::fs::metadata(d.join("ro")).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(d.join("ro"), perm).unwrap();
        println!(
            "ADV92 S3 name=readonly file: create={:#010x}",
            nt_create(&h, "ro", std_access())
        );
        // A file held open by another handle with FILE_SHARE_NONE (our own concurrent probe).
        let held = d.join("held");
        let _hf = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&held)
            .unwrap();
        println!(
            "ADV92 S3 name=file open share-none: create={:#010x}",
            nt_create(&h, "held", std_access())
        );
        println!(
            "ADV92 S3 names with ':' -> {:#010x}",
            nt_create(&h, "a:b", std_access())
        );
    }
    // S4: RootDirectory handle access rights: which ones are needed for a relative create?
    {
        let d = root.join("s4");
        std::fs::create_dir(&d).unwrap();
        for (label, acc) in [
            ("FILE_READ_ATTRIBUTES", FILE_READ_ATTRIBUTES.0),
            ("SYNCHRONIZE", SYNCHRONIZE.0),
            ("0", 0u32),
            ("LIST|SYNC", dl),
        ] {
            let h = open_dir(&d, acc);
            println!(
                "ADV92 S4 root access {label}: create={:#010x} std={}",
                nt_create(&h, &format!("x-{acc}"), std_access()),
                pending(&h)
            );
        }
        println!("ADV92 S4 listing after: {:?}", listing(&d));
    }
    // S5: list-denied directory: probe error text.
    {
        let d = root.join("s5");
        std::fs::create_dir(&d).unwrap();
        let out = Command::new("icacls")
            .arg(&d)
            .args(["/deny", "*S-1-1-0:(RD)"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let r = probe(&d, || "q".to_string());
        println!("ADV92 S5 list-denied -> {r:?}");
        let rd = std::fs::read_dir(&d).map(|mut it| it.next().map(|e| e.map(|e| e.file_name())));
        println!("ADV92 S5 old path std::fs::read_dir -> {rd:?}");
        let parent = open_dir(root, SYNCHRONIZE.0);
        let list = FILE_LIST_DIRECTORY | SYNCHRONIZE;
        println!(
            "ADV92 S5 NtCreateFile open LIST no backup intent -> {:#010x}",
            nt_open(
                &parent,
                "s5",
                list,
                FILE_OPEN,
                FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT
            )
        );
        println!(
            "ADV92 S5 NtCreateFile open LIST with backup intent -> {:#010x}",
            nt_open(
                &parent,
                "s5",
                list,
                FILE_OPEN,
                FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_FOR_BACKUP_INTENT
            )
        );
        let icacls = Command::new("icacls").arg(&d).output().unwrap();
        println!(
            "ADV92 S5 acl: {}",
            String::from_utf8_lossy(&icacls.stdout).replace(['\r', '\n'], " | ")
        );
        let _ = Command::new("icacls").arg(&d).args(["/remove:d", "*S-1-1-0"]).output();
    }
    // S6: write-denied via the real check_usable path: full message.
    {
        let d = root.join("s6");
        std::fs::create_dir(&d).unwrap();
        let out = Command::new("icacls")
            .arg(&d)
            .args(["/deny", "*S-1-1-0:(OI)(CI)(WD,AD)"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let r = super::resolve(Some(d.as_os_str()), || unreachable!());
        println!("ADV92 S6 write-denied resolve -> {r:?}");
        let _ = Command::new("icacls").arg(&d).args(["/remove:d", "*S-1-1-0"]).output();
    }
    // S7: real check_usable leaves nothing behind; names used.
    {
        let d = root.join("s7");
        std::fs::create_dir(&d).unwrap();
        super::windows_probe::check_usable(&d).unwrap();
        println!("ADV92 S7 listing after check_usable: {:?}", listing(&d));
    }
}

/// Candidate strengthening test: the probe leaves nothing behind (targets dropping FILE_DELETE_ON_CLOSE).
#[test]
fn adv92_probe_leaves_nothing_behind() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("taken"), b"keep").unwrap();
    let mut names = ["taken", "fresh"].into_iter();
    probe(tmp.path(), || names.next().unwrap().to_string()).unwrap();
    assert_eq!(listing(tmp.path()), ["taken"]);
}
