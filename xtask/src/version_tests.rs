use crate::version::{
    assert_intra_workspace_pins, dep_version_req, is_valid_next, nearest_ancestor_version_tags,
    validate_cargo_against_nearest, workspace_version, MemberInfo, TagInfo,
};
use gix::ObjectId;
use semver::Version;
use std::fs;
use std::path::Path;
use std::process::Command;

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

fn tag(name: &str) -> TagInfo {
    let version = v(name.strip_prefix('v').unwrap());
    TagInfo {
        name: name.to_string(),
        sha: ObjectId::null(gix::hash::Kind::Sha1),
        version,
    }
}

// is_valid_next =======================================================================================================

#[test]
fn valid_next_same() {
    assert!(is_valid_next(&v("1.2.3"), &v("1.2.3")));
}
#[test]
fn valid_next_patch() {
    assert!(is_valid_next(&v("1.2.3"), &v("1.2.4")));
}
#[test]
fn valid_next_minor() {
    assert!(is_valid_next(&v("1.2.3"), &v("1.3.0")));
}
#[test]
fn valid_next_major() {
    assert!(is_valid_next(&v("1.2.3"), &v("2.0.0")));
}
#[test]
fn invalid_skip_patch() {
    assert!(!is_valid_next(&v("1.2.3"), &v("1.2.5")));
}
#[test]
fn invalid_skip_minor() {
    assert!(!is_valid_next(&v("1.2.3"), &v("1.4.0")));
}
#[test]
fn invalid_backward() {
    assert!(!is_valid_next(&v("1.2.3"), &v("1.2.2")));
}
#[test]
fn invalid_minor_without_patch_reset() {
    assert!(!is_valid_next(&v("1.2.3"), &v("1.3.1")));
}

// workspace_version ===================================================================================================

fn write_workspace(dir: &std::path::Path, root: &str, macros: &str) {
    fs::create_dir_all(dir.join("macros")).unwrap();
    fs::write(dir.join("Cargo.toml"), root).unwrap();
    fs::write(dir.join("macros/Cargo.toml"), macros).unwrap();
}

const MACROS_OK: &str = r#"
[package]
name = "skuld-macros"
version = "0.1.0"
edition = "2021"
"#;

#[test]
fn happy_path() {
    let tmp = tempfile::tempdir().unwrap();
    write_workspace(
        tmp.path(),
        r#"
[workspace]
members = [".", "macros"]

[package]
name = "skuld"
version = "0.1.0"
edition = "2021"

[dependencies]
skuld-macros = { version = "=0.1.0", path = "macros" }
"#,
        MACROS_OK,
    );
    let v = workspace_version(tmp.path()).unwrap();
    assert_eq!(v, Version::new(0, 1, 0));
}

#[test]
fn members_disagree() {
    let tmp = tempfile::tempdir().unwrap();
    write_workspace(
        tmp.path(),
        r#"
[workspace]
members = [".", "macros"]

[package]
name = "skuld"
version = "0.1.0"
edition = "2021"

[dependencies]
skuld-macros = { version = "=0.1.0", path = "macros" }
"#,
        r#"
[package]
name = "skuld-macros"
version = "0.2.0"
edition = "2021"
"#,
    );
    let err = workspace_version(tmp.path()).unwrap_err().to_string();
    assert!(err.contains("disagree"), "unexpected error: {err}");
}

#[test]
fn pre_release_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    write_workspace(
        tmp.path(),
        r#"
[workspace]
members = [".", "macros"]

[package]
name = "skuld"
version = "0.1.0-beta"
edition = "2021"

[dependencies]
skuld-macros = { version = "=0.1.0-beta", path = "macros" }
"#,
        MACROS_OK,
    );
    let err = workspace_version(tmp.path()).unwrap_err().to_string();
    assert!(err.contains("strict MAJOR.MINOR.PATCH"), "unexpected error: {err}");
}

#[test]
fn an_absent_sibling_dependency_is_not_a_version_error() {
    let tmp = tempfile::tempdir().unwrap();
    write_workspace(
        tmp.path(),
        r#"
[workspace]
members = [".", "macros"]

[package]
name = "skuld"
version = "0.1.0"
edition = "2021"

[dependencies]
# skuld-macros intentionally missing
"#,
        MACROS_OK,
    );
    // The pin check constrains the dependencies a member declares; it does not
    // mandate that a particular one exists. Requiring skuld-macros by name is
    // what this check was generalized away from, and the compiler already
    // enforces presence — skuld does not build without its macros.
    assert_eq!(workspace_version(tmp.path()).unwrap(), v("0.1.0"));
}

#[test]
fn wrong_pin_version() {
    let tmp = tempfile::tempdir().unwrap();
    write_workspace(
        tmp.path(),
        r#"
[workspace]
members = [".", "macros"]

[package]
name = "skuld"
version = "0.1.0"
edition = "2021"

[dependencies]
skuld-macros = { version = "=0.0.9", path = "macros" }
"#,
        MACROS_OK,
    );
    let err = workspace_version(tmp.path()).unwrap_err().to_string();
    assert!(
        err.contains("is pinned '=0.0.9', expected '=0.1.0'"),
        "unexpected error: {err}"
    );
}

