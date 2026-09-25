//! Unix-only atomic publish for the coordination DB. Deliberately minimal in
//! scope: no real threat model here.
//!
//! Whichever uid first opens `SKULD_TARGET_PROFILE_DIR/.skuld.db` would
//! otherwise create it at `0644 & ~umask`, locking out every other uid. The
//! one requirement this module exists for: `.skuld.db` gets created at
//! `0666`, whichever uid gets there first. It does this by creating a
//! `0600` temp with `O_EXCL` outside the `.skuld.db*` glob, `fchmod`ing it
//! to `0666`, and publishing it with an atomic no-replace rename
//! (`renameat2`/`RENAME_NOREPLACE` on Linux/Android, `renamex_np`/
//! `RENAME_EXCL` on macOS) into `.skuld.db`. On `EEXIST` — another process
//! already published — the temp is discarded and the existing file is used
//! as-is.
//!
//! **Trust boundary:** root running Skuld in a directory another uid can
//! write to trusts that uid. A root run outside CI or a throwaway VM is not
//! a supported workflow. Given that, the worst a
//! local user sharing a `target/` with another uid can do is disturb
//! Skuld's serial test scheduling for that tree — harmless on CI, minor on
//! a dev host. There is no verify-before-use step, no ownership check, and
//! no cleanup of abandoned temps: none of that is load-bearing once the
//! only goal is "the file exists and is world-writable."
//!
//! `-wal`/`-shm` are not published here at all: SQLite's own Unix VFS
//! derives their mode from the main DB file's already-`0666` mode
//! (`findCreateFileMode`, `unixOpenSharedMemory`), and `robust_open`
//! `fchmod`s to counter the umask — so once `.skuld.db` itself is `0666`,
//! its companions come out `0666` too, umask or not. Measured under a
//! restrictive umask by
//! `tests/coordination_publish_cli.rs::publish_creates_three_0666_files_despite_umask`.

use std::ffi::CString;
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::FromRawFd;
use std::path::{Path, PathBuf};

use super::skuld_debug_eprintln;

/// Ensure `db_path` is published at `0666`. Called by [`super::connect`]
/// before any SQLite call — on every connection, not just the first, so a
/// fast path matters: an `lstat` (`symlink_metadata`, not `exists()` — a
/// dangling symlink must count as "already there" too, same as
/// `publish_one`'s own `EEXIST` handling treats it) skips the publish
/// attempt entirely once something is already at `db_path`. Only creation —
/// the first connection to ever see `db_path` absent — needs the temp
/// create, `fchmod` and no-replace rename `publish_one` does; every later
/// connection would otherwise repeat all three syscalls just to discover an
/// `EEXIST` no-op on the rename. This check can still race a concurrent
/// first publish (TOCTOU between the `lstat` and `publish_one`'s own
/// create), which is why `publish_one` keeps its `EEXIST` handling rather
/// than relying on this check alone: this is a fast path over that
/// mechanism, not a replacement for it. Panics loudly on: a filesystem that
/// ignores modes, one with no atomic no-replace rename, or any other
/// unexpected publish failure.
pub(super) fn ensure_published(db_path: &Path) {
    ensure_published_with(db_path, publish_one);
}

/// [`ensure_published`]'s implementation, parameterized over the publish
/// step so `publish_tests` can assert the fast path skips it entirely
/// instead of only observing side effects that an `EEXIST` no-op would
/// produce too.
pub(super) fn ensure_published_with(db_path: &Path, publish: impl FnOnce(&Path, &Path)) {
    if std::fs::symlink_metadata(db_path).is_ok() {
        return;
    }
    let dir = db_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    publish(dir, db_path);
}

// Publish atomically =====

