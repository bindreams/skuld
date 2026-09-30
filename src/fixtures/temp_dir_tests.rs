use super::temp_dir::{create_dir, TempDir};
use std::io;
use std::path::Path;

/// Whether `err` names `path`, as written or as `{:?}` formats it.
fn names_path(err: &str, path: &Path) -> bool {
    let debug = format!("{path:?}");
    err.contains(path.to_str().unwrap()) || err.contains(debug.trim_matches('"'))
}

/// Create the directory `path` the way `TempDir` does.
fn create(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return create_dir(path);
    #[cfg(windows)]
    {
        let parent = path.parent().unwrap();
        create_dir(&crate::win_nt::open_dir(parent).unwrap(), parent, path)
    }
}

#[test]
fn a_name_held_by_an_existing_entry_is_already_exists() {
    let parent = TempDir::new().unwrap();
    std::fs::write(parent.join("file"), b"keep").unwrap();
    std::fs::create_dir(parent.join("dir")).unwrap();

    for taken in ["file", "dir"] {
        let err = create(&parent.join(taken)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{taken}: {err}");
    }
    assert_eq!(std::fs::read(parent.join("file")).unwrap(), b"keep");
}

#[test]
fn new_directories_are_distinct_and_removed_on_drop() {
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    assert_ne!(a.path(), b.path());
    std::fs::write(a.join("f"), b"").unwrap();
    let path = a.path().to_path_buf();
    drop(a);
    assert!(!path.exists());
    assert!(b.is_dir());
}

#[test]
fn names_embed_the_process_id() {
    let dir = TempDir::new().unwrap();
    let name = dir.file_name().unwrap().to_str().unwrap();
    assert!(name.contains(&format!("-{}-", std::process::id())), "{name}");
}

#[test]
fn new_in_creates_in_the_given_parent() {
    let parent = TempDir::new().unwrap();
    let dir = TempDir::new_in(&parent).unwrap();
    assert_eq!(dir.parent(), Some(parent.path()));
    assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 1);
}

#[test]
fn close_removes_the_directory() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.join("f"), b"").unwrap();
    let path = dir.path().to_path_buf();
    dir.close().unwrap();
    assert!(!path.exists());
}

#[test]
fn debug_names_the_path() {
    let dir = TempDir::new().unwrap();
    assert!(names_path(&format!("{dir:?}"), dir.path()), "{dir:?}");
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn assert_not_root() {
        // SAFETY: geteuid has no preconditions.
        assert_ne!(
            unsafe { libc::geteuid() },
            0,
            "permission tests require a non-root user"
        );
    }

    #[test]
    fn directories_are_private_to_their_owner() {
        let dir = TempDir::new().unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{mode:o}");
    }

    #[test]
    fn close_returns_the_removal_error_naming_the_path() {
        assert_not_root();
        let dir = TempDir::new().unwrap();
        let locked = dir.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("f"), b"").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
        let path = dir.path().to_path_buf();

        let err = dir.close().unwrap_err().to_string();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(&path).unwrap();
        assert!(names_path(&err, &path), "{err}");
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::win_nt::test_support::mark_for_deletion;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn a_name_held_by_a_delete_pending_directory_is_already_exists() {
        let parent = TempDir::new().unwrap();
        let held = parent.join("held");
        std::fs::create_dir(&held).unwrap();
        let _pending = mark_for_deletion(&held, false);

        let err = create(&held).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
    }

    fn assert_says_being_deleted(err: &str, parent: &Path) {
        assert!(
            err.contains("being deleted") && !err.contains("Access is denied"),
            "the error must name the deletion, not an access denial: {err}"
        );
        assert!(names_path(err, parent), "{err}");
    }

    #[test]
    fn a_parent_being_deleted_says_so() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.join("d");
        std::fs::create_dir(&parent).unwrap();
        let _pending = mark_for_deletion(&parent, false);

        let Err(err) = TempDir::new_in(&parent) else {
            panic!("{parent:?} must be rejected")
        };
        assert_says_being_deleted(&err.to_string(), &parent);
    }

    /// Returning AlreadyExists here would have tempfile retry a name that can never succeed.
    #[test]
    fn a_parent_deleted_after_it_was_opened_says_so() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.join("d");
        std::fs::create_dir(&parent).unwrap();
        let handle = crate::win_nt::open_dir(&parent).unwrap();
        let _pending = mark_for_deletion(&parent, false);

        let err = create_dir(&handle, &parent, &parent.join("p")).unwrap_err();
        assert_ne!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
        assert_says_being_deleted(&err.to_string(), &parent);
    }

    #[test]
    fn close_returns_the_removal_error_naming_the_path() {
        let dir = TempDir::new().unwrap();
        let held = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(dir.join("f"))
            .unwrap();
        let path = dir.path().to_path_buf();

        let err = dir.close().unwrap_err().to_string();
        drop(held);
        std::fs::remove_dir_all(&path).unwrap();
        assert!(names_path(&err, &path), "{err}");
    }
}

// Parity with tempfile -----

#[test]
fn a_relative_parent_hands_out_an_absolute_path() {
    let dir = TempDir::new_in(".").unwrap();
    assert!(dir.path().is_absolute(), "{:?}", dir.path());
    let path = dir.path().to_path_buf();
    drop(dir);
    assert!(!path.exists(), "{path:?}");
}

/// Another local user must not be able to plant every next name in a shared parent, so each name
/// ends in tempfile's random suffix rather than a counter.
#[test]
fn names_end_in_a_random_suffix_after_the_pid() {
    let dir = TempDir::new().unwrap();
    let name = dir.file_name().unwrap().to_str().unwrap();
    let marker = format!("-{}-", std::process::id());
    let (_, suffix) = name.rsplit_once(&marker).unwrap_or_else(|| panic!("{name}"));
    assert!(
        suffix.len() == 6 && suffix.chars().all(|c| c.is_ascii_alphanumeric()),
        "{name}"
    );
}

/// Test names are arbitrary strings, and the fixture uses them as the prefix.
#[test]
fn any_prefix_gives_a_directory_directly_in_the_parent() {
    let parent = TempDir::new().unwrap();
    let long = "x".repeat(40_000);
    for prefix in ["a/b", "a\\b", "a:b", "a\u{1}b", long.as_str()] {
        let dir = TempDir::with_prefix_in(prefix, &parent).unwrap_or_else(|e| panic!("{prefix:.20}: {e}"));
        assert_eq!(dir.parent(), Some(parent.path()), "{prefix:.20}");
        assert!(dir.is_dir(), "{prefix:.20}");
        let name = dir.file_name().unwrap().to_str().unwrap();
        assert!(name.encode_utf16().count() <= 255, "{prefix:.20}: {} units", name.len());
    }
}

#[test]
fn a_missing_parent_is_an_error_naming_it() {
    let tmp = TempDir::new().unwrap();
    let parent = tmp.join("nope").join("deeper");
    let Err(err) = TempDir::new_in(&parent) else {
        panic!("{parent:?} must be rejected")
    };
    let err = err.to_string();
    assert!(names_path(&err, &parent), "{err}");
}

#[test]
fn a_parent_that_is_a_file_is_an_error_naming_it() {
    let tmp = TempDir::new().unwrap();
    let parent = tmp.join("file");
    std::fs::write(&parent, b"").unwrap();
    let Err(err) = TempDir::new_in(&parent) else {
        panic!("{parent:?} must be rejected")
    };
    let err = err.to_string();
    assert!(names_path(&err, &parent), "{err}");
}
