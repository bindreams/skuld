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
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // A virtual workspace with zero members is a legitimate, if
        // degenerate, workspace shape (e.g. a freshly scaffolded
        // `[workspace]\nmembers = []`) — `cargo metadata` itself refuses to
        // proceed at all in that case ("the manifest is virtual, and the
        // workspace has no members"), before any package- or binary-level
        // logic runs. Zero packages trivially implies zero binaries, so we
        // treat this one documented cargo condition as an empty result
        // rather than a hard failure. Any other failure (nextest missing,
        // a malformed manifest, etc.) still propagates as an error.
        anyhow::ensure!(
            stderr.contains("the workspace has no members"),
            "cargo nextest list failed: {stderr}"
        );
        return Ok(Vec::new());
    }
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
