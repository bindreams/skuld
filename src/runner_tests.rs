//! Tests for the serial coordination integration in [`crate::runner`].
//!
//! The core coordination logic (can_start, register, concurrent access) is
//! tested in [`crate::coordination::coordination_tests`]. These tests verify
//! that the runner correctly wires up coordination for test execution.

use crate::runner::TestRunner;

#[test]
fn collect_dynamic_tests_populates_metadata_excluding_ignored() {
    let mut runner = TestRunner::new();
    runner.add("plain", &[], false, || {});
    runner.add("ignored", &[], true, || {});
    runner.add_serial("global_serial", &[], false, || {});
    let db_label = crate::label::Label::__new("dbtest");
    runner.add_serial_with(
        "filtered_serial",
        &[db_label],
        false,
        crate::LabelFilter::parse("dbtest").unwrap(),
        || {},
    );

    let mut trials = Vec::new();
    let mut metadata = Vec::new();
    runner.collect_dynamic_tests(None, true, &mut trials, &mut metadata);

    let names: Vec<&str> = metadata.iter().map(|m| m.name.as_str()).collect();
    assert!(
        !names.contains(&"ignored"),
        "ignored dynamic tests must not appear in the dump: {names:?}"
    );
    assert_eq!(names.len(), 3);

    let by_name = |n: &str| metadata.iter().find(|m| m.name == n).unwrap();
    assert_eq!(by_name("plain").serial_filter, "");
    assert!(by_name("plain").labels.is_empty());
    assert_eq!(by_name("global_serial").serial_filter, "*");
    assert_eq!(by_name("filtered_serial").serial_filter, "dbtest");
    assert_eq!(by_name("filtered_serial").labels, vec!["dbtest".to_string()]);
}

#[test]
fn write_nextest_metadata_produces_expected_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("meta.json");
    crate::runner::write_nextest_metadata(
        &path,
        vec![crate::runner::NextestTestMetadata {
            name: "t".into(),
            labels: vec!["a".into()],
            serial_filter: "*".into(),
        }],
    );
    let contents = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
    assert_eq!(parsed["tests"][0]["name"], "t");
    assert_eq!(parsed["tests"][0]["labels"][0], "a");
    assert_eq!(parsed["tests"][0]["serial_filter"], "*");
}

#[test]
fn write_nextest_metadata_handles_empty_list() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("meta.json");
    crate::runner::write_nextest_metadata(&path, vec![]);
    let parsed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed["tests"].as_array().unwrap().len(), 0);
}
