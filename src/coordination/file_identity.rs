//! A file's identity: what stays the same when its path is renamed and
//! differs when the path is retargeted at another file.

use std::fs::File;
use std::io;
use std::path::Path;

/// A file's identity, following symlinks and reparse points as
/// a fresh open of the same path would.
///
/// Unix: device + inode (`stat`). Windows: volume serial + 64-bit file index
/// (`GetFileInformationByHandle`). Resolving a path fresh, not reusing a held
/// handle, is what lets it observe a retargeted ancestor: that changes what a
/// new resolution reaches, not the file, so `FILE_SHARE_DELETE` being
/// withheld does not cover it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct FileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(windows)]
    volume_serial: u32,
    #[cfg(windows)]
    file_index: u64,
}

impl FileIdentity {
    /// `Err` means nothing resolvable exists at `path` right now (or it cannot be inspected),
    /// which is as much "moved" as a mismatched identity.
    #[cfg(unix)]
    pub(super) fn try_of(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).map(|m| Self {
            dev: m.dev(),
            ino: m.ino(),
        })
    }

    /// [`Self::try_of`] for `path` resolved relative to the open directory `dir`, so it names the
    /// directory `dir` is, not whatever now sits at its path.
    #[cfg(unix)]
    pub(super) fn try_of_at(dir: &File, path: &Path) -> io::Result<Self> {
        let st = rustix::fs::statat(dir, path, rustix::fs::AtFlags::empty())?;
        // The same widening `std::os::unix::fs::MetadataExt::{dev, ino}` does.
        #[allow(clippy::unnecessary_cast)]
        Ok(Self {
            dev: st.st_dev as u64,
            ino: st.st_ino as u64,
        })
    }

    #[cfg(windows)]
    pub(super) fn try_of(path: &Path) -> io::Result<Self> {
        Self::of_file(&File::open(path)?)
    }

    /// The identity of the file `file` has open, whatever its path now names.
    pub(super) fn of_file(file: &File) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            Self::of_fd(file.as_raw_fd())
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            Self::of_handle(windows::Win32::Foundation::HANDLE(file.as_raw_handle()))
        }
    }

    /// `fd` must be an fd; a closed one is an `Err`.
    #[cfg(unix)]
    pub(super) fn of_fd(fd: i32) -> io::Result<Self> {
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // Safety: `st` is a valid out-pointer; `fstat` on a closed fd just fails.
        if unsafe { libc::fstat(fd, st.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // Safety: `fstat` succeeded, so it initialised `st`.
        let st = unsafe { st.assume_init() };
        // The same widening `std::os::unix::fs::MetadataExt::{dev, ino}` does.
        #[allow(clippy::unnecessary_cast)]
        Ok(Self {
            dev: st.st_dev as u64,
            ino: st.st_ino as u64,
        })
    }

    /// `handle` must be a valid open file handle.
    #[cfg(windows)]
    pub(super) fn of_handle(handle: windows::Win32::Foundation::HANDLE) -> io::Result<Self> {
        use windows::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};

        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // Safety: the caller guarantees `handle` is valid; `&mut info` is a
        // valid out-pointer.
        unsafe { GetFileInformationByHandle(handle, &mut info) }.map_err(io::Error::from)?;
        Ok(Self {
            volume_serial: info.dwVolumeSerialNumber,
            file_index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        })
    }
}
