//! Tests for the Unix-only atomic no-replace rename ([`super::publish`]).

use super::publish::{classify_rename_errno, RenameError};

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
