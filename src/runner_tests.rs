//! Tests for the serial coordination integration in [`crate::runner`].
//!
//! The core coordination logic (can_start, register, concurrent access) is
//! tested in [`crate::coordination::coordination_tests`]. These tests verify
//! that the runner correctly wires up coordination for test execution.

use crate::runner::{check_duplicate_trial_names, effective_trial_name, ensure_valid_thread_name, TestRunner};

// Trial-name derivation and duplicate detection =====

#[test]
fn effective_trial_name_keeps_the_full_nested_module_path() {
    // A module two levels deep: the crate name ("skuld"), then "a", then
    // "b". Only the crate name is stripped — every intermediate segment
    // must survive. A bug that keeps only the *immediate* parent module
    // (e.g. swapping `split_once` for `rsplit_once`) would produce
    // "b::same" here instead, and every single-level fixture in this
    // repo's `tests/support_bins/` is too shallow to catch that: they all
    // have modules with at most one `::`, where the two splits agree.
    let name = effective_trial_name("skuld::a::b", "same", None, true);
    assert_eq!(name, "a::b::same");
}

#[test]
fn effective_trial_name_crate_root_keeps_the_bare_name() {
    // A crate-root test's module is just the crate name, with no `::` at
    // all — `libtest_names` has nothing to strip down to.
    let name = effective_trial_name("skuld", "top", None, true);
    assert_eq!(name, "top");
}

#[test]
fn effective_trial_name_display_name_wins_over_libtest_names() {
    let name = effective_trial_name("skuld::a::b", "same", Some("custom"), true);
    assert_eq!(name, "custom");
}

#[test]
fn effective_trial_name_bare_name_without_libtest_names() {
    let name = effective_trial_name("skuld::a::b", "same", None, false);
    assert_eq!(name, "same");
}

// Fresh, named thread per trial =====

#[test]
#[should_panic(expected = "skuld: trial name \"bad\\0name\" contains a NUL byte and can't be used as a thread name")]
fn ensure_valid_thread_name_rejects_an_interior_nul_byte() {
    ensure_valid_thread_name("bad\0name");
}

#[test]
fn ensure_valid_thread_name_accepts_an_ordinary_name() {
    ensure_valid_thread_name("a::b::ordinary_name");
}

#[test]
fn check_duplicate_trial_names_reports_every_origin_for_every_duplicate() {
    // The two origins are deliberately not substrings of one another
    // (unlike e.g. "skuld::a::same" / "skuld::a::same (dyn)" would be), so
    // that a bug dropping one origin from the message can't pass by
    // accident because the other origin's string happens to contain it.
    let entries = vec![
        ("a::same".to_string(), "skuld::a::same".to_string()),
        ("a::same".to_string(), "skuld::b::same (dyn)".to_string()),
        ("unique".to_string(), "skuld::unique".to_string()),
    ];
    let err = check_duplicate_trial_names(&entries).expect_err("a repeated name must be rejected");
    assert!(err.contains("a::same"), "message should name the duplicate: {err}");
    assert!(
        err.contains("skuld::a::same"),
        "message should list the first origin: {err}"
    );
    assert!(
        err.contains("skuld::b::same (dyn)"),
        "message should list the second origin: {err}"
    );
    assert!(
        !err.contains("unique"),
        "a name with no duplicate must not be reported: {err}"
    );
}

#[test]
fn check_duplicate_trial_names_accepts_all_unique_names() {
    let entries = vec![
        ("a".to_string(), "origin_a".to_string()),
        ("b".to_string(), "origin_b".to_string()),
    ];
    assert!(check_duplicate_trial_names(&entries).is_ok());
}

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
