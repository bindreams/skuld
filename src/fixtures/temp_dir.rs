//! Temporary directories: the per-test `temp_dir` fixture, and [`TempDir`] for use outside it.

use std::io;
use std::ops::Deref;
use std::path::{Path, PathBuf};

/// A temporary directory, removed with its contents on drop.
///
/// Names are `<prefix>-<pid>-<6 random characters>`, created in an absolute parent through
/// [`tempfile::Builder::make_in`], which retries a taken name with fresh randomness. On Windows a
/// name held by an entry that is deleted but still open elsewhere counts as taken; Win32 would
/// report it as access denied. After tempfile's attempt limit (65,536 taken names) creation gives
/// up with `AlreadyExists`.
///
/// On Unix the directory is created with mode 0700. On Windows it inherits the parent's ACL.
///
/// [`TempDir::new`] and [`TempDir::new_in`] hand out the path as created; the `temp_dir` fixture
/// hands out its canonical form (symlinks such as macOS `/var` → `/private/var` resolved).
///
/// Removal on drop can fail, for example while another process holds a file inside open without
/// sharing deletion; drop then prints a warning to stderr and leaves the directory. [`TempDir::close`]
/// returns that error instead.
#[derive(Debug)]
pub struct TempDir {
    /// The path it was created at; removed on drop. Empty once [`TempDir::close`] has run.
    created: PathBuf,
    /// The path handed out: `created`, or its canonical form for the fixture.
    path: PathBuf,
}

impl TempDir {
    /// A new, empty directory in [`std::env::temp_dir`], named `.tmp-<pid>-<random>`.
    pub fn new() -> io::Result<Self> {
        Self::new_in(std::env::temp_dir())
    }

    /// A new, empty directory in `parent`, named `.tmp-<pid>-<random>`. A relative `parent` is
    /// resolved against the working directory now, so a later directory change does not move it.
    pub fn new_in(parent: impl AsRef<Path>) -> io::Result<Self> {
        Self::with_prefix_in(".tmp", parent.as_ref())
    }

    /// A new directory in `parent`, named after `prefix` made safe for a file name.
    pub(crate) fn with_prefix_in(prefix: &str, parent: &Path) -> io::Result<Self> {
        let parent = std::path::absolute(parent).map_err(|e| io::Error::new(e.kind(), format!("{e}: {parent:?}")))?;
        #[cfg(unix)]
        return Self::create_with(prefix, &parent, create_dir);
        #[cfg(windows)]
        {
            let handle = crate::win_nt::open_dir(&parent)?;
            Self::create_with(prefix, &parent, |path| create_dir(&handle, &parent, path))
        }
    }

    /// [`TempDir::with_prefix_in`] with the directory created by `create`, called with each
    /// candidate path until it returns anything but `AlreadyExists`.
    pub(crate) fn create_with(
        prefix: &str,
        parent: &Path,
        create: impl FnMut(&Path) -> io::Result<()>,
    ) -> io::Result<Self> {
        debug_assert!(parent.is_absolute(), "{parent:?}");
        let prefix = format!("{}-{}-", file_name_safe(prefix), std::process::id());
        let made = tempfile::Builder::new()
            .prefix(&prefix)
            .rand_bytes(RANDOM_LEN)
            .disable_cleanup(true)
            .make_in(parent, create)?;
        let created = made.path().to_path_buf();
        Ok(Self {
            path: created.clone(),
            created,
        })
    }

    /// The directory's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Remove the directory and its contents, returning the error drop would only warn about.
    pub fn close(mut self) -> io::Result<()> {
        let created = std::mem::take(&mut self.created);
        std::fs::remove_dir_all(&created).map_err(|e| io::Error::new(e.kind(), format!("{e}: {created:?}")))
    }
}

/// Characters of randomness tempfile appends to each name.
pub(crate) const RANDOM_LEN: usize = 6;

/// The longest file name in bytes (Unix) or UTF-16 units (Windows) that common file systems accept.
pub(crate) const NAME_MAX: usize = 255;

/// `prefix` made safe as the start of a file name: path separators, `:`, the other characters
/// Windows forbids in names, and control characters become `_`; a DOS device name (see
/// [`is_dos_device`]) gets a leading `_`; and it is cut short enough, in bytes, that
/// `<prefix>-<pid>-<random>` fits [`NAME_MAX`].
pub(crate) fn file_name_safe(prefix: &str) -> String {
    const RESERVED_PID_AND_RANDOM: usize = "-4294967295-".len() + RANDOM_LEN;
    // One byte stays free for the `_` a device name gets.
    let budget = NAME_MAX - RESERVED_PID_AND_RANDOM - 1;
    let mut out = String::new();
    for c in prefix.chars() {
        let c = if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            '_'
        } else {
            c
        };
        if out.len() + c.len_utf8() > budget {
            break;
        }
        out.push(c);
    }
    if is_dos_device(&out) {
        out.insert(0, '_');
    }
    out
}

