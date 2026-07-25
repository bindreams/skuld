//! Pure construction of the `cargo nextest run` invocation — unit-testable
//! without spawning a process. The actual `.status()` call lives in
//! `main.rs`, a one-line, deliberately untested wrapper.

use std::path::Path;
use std::process::Command;

pub fn build_nextest_run_command(tool_config_path: &Path, passthrough: &[String]) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.arg("nextest")
        .arg("run")
        .arg("--tool-config-file")
        .arg(format!("skuld:{}", tool_config_path.display()));
    cmd.args(passthrough);
    cmd
}

#[cfg(test)]
mod command_tests;
