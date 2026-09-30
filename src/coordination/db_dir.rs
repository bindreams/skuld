//! Run-time location of the directory holding `.skuld.db`.
//!
//! `SKULD_DB_DIR` wins when set. Otherwise the directory is derived from the canonicalized
//! executable, so it travels with the binary (relocated build trees, extracted nextest
//! archives) and does not depend on how a symlinked binary was reached:
//!
//! - `<profile>/deps/<exe>` and `<profile>/examples/<exe>` give `<profile>`;
//! - `<profile>/build/<pkg>/<hash>/out/<exe>` (cargo's new build-dir layout) gives `<profile>`;
//! - anything else gives the executable's own directory (copied binaries).
//!
//! The first two rules step up only on a positive cargo marker: a cargo-hashed executable name
//! (`name-<16 hex>`, which nextest archives keep), a `.fingerprint` directory in the candidate
//! profile directory, or a `CACHEDIR.TAG` beside it. Without one, `/opt/app/deps/t` is just a
//! directory named `deps`.
//!
//! The database is created on first use, so the directory must let the caller create files. On
//! Unix it must also be readable and searchable, since the lock opens the directory itself; on
//! Windows the lock is a file. [`resolve`] checks this once and fails with one message naming the
//! directory and `SKULD_DB_DIR`.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use super::DB_DIR_ENV;

#[cfg(windows)]
mod windows_probe;
#[cfg(all(test, windows))]
mod windows_probe_tests;

/// `override_dir` is the value of `SKULD_DB_DIR`, if set; `exe` is called only when it is not.
/// Returns a usable directory, or a message naming the cause.
pub(super) fn resolve(
    override_dir: Option<&OsStr>,
    exe: impl FnOnce() -> io::Result<PathBuf>,
) -> Result<PathBuf, String> {
    let dir = match override_dir {
        Some(dir) => {
            let dir = Path::new(dir);
            if !dir.is_absolute() {
                return Err(format!(
                    "{DB_DIR_ENV} must be an absolute path, got {dir:?} (a relative path would depend on the working directory)"
                ));
            }
            // An existing non-directory, or an entry that cannot be queried (on Windows, one being
            // deleted), is left for `check_usable` to reject with a clear message.
            if matches!(dir.try_exists(), Ok(false)) {
                create_dir_all_open(dir)
                    .map_err(|e| format!("cannot create coordination DB directory {dir:?} (from {DB_DIR_ENV}): {e}"))?;
            }
            dir.to_path_buf()
        }
        None => {
            let exe = exe().map_err(|e| {
                format!("cannot locate the coordination DB: current_exe() failed: {e}; set {DB_DIR_ENV} to an absolute directory")
            })?;
            let exe = canonical_exe(&exe).map_err(|e| {
                format!("cannot canonicalize executable {exe:?}: {e}; set {DB_DIR_ENV} to an absolute directory")
            })?;
            layout_dir(&exe, has_cargo_marker).ok_or_else(|| {
                format!("cannot derive the coordination DB directory from executable {exe:?}; set {DB_DIR_ENV} to an absolute directory")
            })?
        }
    };
    check_usable(&dir).map_err(|e| {
        format!("coordination DB directory {dir:?} is unusable: {e}; set {DB_DIR_ENV} to a writable absolute path")
    })?;
    Ok(dir)
}

/// True when `profile` looks like a cargo profile directory.
pub(super) fn has_cargo_marker(profile: &Path) -> bool {
    profile.join(".fingerprint").is_dir() || profile.parent().is_some_and(|t| t.join("CACHEDIR.TAG").is_file())
}

/// The directory the layout rules (module doc) assign to a canonical executable path.
/// `has_marker` is asked about a candidate profile directory.
pub(super) fn layout_dir(exe: &Path, has_marker: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let parent = exe.parent()?;
    let name = |p: &Path| p.file_name().and_then(OsStr::to_str).map(str::to_owned);
    let hashed = is_cargo_hashed(exe);
    let step_up = |profile: Option<&Path>| profile.filter(|p| hashed || has_marker(p)).map(Path::to_path_buf);
    if matches!(name(parent).as_deref(), Some("deps" | "examples")) {
        if let Some(profile) = step_up(parent.parent()) {
            return Some(profile);
        }
    } else if name(parent).as_deref() == Some("out") {
        // <profile>/build/<pkg>/<hash>/out
        let build = parent.parent().and_then(Path::parent).and_then(Path::parent);
        if let Some(build) = build.filter(|b| name(b).as_deref() == Some("build")) {
            if let Some(profile) = step_up(build.parent()) {
                return Some(profile);
            }
        }
    }
    Some(parent.to_path_buf())
}

