//! Tests for the `metadata` fixture and the `TestMetadata` / `FixtureMetadata` types.

use skuld::fixtures::metadata::metadata;
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
    assert!(
        names.contains(&"metadata"),
        "fixtures should include 'metadata', got {names:?}"
    );
}

#[skuld::test(serial)]
fn metadata_serial_flag(#[fixture(metadata)] meta: &TestMetadata) {
    assert!(
        !meta.serial.is_empty(),
        "test marked serial should report non-empty serial filter"
    );
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
    assert!(fm.serial.is_empty());
    let yaml = fm.to_string();
    assert!(yaml.contains("test_name"));
}

fn always_ok() -> Result<(), String> {
    Ok(())
}

#[skuld::test(requires = [always_ok])]
fn metadata_has_requirements(#[fixture(metadata)] meta: &TestMetadata) {
    assert!(!meta.requires.is_empty(), "should have at least one requirement");
    assert!(meta.requires[0].met);
    assert!(meta.requires[0].name.contains("always_ok"));
}

// Resolved labels ---------------------------------------------------------------------------------

#[skuld::label]
pub const META_OWN: skuld::Label;
#[skuld::label]
pub const META_MOD: skuld::Label;
#[skuld::label]
pub const META_FIXTURE: skuld::Label;
#[skuld::label]
pub const META_INNER: skuld::Label;

#[skuld::fixture(labels = [META_INNER])]
fn metadata_inner_labeled() -> Result<u32, String> {
    Ok(0)
}

#[skuld::fixture(labels = [META_FIXTURE])]
fn metadata_labeled(#[fixture(metadata_inner_labeled)] _inner: &u32) -> Result<u32, String> {
    Ok(0)
}

fn sorted(labels: &[String]) -> Vec<&str> {
    let mut v: Vec<&str> = labels.iter().map(String::as_str).collect();
    v.sort_unstable();
    v
}

#[skuld::test(labels = [META_OWN])]
fn metadata_reports_own_and_transitive_fixture_labels(
    #[fixture(metadata)] meta: &TestMetadata,
    #[fixture(metadata_labeled)] _l: &u32,
) {
    assert_eq!(sorted(&meta.labels), ["meta_fixture", "meta_inner", "meta_own"]);
}

#[skuld::test(labels = [])]
fn metadata_explicit_empty_labels_keep_fixture_labels(
    #[fixture(metadata)] meta: &TestMetadata,
    #[fixture(metadata_labeled)] _l: &u32,
) {
    assert_eq!(sorted(&meta.labels), ["meta_fixture", "meta_inner"]);
}

mod module_default {
    use super::*;

    skuld::default_labels!(super::META_MOD);

    #[skuld::test]
    fn metadata_reports_module_default_and_fixture_labels(
        #[fixture(metadata)] meta: &TestMetadata,
        #[fixture(metadata_labeled)] _l: &u32,
    ) {
        assert_eq!(sorted(&meta.labels), ["meta_fixture", "meta_inner", "meta_mod"]);
    }
}

#[skuld::test]
fn fixture_metadata_reports_carried_labels() {
    let registry = skuld::fixture_registry();
    let outer = FixtureMetadata::from_def(registry.get("metadata_labeled").unwrap());
    assert_eq!(
        outer.labels,
        ["meta_fixture", "meta_inner"],
        "own first, then dependencies'"
    );
    let inner = FixtureMetadata::from_def(registry.get("metadata_inner_labeled").unwrap());
    assert_eq!(inner.labels, ["meta_inner"]);
}
