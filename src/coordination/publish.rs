//! Unix-only atomic no-replace rename, which `db_dir` publishes the coordination directories with.
//!
//! A directory is created at a temp sibling ([`make_temp_path`]), chmod to 0777, then renamed into
//! place by [`rename_no_replace`] (`renameat2`/`RENAME_NOREPLACE` on Linux/Android,
//! `renamex_np`/`RENAME_EXCL` on macOS), so a path is never visible at a umask-trimmed mode and
//! two creators racing converge on the winner's directory.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// A unique sibling path for a directory that is about to be published by [`rename_no_replace`]:
/// the pid, a nanosecond timestamp and a per-process counter make it unique against this process;
/// a collision with a different process (possible across pid namespaces) is resolved by the
/// caller retrying, since creating it is exclusive.
pub(super) fn make_temp_path(dir: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".skuld-publish-{pid}-{nanos}-{seq}.tmp"))
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RenameError {
    AlreadyExists,
    Unsupported(i32),
    KernelTooOld(i32),
    Other(i32),
}

pub(super) fn classify_rename_errno(errno: i32) -> RenameError {
    match errno {
        libc::EEXIST => RenameError::AlreadyExists,
        libc::EINVAL | libc::ENOTSUP => RenameError::Unsupported(errno),
        libc::ENOSYS => RenameError::KernelTooOld(errno),
        other => RenameError::Other(other),
    }
}

// `libc::renameat2` (the function, not the `SYS_renameat2` syscall-number
// constant) has a version floor on every libc that would matter here, and
// the floor is a *link-time* one — the call compiles, then fails to
// link — which is worse than a `compile_error!` because it doesn't say why:
//   - glibc only exports the wrapper from 2.28 onward; Rust's `*-linux-gnu`
//     baseline is 2.17 (RHEL/CentOS 7 territory), well below that.
//   - musl only gained the wrapper in 1.2.6; the musl libc Rust's own
//     `*-linux-musl` targets bundle and link against is 1.2.5 (confirmed via
//     `nm` on rustup's bundled `libc.a`: `renameat`/`preadv2`/`statx` are
//     present, `renameat2` isn't).
//   - uclibc's Linux targets (`arm`/`mips(el)-unknown-linux-uclibc*`) don't
//     declare the wrapper in the `libc` crate at all — only the
//     syscall-number constant — so calling `libc::renameat2` there doesn't
//     even get this far: it's `E0425: cannot find function` at compile time.
//
// `SYS_renameat2`, the syscall-number constant as opposed to the wrapper
// function, carries none of those floors: it's declared for every
// Linux/libc combination this crate can target (glibc, musl and uclibc
// alike, every architecture), because it's just an integer, not a symbol
// that has to exist in some linked-against library. Going straight to the
// kernel via `syscall()` sidesteps every one of the wrapper's version
// floors at once — Bionic's own API-30 floor on the wrapper below, which
// this same sidestep already had to clear for Android, generalizes to the
// whole Linux family the same way, since none of its libcs can be relied
// on for the wrapper either.
//
// This does not sidestep the *kernel's* floor: `renameat2` support was
// only added in Linux 3.15, and a syscall on an older kernel fails with
// `ENOSYS`, which `classify_rename_errno` maps to a distinct `RenameError`
// so `handle_rename_result`'s panic says the kernel is too old rather than
// blaming the filesystem, which is what `EINVAL`/`ENOTSUP` mean here.
// `libc::syscall` is declared variadic, so passing `RENAME_NOREPLACE` as
// the `c_uint` every Linux libc already declares it would compile just as
// well as this cast does; the explicit `as libc::c_uint` only matters on
// Android, where `libc` declares the constant as `c_int` instead — this
// cast keeps the call uniform across both `cfg`s and matches the kernel's
// own `flags: unsigned int` signature for this argument either way.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn atomic_rename_no_replace(from: &CString, to: &CString) -> Result<(), RenameError> {
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE as libc::c_uint,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(classify_rename_errno(
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        ))
    }
}

#[cfg(target_os = "macos")]
fn atomic_rename_no_replace(from: &CString, to: &CString) -> Result<(), RenameError> {
    let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(classify_rename_errno(
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        ))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
compile_error!(
    "skuld's coordination directory publish needs an atomic no-replace rename primitive; \
     this Unix target isn't one of the ones it's implemented for (Linux, Android, macOS)"
);

/// `rename(2)` `from` to `to` without replacing an existing `to`, for the directories `db_dir`
/// publishes.
pub(super) fn rename_no_replace(from: &Path, to: &Path) -> Result<(), RenameError> {
    atomic_rename_no_replace(&to_cstring(from), &to_cstring(to))
}

// Shared helpers =====

fn to_cstring(path: &Path) -> CString {
    CString::new(path.as_os_str().as_bytes())
        .unwrap_or_else(|e| panic!("skuld: path {path:?} contains a NUL byte: {e}"))
}
