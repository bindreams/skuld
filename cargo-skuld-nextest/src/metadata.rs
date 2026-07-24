//! Probes each discovered binary for Skuld's private nextest-metadata
//! dump, bounded by a timeout so a hung binary can't stall generation.
//! Deserializes directly into `skuld::NextestTestMetadata`.
//!
//! Error-handling policy, applied uniformly and at the narrowest possible
//! scope: nextest's own custom-harness contract requires every binary it
//! discovers to already support `--list` successfully, so ANY anomaly
//! (spawn failure, timeout, nonzero exit, unreadable dump, malformed JSON,
//! an individual test's unparsable serial_filter) is logged as a warning
//! and only the affected test or binary is skipped — never conflated with
//! the one truly-benign case: exit 0, no dump file, meaning the binary
//! simply isn't Skuld-based.

use crate::discovery::DiscoveredBinary;
use skuld::NextestTestMetadata;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

const METADATA_PATH_ENV: &str = "SKULD_NEXTEST_METADATA_PATH";
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestMetadata {
    pub binary_id: String,
    pub name: String,
    pub labels: Vec<String>,
    pub serial_filter: String,
}

#[derive(serde::Deserialize)]
struct Dump {
    tests: Vec<NextestTestMetadata>,
}

pub fn collect_metadata(binaries: &[DiscoveredBinary]) -> anyhow::Result<Vec<TestMetadata>> {
    collect_metadata_with_timeout(binaries, PROBE_TIMEOUT)
}

fn collect_metadata_with_timeout(
    binaries: &[DiscoveredBinary],
    timeout: Duration,
) -> anyhow::Result<Vec<TestMetadata>> {
    let mut all = Vec::new();
    for binary in binaries {
        let dir = tempfile::tempdir()?;
        let dump_path = dir.path().join("meta.json");
        // stderr goes to a real file, not a pipe: a piped stream that isn't
        // drained until after wait_timeout() returns can deadlock if the
        // child writes more than the OS pipe buffer before exiting (review
        // round 3 fix: correctness finding 5b33e075). Writing to a file has
        // no such buffer limit.
        let stderr_path = dir.path().join("stderr.log");
        let stderr_file = std::fs::File::create(&stderr_path)?;

        let mut child = match Command::new(&binary.binary_path)
            .arg("--list")
            .env(METADATA_PATH_ENV, &dump_path)
            // The dump must describe the FULL test set regardless of this
            // tool's own ambient environment.
            .env_remove("SKULD_LABELS")
            .env_remove("SKULD_DEBUG")
            .stdout(Stdio::null())
            .stderr(stderr_file)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "[cargo-skuld-nextest] warning: failed to spawn {:?}: {e}; skipping",
                    binary.binary_path
                );
                continue;
            }
        };

        let status = match child.wait_timeout(timeout) {
            Ok(Some(status)) => status,
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                eprintln!(
                    "[cargo-skuld-nextest] warning: {:?} did not exit within {timeout:?} on --list; killed and skipped",
                    binary.binary_path
                );
                continue;
            }
            Err(e) => {
                eprintln!(
                    "[cargo-skuld-nextest] warning: failed to wait on {:?}: {e}; skipping",
                    binary.binary_path
                );
                continue;
            }
        };

        if !status.success() {
            let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
            eprintln!(
                "[cargo-skuld-nextest] warning: {:?} exited with {:?} on --list; skipping (stderr: {stderr})",
                binary.binary_path,
                status.code()
            );
            continue;
        }
        if !dump_path.exists() {
            // The one legitimate silent case: --list succeeded, the binary
            // just isn't Skuld-based.
            continue;
        }
        let contents = match std::fs::read_to_string(&dump_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "[cargo-skuld-nextest] warning: failed to read metadata dump for {:?}: {e}; skipping",
                    binary.binary_path
                );
                continue;
            }
        };
        let dump: Dump = match serde_json::from_str(&contents) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "[cargo-skuld-nextest] warning: malformed metadata dump for {:?}: {e}; skipping",
                    binary.binary_path
                );
                continue;
            }
        };

        for m in dump.tests {
            if !m.serial_filter.is_empty() && m.serial_filter != "*" {
                if let Err(e) = skuld::LabelFilter::parse(&m.serial_filter) {
                    eprintln!(
                        "[cargo-skuld-nextest] warning: test {:?} in {:?} has an unparsable serial_filter {:?}: {e}; excluding it from the conflict graph",
                        m.name, binary.binary_path, m.serial_filter
                    );
                    continue;
                }
            }
            all.push(TestMetadata {
                binary_id: binary.binary_id.clone(),
                name: m.name,
                labels: m.labels,
                serial_filter: m.serial_filter,
            });
        }
    }
    Ok(all)
}

#[cfg(test)]
mod metadata_tests;
