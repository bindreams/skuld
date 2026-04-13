use crate::version::{is_valid_next, workspace_version};
use semver::Version;
use std::fs;

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
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
fn missing_macros_dep() {
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
    let err = workspace_version(tmp.path()).unwrap_err().to_string();
    assert!(
        err.contains("missing the skuld-macros dependency"),
        "unexpected error: {err}"
    );
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
    assert!(err.contains("skuld-macros dep pin"), "unexpected error: {err}");
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