/// Publish `target` if nobody has yet. Composes [`create_publish_temp`] and
/// [`rename_publish_temp`]; split in two so tests in
/// `coordination::publish_tests` can drive scenarios a single `publish_one`
/// call can't: `a_lost_publish_race_uses_the_winners_file` calls
/// [`create_publish_temp`] independently for two simulated publishers before
/// either renames, and `a_vanished_own_temp_panics_loudly` removes its own
/// temp in the window between creating it and renaming it, to drive a
/// non-EEXIST rename failure — neither is reproducible as a genuine race
/// without control over exactly this window.
fn publish_one(dir: &Path, target: &Path) {
    let tmp_path = create_publish_temp(dir);
    rename_publish_temp(dir, &tmp_path, target);
}

/// Create a `0600` temp with `O_EXCL` outside the `.skuld.db*` glob,
/// `fchmod` it to `0666`, and verify the filesystem actually kept that
/// mode. Panics loudly on any failure other than the candidate name already
/// existing, which is retried with a fresh name (see
/// [`create_publish_temp_with`]'s doc for why this can happen and why the
/// retry is guaranteed to terminate).
pub(super) fn create_publish_temp(dir: &Path) -> PathBuf {
    create_publish_temp_with(dir, make_temp_path)
}

/// Outcome of one attempt to create a single publish temp candidate.
enum CreateTempOutcome {
    Created,
    /// The candidate name was already taken by another file (`O_EXCL`
    /// reported `EEXIST`). Not an error — the caller picks a new candidate
    /// and retries.
    NameTaken,
}

/// [`create_publish_temp`]'s implementation, parameterized over the
/// candidate-name generator so tests can force a deterministic collision
/// instead of racing real clock nanoseconds.
///
/// `make_temp_path`'s name embeds the pid, but pid namespaces are
/// independent: two different processes in two different namespaces (for
/// example a root container lane and an unprivileged step sharing the same
/// `target/` afterward) can report the same OS-visible pid, and both would
/// see `<seq>=0` on their first publish call. Only the nanosecond timestamp
/// would then separate them, which is a data race, not a guarantee — so a
/// same-instant collision on the *candidate name itself* is a real,
/// reachable case, not a theoretical one, and gets a real retry rather than
/// a panic.
///
/// The retry is unbounded but provably terminates: `O_EXCL` gives the
/// kernel's own mutual exclusion on the candidate name, so every `NameTaken`
/// outcome means a distinct file genuinely exists there already, and
/// `candidate` is called again to produce a new name (via a fresh
/// timestamp/counter pair) each time — the same candidate is never retried
/// twice.
pub(super) fn create_publish_temp_with(dir: &Path, mut candidate: impl FnMut(&Path) -> PathBuf) -> PathBuf {
    loop {
        let tmp_path = candidate(dir);
        match try_create_publish_temp(dir, &tmp_path) {
            CreateTempOutcome::Created => return tmp_path,
            CreateTempOutcome::NameTaken => {
                skuld_debug_eprintln!(
                    "create_publish_temp: {tmp_path:?} already existed (name collision); retrying with a fresh name"
                );
                continue;
            }
        }
    }
}

/// One attempt at [`create_publish_temp_with`]'s create-fchmod-verify
/// sequence for a single candidate path. Panics loudly on any failure
/// except the candidate already existing, which the caller retries.
fn try_create_publish_temp(dir: &Path, tmp_path: &Path) -> CreateTempOutcome {
    let tmp_c = to_cstring(tmp_path);

    let fd = unsafe {
        libc::open(
            tmp_c.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EEXIST) {
            return CreateTempOutcome::NameTaken;
        }
        panic!("skuld: could not create publish temp {tmp_path:?} in {dir:?}: {err}");
    }
    // Safety: fd was just returned by a successful open() above; File takes
    // ownership and closes it on drop.
    let file = unsafe { File::from_raw_fd(fd) };

    if unsafe { libc::fchmod(fd, 0o666) } != 0 {
        let err = std::io::Error::last_os_error();
        drop(file);
        let _ = std::fs::remove_file(tmp_path);
        panic!("skuld: could not chmod publish temp {tmp_path:?} to 0666: {err}");
    }

    let meta = match file.metadata() {
        Ok(m) => m,
        Err(e) => {
            drop(file);
            let _ = std::fs::remove_file(tmp_path);
            panic!("skuld: could not fstat publish temp {tmp_path:?}: {e}");
        }
    };
    // The requirement is world-writable, not an exact bit pattern: a
    // filesystem that hands back extra bits on top of what was asked for
    // (e.g. 0777) still satisfies it and isn't the "doesn't keep modes at
    // all" failure this guards against.
    let got_mode = meta.mode() & 0o777;
    if got_mode & 0o666 != 0o666 {
        drop(file);
        let _ = std::fs::remove_file(tmp_path);
        panic!(
            "skuld: the filesystem holding {dir:?} does not keep file modes (asked 0666, got {got_mode:04o}); put target/ on a POSIX filesystem"
        );
    }
    drop(file);

    CreateTempOutcome::Created
}

