//! Built-in precondition helpers for checking external tool availability.

use std::path::Path;
use std::process::{Command, Stdio};

/// Precondition: check that an executable is on PATH.
///
/// Returns `Ok(())` if `<name> --version` succeeds, or `Err` with a message.
pub fn probe_executable(name: &str) -> Result<(), String> {
    let ok = Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        Ok(())
    } else {
        Err(format!("{name} not installed"))
    }
}

/// Precondition: check that a file exists at the given path.
///
/// Returns `Ok(())` if the path exists, or `Err` with a message.
pub fn probe_path(path: impl AsRef<Path>) -> Result<(), String> {
    let path = path.as_ref();
    if path.exists() {
        Ok(())
    } else {
        Err(format!("{} not found", path.display()))
    }
}
