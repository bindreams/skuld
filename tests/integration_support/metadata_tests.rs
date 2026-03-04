//! Tests for the `metadata` fixture and the `TestMetadata` / `FixtureMetadata` types.

use skuld::metadata::{FixtureMetadata, TestMetadata};

#[skuld::test]
fn metadata_has_test_name(#[fixture(metadata)] meta: &TestMetadata) {
    assert_eq!(meta.name, "metadata_has_test_name");
}

#[skuld::test]
fn metadata_has_module(#[fixture(metadata)] meta: &TestMetadata) {
    assert!(!meta.module.is_empty());
}

#[skuld::test]
fn metadata_lists_own_fixture(#[fixture(metadata)] meta: &TestMetadata) {
    let names: Vec<&str> = meta.fixtures.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"metadata"), "fixtures should include 'metadata', got {names:?}");
}

#[skuld::test(serial)]
fn metadata_serial_flag(#[fixture(metadata)] meta: &TestMetadata) {
    assert!(meta.serial, "test marked serial should report serial=true");
}

#[skuld::test]
fn metadata_display_is_yaml(#[fixture(metadata)] meta: &TestMetadata) {
    let yaml = meta.to_string();
    assert!(yaml.contains("name:"), "Display should produce YAML with 'name:' key");
    assert!(yaml.contains("metadata_display_is_yaml"));
}

#[skuld::test]
fn fixture_metadata_from_registry() {
    let registry = skuld::fixture_registry();
    let def = registry.get("test_name").expect("test_name fixture should exist");
    let fm = FixtureMetadata::from_def(def);
    assert_eq!(fm.name, "test_name");
    assert_eq!(fm.scope, "test");
    assert!(!fm.serial);
    let yaml = fm.to_string();
    assert!(yaml.contains("test_name"));
}

fn always_ok() -> Result<(), String> {
    Ok(())
}

#[skuld::test(requires = [always_ok])]
fn metadata_has_requirements(#[fixture(metadata)] meta: &TestMetadata) {
    assert!(!meta.requires.is_empty(), "should have at least one requirement");
    assert_eq!(meta.requires[0].met, true);
    assert!(meta.requires[0].name.contains("always_ok"));
}