/// Link `tmp_path` in as `target` with an atomic no-replace rename.
/// `EEXIST` (another process won the race) is not an error — the temp is
/// discarded and the existing file is used as-is. Any other failure,
/// including the rename's own source having vanished, is unexpected —
/// nothing in this module ever removes a temp it didn't create itself — and
/// panics loudly.
pub(super) fn rename_publish_temp(dir: &Path, tmp_path: &Path, target: &Path) {
    let tmp_c = to_cstring(tmp_path);
    let target_c = to_cstring(target);
    handle_rename_result(dir, tmp_path, target, atomic_rename_no_replace(&tmp_c, &target_c));
}

/// The decision table `rename_publish_temp` drives off of, pulled out so
/// `publish_tests` can exercise every `RenameError` variant — including
/// [`RenameError::KernelTooOld`], which nothing in this environment can
/// trigger for real — without needing a real rename syscall to fail.
pub(super) fn handle_rename_result(dir: &Path, tmp_path: &Path, target: &Path, result: Result<(), RenameError>) {
    match result {
        Ok(()) => {}
        Err(RenameError::AlreadyExists) => {
            // Another process won the race. Its file is the one to use.
            let _ = std::fs::remove_file(tmp_path);
        }
        Err(RenameError::Unsupported(errno)) => {
            let _ = std::fs::remove_file(tmp_path);
            panic!(
                "skuld: the filesystem holding {dir:?} has no atomic no-replace rename ({}); put target/ on a local POSIX filesystem",
                std::io::Error::from_raw_os_error(errno)
            );
        }
        Err(RenameError::KernelTooOld(errno)) => {
            let _ = std::fs::remove_file(tmp_path);
            panic!(
                "skuld: this kernel doesn't implement renameat2/RENAME_NOREPLACE, needed to publish {dir:?} ({}); needs Linux >= 3.15",
                std::io::Error::from_raw_os_error(errno)
            );
        }
        Err(RenameError::Other(errno)) => {
            let _ = std::fs::remove_file(tmp_path);
            let name = target
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            panic!(
                "skuld: could not publish {name} in {dir:?}: {}",
                std::io::Error::from_raw_os_error(errno)
            );
        }
    }
}

/// A temp path outside the `.skuld.db*` glob so it's never mistaken for the
/// published file. The trailing nanosecond timestamp and per-process
/// counter make it unique even against another publish attempt from this
/// same process; a collision with a *different* process that happens to
/// report the same OS-visible pid (possible across pid namespaces) is not
/// ruled out by this scheme and is instead handled by retry — see
/// `create_publish_temp_with`.
fn make_temp_path(dir: &Path) -> PathBuf {
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
    "skuld's coordination DB publish needs an atomic no-replace rename primitive; \
     this Unix target isn't one of the ones it's implemented for (Linux, Android, macOS)"
);

// Shared helpers =====

fn to_cstring(path: &Path) -> CString {
    CString::new(path.as_os_str().as_bytes())
        .unwrap_or_else(|e| panic!("skuld: coordination DB path {path:?} contains a NUL byte: {e}"))
}
