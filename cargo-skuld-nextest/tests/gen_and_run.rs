use std::path::Path;
use std::process::Command;

fn fixture_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/test-workspace")
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cargo-skuld-nextest")
}

#[test]
fn gen_writes_groups_for_the_shared_resource_and_weird_name_conflicts() {
    let out_dir = tempfile::tempdir().expect("tempdir");
    let output = out_dir.path().join("skuld-nextest.toml");

    let status = Command::new(bin())
        .current_dir(fixture_root())
        .args(["gen", "--output"])
        .arg(&output)
        .status()
        .expect("spawn gen");
    assert!(status.success());

    let rendered = std::fs::read_to_string(&output).expect("output file must exist");
    let value: toml::Value = rendered.parse().expect("valid TOML");
    let overrides = value["profile"]["default"]["overrides"]
        .as_array()
        .expect("overrides array");
    // Two independent conflicts: the shared-resource pair and the
    // weird-named escape-proof pair.
    assert_eq!(overrides.len(), 2, "expected exactly two conflict groups: {rendered}");
    let filters: Vec<&str> = overrides.iter().map(|o| o["filter"].as_str().unwrap()).collect();
    assert!(filters
        .iter()
        .any(|f| f.contains("a_uses_shared_resource") && f.contains("b_locks_shared_resource")));
    assert!(filters
        .iter()
        .any(|f| f.contains("weird") && f.contains("[a]") && f.contains("[b]")));
}

#[test]
fn gen_check_matches_after_gen() {
    let out_dir = tempfile::tempdir().expect("tempdir");
    let output = out_dir.path().join("skuld-nextest.toml");
    let gen_status = Command::new(bin())
        .current_dir(fixture_root())
        .args(["gen", "--output"])
        .arg(&output)
        .status()
        .expect("spawn gen");
    assert!(gen_status.success());
    let check_status = Command::new(bin())
        .current_dir(fixture_root())
        .args(["gen", "--check", "--output"])
        .arg(&output)
        .status()
        .expect("spawn gen --check");
    assert!(
        check_status.success(),
        "gen --check must succeed immediately after gen with no source changes"
    );
}

#[test]
fn gen_check_fails_on_stale_file() {
    let out_dir = tempfile::tempdir().expect("tempdir");
    let output = out_dir.path().join("skuld-nextest.toml");
    std::fs::write(&output, "# stale, does not match current test set\n").unwrap();
    let check_status = Command::new(bin())
        .current_dir(fixture_root())
        .args(["gen", "--check", "--output"])
        .arg(&output)
        .status()
        .expect("spawn gen --check");
    assert!(!check_status.success(), "gen --check must fail on a stale file");
    assert!(
        std::fs::read_to_string(&output).unwrap().contains("stale"),
        "must not have overwritten the stale file"
    );
}

