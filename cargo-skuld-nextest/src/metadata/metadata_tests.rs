use super::*;
use crate::discovery::discover_binaries;
use std::path::{Path, PathBuf};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/test-workspace")
}

/// Builds the named `crate-d-broken` bin and returns its real compiled
/// path, parsed from `--message-format=json`'s `compiler-artifact`
/// events rather than assumed at `<root>/target/debug/<name>` — a
/// global cargo config or an inherited `CARGO_TARGET_DIR` can redirect
/// build output elsewhere, and a hard-coded path silently breaks in
/// that case (review round 3 fix: correctness finding 9ba16e98).
fn build_and_locate_broken_binary(name: &str) -> PathBuf {
    let root = fixture_root();
    let output = Command::new("cargo")
        .current_dir(&root)
        .args([
            "build",
            "--package",
            "fixture-crate-d-broken",
            "--bin",
            name,
            "--message-format=json",
        ])
        .output()
        .expect("build broken fixture binary");
    assert!(
        output.status.success(),
        "cargo build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if msg["reason"] == "compiler-artifact" && msg["target"]["name"] == name {
            if let Some(exe) = msg["executable"].as_str() {
                return PathBuf::from(exe);
            }
        }
    }
    panic!("cargo build --message-format=json did not report an executable for bin {name}");
}

#[test]
fn collects_metadata_across_both_skuld_fixture_binaries() {
    let binaries = discover_binaries(&fixture_root()).expect("discovery");
    let metadata = collect_metadata(&binaries).expect("collection");
    let find = |name: &str| {
        metadata
            .iter()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("{name} missing from {metadata:?}"))
    };

    assert_eq!(find("a_uses_shared_resource").labels, vec!["shared".to_string()]);
    assert_eq!(find("a_uses_shared_resource").serial_filter, "");
    assert_eq!(find("b_locks_shared_resource").serial_filter, "shared");
    assert_ne!(
        find("a_uses_shared_resource").binary_id,
        find("b_locks_shared_resource").binary_id
    );
}

#[test]
fn non_skuld_binary_is_silently_skipped_without_error() {
    let binaries = discover_binaries(&fixture_root()).expect("discovery");
    assert!(binaries.iter().any(|b| b.binary_id.contains("fixture-crate-c-plain")));
    let metadata = collect_metadata(&binaries).expect("collection must not error on a non-Skuld binary");
    assert!(!metadata.iter().any(|m| m.name == "plain_libtest_test"));
    assert!(metadata.iter().any(|m| m.name == "a_uses_shared_resource"));
}

#[test]
fn nonzero_exit_binary_contributes_nothing_and_does_not_error() {
    let path = build_and_locate_broken_binary("broken-nonzero-exit");
    let binaries = vec![DiscoveredBinary {
        binary_id: "broken".into(),
        binary_path: path,
    }];
    assert!(collect_metadata(&binaries)
        .expect("must not error, only warn")
        .is_empty());
}

#[test]
fn malformed_json_dump_contributes_nothing_and_does_not_error() {
    let path = build_and_locate_broken_binary("broken-bad-json");
    let binaries = vec![DiscoveredBinary {
        binary_id: "broken".into(),
        binary_path: path,
    }];
    assert!(collect_metadata(&binaries)
        .expect("must not error, only warn")
        .is_empty());
}

#[test]
fn unreadable_dump_contributes_nothing_and_does_not_error() {
    let path = build_and_locate_broken_binary("broken-dir-dump");
    let binaries = vec![DiscoveredBinary {
        binary_id: "broken".into(),
        binary_path: path,
    }];
    assert!(collect_metadata(&binaries)
        .expect("must not error, only warn")
        .is_empty());
}

#[test]
fn hanging_binary_is_killed_after_timeout_and_does_not_error() {
    let path = build_and_locate_broken_binary("broken-hangs");
    let binaries = vec![DiscoveredBinary {
        binary_id: "broken".into(),
        binary_path: path,
    }];
    let metadata = collect_metadata_with_timeout(&binaries, Duration::from_millis(300))
        .expect("must not error, only warn, even on timeout");
    assert!(metadata.is_empty());
}

#[test]
fn test_with_unparsable_serial_filter_is_excluded_but_siblings_survive() {
    let path = build_and_locate_broken_binary("broken-bad-serial-filter");
    let binaries = vec![DiscoveredBinary {
        binary_id: "broken".into(),
        binary_path: path,
    }];
    let metadata = collect_metadata(&binaries).expect("must not error, only warn and exclude the one bad test");
    assert!(
        metadata.iter().any(|m| m.name == "good_test"),
        "sibling test in the same dump must survive"
    );
    assert!(
        !metadata.iter().any(|m| m.name == "bad_test"),
        "test with unparsable serial_filter must be excluded"
    );
}

/// The narrowest-scope contract ("skip the offending binary/test, not
/// the whole run") is only actually exercised by a call that also
/// contains a *healthy* binary — a single-broken-binary call can't
/// distinguish "skip this one" from "abandon the whole run", since both
/// produce the same empty result (review round 3 fix: failure finding
/// 9a0d90b6).
#[test]
fn broken_binary_does_not_affect_collection_of_other_binaries_in_the_same_call() {
    let mut binaries = discover_binaries(&fixture_root()).expect("discovery");
    binaries.push(DiscoveredBinary {
        binary_id: "broken".into(),
        binary_path: build_and_locate_broken_binary("broken-nonzero-exit"),
    });
    let metadata = collect_metadata(&binaries).expect("must not error even with a broken binary mixed in");
    assert!(
        metadata.iter().any(|m| m.name == "a_uses_shared_resource"),
        "healthy binaries' tests must still be collected when a broken binary is also present"
    );
}
