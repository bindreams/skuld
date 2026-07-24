//! Discovers workspace test binaries via `cargo nextest list
//! --list-type binaries-only`, reusing nextest's own authoritative
//! binary-id/binary-path resolution.
//!
//! Requires `cargo-nextest` installed and on `PATH`.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredBinary {
    pub binary_id: String,
    pub binary_path: PathBuf,
}

#[derive(serde::Deserialize)]
struct BinariesOnlyList {
    #[serde(rename = "rust-binaries")]
    rust_binaries: std::collections::BTreeMap<String, BinaryEntry>,
}

#[derive(serde::Deserialize)]
struct BinaryEntry {
    #[serde(rename = "binary-id")]
    binary_id: String,
    #[serde(rename = "binary-path")]
    binary_path: PathBuf,
}

pub fn discover_binaries(manifest_dir: &Path) -> anyhow::Result<Vec<DiscoveredBinary>> {
    let output = Command::new("cargo")
        .current_dir(manifest_dir)
        .args([
            "nextest",
            "list",
            "--list-type",
            "binaries-only",
            "--message-format",
            "json",
        ])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "cargo nextest list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: BinariesOnlyList = serde_json::from_slice(&output.stdout)?;
    Ok(parsed
        .rust_binaries
        .into_values()
        .map(|e| DiscoveredBinary {
            binary_id: e.binary_id,
            binary_path: e.binary_path,
        })
        .collect())
}

#[cfg(test)]
mod discovery_tests;