/// True for cargo's `name-<16 hex digits>` binary names (optionally `.exe` or ` (deleted)`).
fn is_cargo_hashed(exe: &Path) -> bool {
    let Some(name) = exe.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    // Linux appends " (deleted)" to the path of a binary replaced mid-run.
    let name = name.strip_suffix(" (deleted)").unwrap_or(name);
    let stem = name.strip_suffix(".exe").unwrap_or(name);
    stem.rsplit_once('-')
        .is_some_and(|(_, hash)| hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Canonicalize `exe`. Only Linux's `<path> (deleted)` form, which names a binary replaced
/// mid-run, falls back to canonicalizing the parent; any other failure is an error, since
/// resolving a dangling symlink through its own directory would pick the wrong database.
fn canonical_exe(exe: &Path) -> io::Result<PathBuf> {
    match dunce::canonicalize(exe) {
        Err(e) if e.kind() == io::ErrorKind::NotFound && cfg!(target_os = "linux") => {
            let deleted = exe
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|n| n.ends_with(" (deleted)"));
            match (deleted, exe.parent(), exe.file_name()) {
                (true, Some(parent), Some(file)) => Ok(dunce::canonicalize(parent)?.join(file)),
                _ => Err(e),
            }
        }
        other => other,
    }
}

/// `create_dir_all`, but every directory it creates is mode 0777 regardless of umask (as
/// `publish.rs` does for the DB), so whichever uid gets there first does not lock out the rest.
fn create_dir_all_open(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        create_dir_all_open_with(dir, |_, _| {})
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Unix body of [`create_dir_all_open`]. Each missing level is made at a temp sibling, chmod to
/// 0777, then renamed into place without replacing, so a path is never visible at a
/// umask-trimmed mode. `before_publish(temp, target)` runs between the chmod and the rename.
#[cfg(unix)]
pub(super) fn create_dir_all_open_with(dir: &Path, mut before_publish: impl FnMut(&Path, &Path)) -> io::Result<()> {
    use super::publish::{make_temp_path, rename_no_replace, RenameError};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let mut missing = Vec::new();
    let mut cur = dir;
    while !cur.exists() {
        missing.push(cur);
        match cur.parent() {
            Some(p) => cur = p,
            None => break,
        }
    }
    for target in missing.into_iter().rev() {
        let parent = target.parent().expect("a missing directory has a parent");
        let temp = loop {
            let candidate = make_temp_path(parent);
            match std::fs::DirBuilder::new().mode(0o700).create(&candidate) {
                Ok(()) => break candidate,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        };
        let published = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o777)).and_then(|()| {
            before_publish(&temp, target);
            match rename_no_replace(&temp, target) {
                Ok(()) => Ok(true),
                // Someone else published it first; theirs was published the same way.
                Err(RenameError::AlreadyExists) => Ok(false),
                Err(e) => Err(io::Error::other(format!("cannot publish {target:?}: {e:?}"))),
            }
        });
        match published {
            Ok(true) => {}
            Ok(false) => std::fs::remove_dir(&temp)?,
            Err(e) => {
                let _ = std::fs::remove_dir(&temp);
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Fail unless `dir` is a directory the calling identity can use (module doc), `EROFS` included.
fn check_usable(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        if !std::fs::metadata(dir)?.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, "not a directory"));
        }

        let c = std::ffi::CString::new(dir.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        // `access`, not `faccessat`: glibc's `faccessat` issues `faccessat2`, which older
        // seccomp profiles reject with EPERM. Test binaries are not setuid, so real and
        // effective ids agree.
        // SAFETY: `c` is a valid NUL-terminated string.
        if unsafe { libc::access(c.as_ptr(), libc::R_OK | libc::W_OK | libc::X_OK) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        windows_probe::check_usable(dir)
    }
}
