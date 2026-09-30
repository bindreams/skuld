//! End-to-end tests for `SKULD_NEXTEST_METADATA_PATH`.

use std::process::Command;

fn spawn_list(meta_path: Option<&std::path::Path>) -> (bool, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_label_filter_fixture"));
    cmd.arg("--list");
    for key in ["SKULD_LABELS", "SKULD_DEBUG", "SKULD_NEXTEST_METADATA_PATH"] {
        cmd.env_remove(key);
    }
    if let Some(p) = meta_path {
        cmd.env("SKULD_NEXTEST_METADATA_PATH", p);
    }
    let out = cmd.output().expect("spawn label_filter_fixture");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn dump_matches_declared_tests_and_excludes_ignored() {
    let dir = skuld::TempDir::new().expect("tempdir");
    let meta_path = dir.path().join("meta.json");

    let (ok, _stdout, stderr) = spawn_list(Some(&meta_path));
    assert!(ok, "stderr: {stderr}");

    let contents = std::fs::read_to_string(&meta_path).expect("metadata file must exist after --list");
    let dump: serde_json::Value = serde_json::from_str(&contents).expect("valid JSON");
    let tests = dump["tests"].as_array().expect("tests array");

    let find = |name: &str| tests.iter().find(|t| t["name"] == name);

    let none = find("t_none").unwrap_or_else(|| panic!("t_none missing from {tests:#?}"));
    assert_eq!(none["serial_filter"], "");
    assert_eq!(none["labels"].as_array().unwrap().len(), 0);

    let serial_fast = find("t_serial_fast").unwrap_or_else(|| panic!("t_serial_fast missing"));
    assert_eq!(serial_fast["serial_filter"], "*");
    assert_eq!(serial_fast["labels"][0], "fast");

    assert_eq!(find("t_serial_filter_fast").unwrap()["serial_filter"], "fast");

    // Ignored/unavailable tests never run under a normal invocation and
    // would over-serialize real tests if included — must be absent.
    assert!(
        find("t_outer_ignored_fast").is_none(),
        "statically-ignored tests must NOT appear in the dump (over-serialization risk)"
    );
    assert!(
        find("t_req_unmet_fast").is_none(),
        "unavailable tests must NOT appear in the dump"
    );
}

#[test]
fn no_file_written_when_env_unset() {
    let dir = skuld::TempDir::new().expect("tempdir");
    let meta_path = dir.path().join("meta.json");
    let (ok, _, _) = spawn_list(None);
    assert!(ok);
    assert!(
        !meta_path.exists(),
        "no metadata file should be written when the env var is unset"
    );
}

#[test]
fn list_stdout_and_exit_code_unaffected_by_metadata_dump_flag() {
    let (ok_without, stdout_without, _) = spawn_list(None);
    let dir = skuld::TempDir::new().expect("tempdir");
    let meta_path = dir.path().join("meta.json");
    let (ok_with, stdout_with, _) = spawn_list(Some(&meta_path));
    assert_eq!(ok_without, ok_with);
    assert_eq!(
        stdout_without, stdout_with,
        "--list stdout must be byte-identical regardless of the metadata-dump env var"
    );
    assert!(
        meta_path.exists(),
        "sanity: the dump-triggering run did produce the side file"
    );
}