/// Whether Win32 would read a name starting with `name` as a DOS device: the part before the first
/// `.`, trailing spaces trimmed, is `CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9`, `LPT0`-`LPT9` (the
/// digit may be a superscript `¹²³`), `CONIN$` or `CONOUT$`, in any case. The directory itself is
/// created natively, but every later Win32 use of its path would open the device instead.
fn is_dos_device(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or_default().trim_end_matches(' ');
    let upper = base.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") {
        return true;
    }
    let mut chars = upper.chars();
    let stem: String = chars.by_ref().take(3).collect();
    let digit = chars.next();
    (stem == "COM" || stem == "LPT")
        && chars.next().is_none()
        && digit.is_some_and(|d| d.is_ascii_digit() || matches!(d, '¹' | '²' | '³'))
}

/// Create the directory `path`. A taken name is `AlreadyExists`, which tempfile retries.
#[cfg(unix)]
pub(crate) fn create_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(|e| io::Error::new(e.kind(), format!("{e} at path {path:?}")))
}

/// Create the directory `path` in `parent`, which `handle` is open on. A taken name, including one
/// held by an entry that is deleted but still open elsewhere, is `AlreadyExists`, which tempfile
/// retries; a parent that is itself being deleted is an error.
#[cfg(windows)]
pub(crate) fn create_dir(handle: &std::fs::File, parent: &Path, path: &Path) -> io::Result<()> {
    use crate::win_nt::{nt_create, nt_error, standard_info};
    use std::os::windows::ffi::OsStrExt;
    use windows::Wdk::Storage::FileSystem::{FILE_CREATE, FILE_DIRECTORY_FILE};
    use windows::Win32::Foundation::{STATUS_DELETE_PENDING, STATUS_OBJECT_NAME_COLLISION};
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAGS_AND_ATTRIBUTES, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let name: Vec<u16> = path
        .file_name()
        .expect("tempfile names a child")
        .encode_wide()
        .collect();
    let created = nt_create(
        Some(handle),
        &name,
        FILE_READ_ATTRIBUTES,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        FILE_CREATE,
        FILE_DIRECTORY_FILE,
    );
    let status = match created {
        Ok(_) => return Ok(()),
        Err(status) => status,
    };
    let taken = |why: &str| io::Error::new(io::ErrorKind::AlreadyExists, format!("{path:?} {why}"));
    if status == STATUS_OBJECT_NAME_COLLISION {
        return Err(taken("exists"));
    }
    if status != STATUS_DELETE_PENDING {
        return Err(nt_error(status, path));
    }
    let parent_pending = standard_info(handle)
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "creating {path:?} gave NTSTATUS {:#010x}, and asking whether {parent:?} is being deleted failed: {e}",
                    status.0 as u32
                ),
            )
        })?
        .DeletePending;
    if parent_pending {
        return Err(io::Error::other(format!(
            "{parent:?} is being deleted (NTSTATUS {:#010x})",
            status.0 as u32
        )));
    }
    Err(taken("is held by an entry being deleted"))
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if self.created.as_os_str().is_empty() {
            return;
        }
        if let Err(e) = std::fs::remove_dir_all(&self.created) {
            warn(format_args!(
                "skuld: warning: could not remove temporary directory {:?}: {e}",
                self.created
            ));
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Where this thread's [`warn`] writes, when a test sets it; stderr otherwise.
    pub(crate) static WARNINGS: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
}

/// Print `msg` to stderr, ignoring a failed write: a drop must not panic.
fn warn(msg: std::fmt::Arguments<'_>) {
    use std::io::Write;

    #[cfg(test)]
    {
        let captured = WARNINGS.with_borrow_mut(|sink| sink.as_mut().map(|buf| writeln!(buf, "{msg}")));
        if captured.is_some() {
            return;
        }
    }
    let _ = writeln!(io::stderr(), "{msg}");
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

use crate::fixtures::test_name::test_name;

/// A fresh temporary directory named after the current test (see [`TempDir`]). Its path is
/// canonical.
#[skuld::fixture(deref)]
pub fn temp_dir(#[fixture(test_name)] name: &str) -> Result<TempDir, String> {
    let mut dir =
        TempDir::with_prefix_in(name, &std::env::temp_dir()).map_err(|e| format!("failed to create temp dir: {e}"))?;
    dir.path = dir
        .created
        .canonicalize()
        .map_err(|e| format!("failed to canonicalize temp dir: {e}"))?;
    Ok(dir)
}
