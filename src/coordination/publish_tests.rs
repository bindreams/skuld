//! Tests for the Unix-only atomic no-replace rename ([`super::publish`]).

use super::publish::{classify_rename_errno, make_temp_path, rename_no_replace, RenameError};

#[test]
fn classify_rename_errno_maps_known_errnos() {
    assert_eq!(classify_rename_errno(libc::EEXIST), RenameError::AlreadyExists);
    assert_eq!(
        classify_rename_errno(libc::EINVAL),
        RenameError::Unsupported(libc::EINVAL)
    );
    assert_eq!(
        classify_rename_errno(libc::ENOTSUP),
        RenameError::Unsupported(libc::ENOTSUP)
    );
    assert_eq!(
        classify_rename_errno(libc::ENOSYS),
        RenameError::KernelTooOld(libc::ENOSYS)
    );
    assert_eq!(classify_rename_errno(libc::EACCES), RenameError::Other(libc::EACCES));
}

#[test]
fn rename_no_replace_moves_to_a_fresh_name() {
    let dir = crate::TempDir::new().unwrap();
    let (from, to) = (dir.path().join("from"), dir.path().join("to"));
    std::fs::write(&from, "moved").unwrap();

    assert_eq!(rename_no_replace(&from, &to), Ok(()));
    assert!(!from.exists());
    assert_eq!(std::fs::read_to_string(&to).unwrap(), "moved");
}

#[test]
fn rename_no_replace_refuses_an_existing_name_and_leaves_it_unchanged() {
    let dir = crate::TempDir::new().unwrap();
    let (from, to) = (dir.path().join("from"), dir.path().join("to"));
    std::fs::write(&from, "new").unwrap();
    std::fs::write(&to, "old").unwrap();

    assert_eq!(rename_no_replace(&from, &to), Err(RenameError::AlreadyExists));
    assert_eq!(std::fs::read_to_string(&to).unwrap(), "old");
    assert_eq!(std::fs::read_to_string(&from).unwrap(), "new");
}

#[test]
fn make_temp_path_yields_distinct_paths_in_the_directory() {
    let dir = crate::TempDir::new().unwrap();
    let (a, b) = (make_temp_path(dir.path()), make_temp_path(dir.path()));
    assert_ne!(a, b);
    assert_eq!(a.parent(), Some(dir.path()));
}