#[test]
fn non_exact_pin_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    write_workspace(
        tmp.path(),
        r#"
[workspace]
members = [".", "macros"]

[package]
name = "skuld"
version = "0.1.0"
edition = "2021"

[dependencies]
skuld-macros = { version = "0.1.0", path = "macros" }
"#,
        MACROS_OK,
    );
    let err = workspace_version(tmp.path()).unwrap_err().to_string();
    assert!(err.contains("expected '=0.1.0'"), "unexpected error: {err}");
}

// validate_cargo_against_nearest ======================================================================================

#[test]
fn empty_nonexact_passes() {
    validate_cargo_against_nearest(&v("1.0.0"), &[], false).unwrap();
}

#[test]
fn empty_exact_fails() {
    let err = validate_cargo_against_nearest(&v("1.0.0"), &[], true)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no ancestor"), "unexpected error: {err}");
}

#[test]
fn single_exact_match() {
    validate_cargo_against_nearest(&v("1.2.3"), &[tag("v1.2.3")], true).unwrap();
}

#[test]
fn single_exact_mismatch() {
    validate_cargo_against_nearest(&v("1.2.4"), &[tag("v1.2.3")], true).unwrap_err();
}

#[test]
fn single_nonexact_same() {
    validate_cargo_against_nearest(&v("1.2.3"), &[tag("v1.2.3")], false).unwrap();
}

#[test]
fn single_nonexact_patch_bump() {
    validate_cargo_against_nearest(&v("1.2.4"), &[tag("v1.2.3")], false).unwrap();
}

#[test]
fn single_nonexact_minor_bump() {
    validate_cargo_against_nearest(&v("1.3.0"), &[tag("v1.2.3")], false).unwrap();
}

#[test]
fn single_nonexact_major_bump() {
    validate_cargo_against_nearest(&v("2.0.0"), &[tag("v1.2.3")], false).unwrap();
}

#[test]
fn single_nonexact_double_patch_rejected() {
    validate_cargo_against_nearest(&v("1.2.5"), &[tag("v1.2.3")], false).unwrap_err();
}

#[test]
fn single_nonexact_backward_rejected() {
    validate_cargo_against_nearest(&v("1.2.2"), &[tag("v1.2.3")], false).unwrap_err();
}

fn merged_branches() -> Vec<TagInfo> {
    vec![tag("v1.2.4"), tag("v2.0.0")]
}

#[test]
fn merged_branches_nonexact_ok_vs_lower() {
    validate_cargo_against_nearest(&v("1.3.0"), &merged_branches(), false).unwrap();
}

#[test]
fn merged_branches_nonexact_ok_vs_higher() {
    validate_cargo_against_nearest(&v("2.0.1"), &merged_branches(), false).unwrap();
}

#[test]
fn merged_branches_nonexact_between() {
    validate_cargo_against_nearest(&v("1.2.5"), &merged_branches(), false).unwrap();
}

#[test]
fn merged_branches_nonexact_none_match() {
    validate_cargo_against_nearest(&v("3.1.0"), &merged_branches(), false).unwrap_err();
}

#[test]
fn merged_branches_exact_matches_one() {
    validate_cargo_against_nearest(&v("2.0.0"), &merged_branches(), true).unwrap();
}

#[test]
fn merged_branches_exact_matches_none() {
    validate_cargo_against_nearest(&v("1.5.0"), &merged_branches(), true).unwrap_err();
}

// nearest_ancestor_version_tags =======================================================================================
//
// Integration tests: build a tiny real git repo in a tempdir via subprocess
// `git`, then read it via gix through `nearest_ancestor_version_tags`.

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .status()
        .expect("failed to spawn git");
    assert!(status.success(), "git {} failed", args.join(" "));
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "--initial-branch=main"]);
    git(dir, &["config", "user.email", "test@test"]);
    git(dir, &["config", "user.name", "test"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    git(dir, &["config", "tag.gpgsign", "false"]);
}

fn commit(dir: &Path, msg: &str) {
    // Empty commits work with --allow-empty; cheaper than file churn.
    git(dir, &["commit", "-q", "--allow-empty", "-m", msg]);
}

fn tag_name_set(tags: &[TagInfo]) -> Vec<String> {
    let mut names: Vec<String> = tags.iter().map(|t| t.name.clone()).collect();
    names.sort();
    names
}

#[test]
fn integration_no_tags() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    commit(tmp.path(), "initial");
    let nearest = nearest_ancestor_version_tags(tmp.path()).unwrap();
    assert_eq!(nearest.len(), 0);
}

#[test]
fn integration_single_tag() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    commit(tmp.path(), "initial");
    git(tmp.path(), &["tag", "v1.0.0"]);
    let nearest = nearest_ancestor_version_tags(tmp.path()).unwrap();
    assert_eq!(tag_name_set(&nearest), vec!["v1.0.0".to_string()]);
}