#[test]
fn gen_on_a_workspace_with_no_skuld_binaries_is_a_harmless_noop() {
    let empty_ws = tempfile::tempdir().expect("tempdir");
    std::fs::write(empty_ws.path().join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    let out_dir = tempfile::tempdir().expect("tempdir");
    let output = out_dir.path().join("skuld-nextest.toml");
    let status = Command::new(bin())
        .current_dir(empty_ws.path())
        .args(["gen", "--output"])
        .arg(&output)
        .status()
        .expect("spawn gen");
    assert!(status.success());
    let value: toml::Value = std::fs::read_to_string(&output)
        .unwrap()
        .parse()
        .expect("valid TOML even with zero binaries");
    assert!(value.get("test-groups").is_none() || value["test-groups"].as_table().unwrap().is_empty());
}

fn read_window(dir: &Path, name: &str) -> (u128, u128) {
    let start: u128 = std::fs::read_to_string(dir.join(format!("{name}-proc-start")))
        .unwrap_or_else(|e| panic!("start marker for {name} missing: {e}"))
        .parse()
        .unwrap();
    let end: u128 = std::fs::read_to_string(dir.join(format!("{name}-proc-end")))
        .unwrap_or_else(|e| panic!("end marker for {name} missing: {e}"))
        .parse()
        .unwrap();
    (start, end)
}

fn windows_overlap(a: (u128, u128), b: (u128, u128)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

#[test]
fn negative_control_two_directly_spawned_processes_overlap() {
    // Bypasses nextest's scheduler entirely — spawns the two conflicting
    // tests' binaries directly, back-to-back, so overlap is guaranteed by
    // construction (microseconds between spawns vs. each test's 200ms
    // body), not by scheduler luck. Validates the MEASUREMENT technique
    // (process-lifetime windows correctly detect two simultaneously-alive
    // processes); the positive case below validates nextest's own
    // scheduling behavior separately.
    let timing_dir = tempfile::tempdir().expect("tempdir");
    let binaries = cargo_skuld_nextest::discovery::discover_binaries(&fixture_root()).expect("discovery");
    let bin_a = &binaries
        .iter()
        .find(|b| b.binary_id.contains("fixture-crate-a"))
        .expect("crate-a binary")
        .binary_path;
    let bin_b = &binaries
        .iter()
        .find(|b| b.binary_id.contains("fixture-crate-b"))
        .expect("crate-b binary")
        .binary_path;

    let mut child_a = Command::new(bin_a)
        .args(["a_uses_shared_resource", "--exact"])
        .env("SKULD_NEXTEST_FIXTURE_TIMING_DIR", timing_dir.path())
        .spawn()
        .expect("spawn crate-a binary directly");
    let mut child_b = Command::new(bin_b)
        .args(["b_locks_shared_resource", "--exact"])
        .env("SKULD_NEXTEST_FIXTURE_TIMING_DIR", timing_dir.path())
        .spawn()
        .expect("spawn crate-b binary directly");
    assert!(child_a.wait().expect("wait a").success());
    assert!(child_b.wait().expect("wait b").success());

    let a = read_window(timing_dir.path(), "a_uses_shared_resource");
    let b = read_window(timing_dir.path(), "b_locks_shared_resource");
    assert!(
        windows_overlap(a, b),
        "methodology check: two processes spawned back-to-back by this test were expected to \
         overlap but did not — a={a:?} b={b:?}"
    );
}

#[test]
fn run_serializes_the_cross_binary_conflict_via_generated_tool_config() {
    // Positive case: with the generated tool-config, nextest must not
    // launch the second process until the first has fully exited.
    let real_dir = tempfile::tempdir().expect("tempdir");
    let status = Command::new(bin())
        .current_dir(fixture_root())
        .env("SKULD_NEXTEST_FIXTURE_TIMING_DIR", real_dir.path())
        .arg("run")
        .status()
        .expect("spawn cargo-skuld-nextest run");
    assert!(
        status.success(),
        "cargo-skuld-nextest run must succeed against the fixture workspace"
    );
    let real_a = read_window(real_dir.path(), "a_uses_shared_resource");
    let real_b = read_window(real_dir.path(), "b_locks_shared_resource");
    assert!(
        !windows_overlap(real_a, real_b),
        "conflicting tests' PROCESSES overlapped even with the generated tool-config: \
         a={real_a:?} b={real_b:?} — nextest did not serialize them via the group"
    );
}

/// Proves `escape_nextest_name`'s output is accepted by REAL nextest
/// parsing, not just by our own string assertions — both weird-named
/// tests must actually execute.
#[test]
fn run_correctly_selects_tests_with_special_characters_in_their_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let status = Command::new(bin())
        .current_dir(fixture_root())
        .env("SKULD_NEXTEST_FIXTURE_TIMING_DIR", dir.path())
        .arg("run")
        .status()
        .expect("spawn run");
    assert!(status.success());
    assert!(
        dir.path().join("weird-a-ran").exists(),
        "the weird-named test in crate-a must have executed"
    );
    assert!(
        dir.path().join("weird-b-ran").exists(),
        "the weird-named test in crate-b must have executed"
    );
}
