use super::*;

#[test]
fn empty_groups_produce_parseable_toml_with_no_overrides() {
    let rendered = render_tool_config(&[]);
    let value: toml::Value = rendered.parse().expect("must be valid TOML");
    assert!(value.get("test-groups").is_none() || value["test-groups"].as_table().unwrap().is_empty());
}

#[test]
fn one_group_roundtrips_with_expected_shape() {
    let groups = vec![TestGroup {
        name: "skuld_group_0".to_string(),
        members: vec![
            ("bin-a".to_string(), "locker".to_string()),
            ("bin-b".to_string(), "user".to_string()),
        ],
    }];
    let rendered = render_tool_config(&groups);
    let value: toml::Value = rendered.parse().expect("must be valid TOML");
    assert_eq!(
        value["test-groups"]["skuld_group_0"]["max-threads"].as_integer(),
        Some(1)
    );
    let overrides = value["profile"]["default"]["overrides"].as_array().unwrap();
    assert_eq!(overrides.len(), 1);
    let filter = overrides[0]["filter"].as_str().unwrap();
    assert!(filter.contains("binary_id(=bin-a)") && filter.contains("test(=locker)"));
    assert!(filter.contains("binary_id(=bin-b)") && filter.contains("test(=user)"));
    assert_eq!(overrides[0]["test-group"].as_str(), Some("skuld_group_0"));
    assert_eq!(value["nextest-version"]["required"].as_str(), Some(MIN_NEXTEST_VERSION));
}

/// Covers every documented escape sequence, not just comma/paren.
#[test]
fn filter_escapes_every_documented_sequence() {
    let groups = vec![TestGroup {
        name: "g".to_string(),
        members: vec![("b,\\/\n\t\rid".to_string(), "weird test (name)".to_string())],
    }];
    let rendered = render_tool_config(&groups);
    let value: toml::Value = rendered.parse().expect("must be valid TOML");
    let filter = value["profile"]["default"]["overrides"][0]["filter"].as_str().unwrap();
    assert!(
        filter.contains("b\\,\\\\\\/\\n\\t\\rid"),
        "all seven escapes must be applied: {filter}"
    );
    assert!(
        filter.contains("weird test (name\\))"),
        "unescaped '(' passes through, ')' is escaped: {filter}"
    );
}
