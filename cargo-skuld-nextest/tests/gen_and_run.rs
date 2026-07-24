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
