//! `xtask` library — workspace task runner for skuld.
//!
//! Currently exposes only the `version` subcommand. More can be added later
//! (e.g. release rehearsal, docs build) using the same dispatch pattern.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

pub mod version;

#[cfg(test)]
#[path = "version_tests.rs"]
mod version_tests;

#[derive(Parser)]
#[command(name = "xtask", about = "Workspace task runner for skuld")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Print or validate the workspace version.
    ///
    /// Without flags: print the shared Cargo.toml version.
    /// With `--check`: validate it matches the nearest git tag, or is one bump ahead.
    /// With `--check --exact`: require an exact match (used by release CI).
    Version {
        #[arg(long)]
        check: bool,
        #[arg(long, requires = "check")]
        exact: bool,
    },
}

pub fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Version { check, exact } => run_version(check, exact),
    }
}

fn run_version(check: bool, exact: bool) -> Result<()> {
    let repo_root = repo_root()?;
    let v = if check {
        version::validate_against_tag(&repo_root, exact)?
    } else {
        version::workspace_version(&repo_root)?
    };
    println!("{v}");
    Ok(())
}

/// Locate the workspace root. Prefers `CARGO_MANIFEST_DIR`'s parent (set by
/// cargo when building xtask); falls back to walking up from `current_exe`.
/// Avoids git so xtask works in source tarballs / minimal CI images.
fn repo_root() -> Result<PathBuf> {
    if let Some(manifest_dir) = std::env::var_os("CARGO_MANIFEST_DIR") {
        let manifest_dir = PathBuf::from(manifest_dir);
        if let Some(parent) = manifest_dir.parent() {
            if parent.join("Cargo.toml").is_file() {
                return Ok(parent.to_path_buf());
            }
        }
    }
    let mut dir = std::env::current_exe()?;
    while dir.pop() {
        let candidate = dir.join("Cargo.toml");
        if candidate.is_file() {
            if let Ok(s) = std::fs::read_to_string(&candidate) {
                // Line-prefix check, not substring — avoids false positives
                // if `[workspace]` appears inside a string literal somewhere.
                if s.lines().any(|l| l.trim_start().starts_with("[workspace]")) {
                    return Ok(dir);
                }
            }
        }
    }
    anyhow::bail!("could not locate workspace root")
}