#[test]
fn integration_linear_hides_earlier() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    commit(tmp.path(), "c1");
    git(tmp.path(), &["tag", "v1.0.0"]);
    commit(tmp.path(), "c2");
    git(tmp.path(), &["tag", "v1.1.0"]);
    let nearest = nearest_ancestor_version_tags(tmp.path()).unwrap();
    assert_eq!(tag_name_set(&nearest), vec!["v1.1.0".to_string()]);
}

#[test]
fn integration_two_tags_same_commit() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    commit(tmp.path(), "c1");
    git(tmp.path(), &["tag", "v1.0.0"]);
    git(tmp.path(), &["tag", "v1.0.1"]);
    let nearest = nearest_ancestor_version_tags(tmp.path()).unwrap();
    assert_eq!(tag_name_set(&nearest), vec!["v1.0.0".to_string(), "v1.0.1".to_string()]);
}

#[test]
fn integration_merge_two_branches() {
    // The worked example from the plan:
    //   main:    M1 -- M2 (tag v2.0.0)
    //              \
    //               \ (branch)
    //                F1 (tag v1.2.4) -- H (merge M2 into feature)
    // HEAD = H. Nearest must be {v1.2.4, v2.0.0}.
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    commit(tmp.path(), "M1");
    git(tmp.path(), &["tag", "v1.2.3"]);
    git(tmp.path(), &["checkout", "-q", "-b", "feature"]);
    commit(tmp.path(), "F1");
    git(tmp.path(), &["tag", "v1.2.4"]);
    git(tmp.path(), &["checkout", "-q", "main"]);
    commit(tmp.path(), "M2");
    git(tmp.path(), &["tag", "v2.0.0"]);
    git(tmp.path(), &["checkout", "-q", "feature"]);
    git(tmp.path(), &["merge", "-q", "--no-ff", "-m", "merge main", "main"]);
    let nearest = nearest_ancestor_version_tags(tmp.path()).unwrap();
    assert_eq!(tag_name_set(&nearest), vec!["v1.2.4".to_string(), "v2.0.0".to_string()]);
}

#[test]
fn integration_non_semver_tag_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    commit(tmp.path(), "c1");
    git(tmp.path(), &["tag", "foo"]);
    git(tmp.path(), &["tag", "v1.0.0"]);
    let nearest = nearest_ancestor_version_tags(tmp.path()).unwrap();
    assert_eq!(tag_name_set(&nearest), vec!["v1.0.0".to_string()]);
}

#[test]
fn integration_prerelease_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    commit(tmp.path(), "c1");
    git(tmp.path(), &["tag", "v0.9.0"]);
    commit(tmp.path(), "c2");
    git(tmp.path(), &["tag", "v1.0.0-beta"]);
    let nearest = nearest_ancestor_version_tags(tmp.path()).unwrap();
    assert_eq!(tag_name_set(&nearest), vec!["v0.9.0".to_string()]);
}

// Intra-workspace pin checks ==========================================================================================

fn member(name: &str, deps: &[(&str, toml::Value)]) -> MemberInfo {
    MemberInfo {
        path: format!("{name}/Cargo.toml"),
        name: name.to_string(),
        dependencies: deps.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
    }
}

#[test]
fn dep_version_req_reads_both_declaration_forms() {
    assert_eq!(
        dep_version_req(&toml::Value::String("=1.2.3".into())),
        Some("=1.2.3".into())
    );
    let mut t = toml::value::Table::new();
    t.insert("version".into(), toml::Value::String("=1.2.3".into()));
    t.insert("path".into(), toml::Value::String("..".into()));
    assert_eq!(dep_version_req(&toml::Value::Table(t)), Some("=1.2.3".into()));
}

#[test]
fn a_path_only_dependency_declares_no_requirement() {
    let mut t = toml::value::Table::new();
    t.insert("path".into(), toml::Value::String("..".into()));
    assert_eq!(dep_version_req(&toml::Value::Table(t)), None);
}

#[test]
fn an_exact_pin_on_a_sibling_passes() {
    let members = vec![
        member("lib", &[]),
        member("cli", &[("lib", toml::Value::String("=1.2.3".into()))]),
    ];
    assert!(assert_intra_workspace_pins(&members, &v("1.2.3")).is_ok());
}

#[test]
fn a_loose_pin_on_a_sibling_is_rejected() {
    // The gap this check exists to close: a loose pin builds and tests fine
    // against the path dependency, so nothing else in the pipeline catches it.
    let members = vec![
        member("lib", &[]),
        member("cli", &[("lib", toml::Value::String("1.2".into()))]),
    ];
    let err = assert_intra_workspace_pins(&members, &v("1.2.3"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("pinned '1.2'"), "{err}");
}

#[test]
fn a_dependency_outside_the_workspace_is_not_constrained() {
    let members = vec![member("lib", &[("serde", toml::Value::String("1".into()))])];
    assert!(assert_intra_workspace_pins(&members, &v("1.2.3")).is_ok());
}
