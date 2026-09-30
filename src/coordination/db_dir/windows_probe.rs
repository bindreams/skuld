//! Windows half of `check_usable`: no `access`-style query reflects ACLs, so create and delete a
//! probe file.

use std::io;
use std::path::Path;

/// Fail unless a file can be created in `dir`.
pub(super) fn check_usable(dir: &Path) -> io::Result<()> {
    let pid = std::process::id();
    let mut n = 0u64;
    probe(dir, || {
        n += 1;
        format!(".skuld-probe-{pid}-{n}")
    })
}

/// [`check_usable`] with the probe names drawn from `next_name`.
pub(super) fn probe(dir: &Path, mut next_name: impl FnMut() -> String) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_TEMPORARY;

    drop(std::fs::read_dir(dir)?);
    loop {
        let path = dir.join(next_name());
        let created = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .custom_flags(FILE_ATTRIBUTE_TEMPORARY.0)
            .open(&path);
        match created {
            Ok(file) => {
                let _ = std::fs::remove_file(&path);
                drop(file);
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io::Error::new(e.kind(), format!("{e} at path {path:?}"))),
        }
    }
}
