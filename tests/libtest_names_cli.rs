//! End-to-end tests for opt-in libtest-style trial names, and a hard error
//! on duplicate final trial names. Out-of-harness because each
//! scenario either inspects `--list` output or observes a startup panic
//! that ends the process with a failure exit status — neither can be
//! asserted in-process without contaminating the real `inventory` registry
//! used by skuld's own dogfooded `#[skuld::test]`s.

use std::process::Command;

#[test]
fn libtest_names_match_libtest() {
    let out = Command::new(env!("CARGO_BIN_EXE_libtest_names"))
        .arg("--list")
        .output()
        .expect("spawn libtest_names");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut names: Vec<&str> = stdout.lines().filter_map(|l| l.strip_suffix(": test")).collect();
    names.sort_unstable();

    assert_eq!(
        names,
        vec!["a::same", "b::same", "top"],
        "full --list output:\n{stdout}"
    );
}

#[test]
fn duplicate_names_panic_at_startup() {
    let out = Command::new(env!("CARGO_BIN_EXE_duplicate_names"))
        .output()
        .expect("spawn duplicate_names");
    assert!(!out.status.success(), "expected a startup panic, got success");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("panicked at"), "expected a panic; stderr:\n{stderr}");
    assert!(
        stderr.contains("\"same\""),
        "expected the duplicate name 'same'; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("::a::same") && stderr.contains("::b::same"),
        "expected both modules 'a' and 'b' named in the panic; stderr:\n{stderr}"
    );
}

#[test]
fn duplicate_display_names_panic() {
    let out = Command::new(env!("CARGO_BIN_EXE_duplicate_display_names"))
        .output()
        .expect("spawn duplicate_display_names");
    assert!(!out.status.success(), "expected a startup panic, got success");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("\"dup\""),
        "expected the duplicate name 'dup'; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("::a::one") && stderr.contains("::b::two"),
        "expected both origins named in the panic; stderr:\n{stderr}"
    );
}

#[test]
fn display_name_equal_to_a_libtest_path_panics() {
    let out = Command::new(env!("CARGO_BIN_EXE_display_name_collides_with_libtest_path"))
        .output()
        .expect("spawn display_name_collides_with_libtest_path");
    assert!(!out.status.success(), "expected a startup panic, got success");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("\"a::same\""),
        "expected the colliding computed name 'a::same'; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("::a::same") && stderr.contains("::collider"),
        "expected both origins named in the panic; stderr:\n{stderr}"
    );
}

#[test]
fn dynamic_name_equal_to_an_inventory_name_panics() {
    let out = Command::new(env!("CARGO_BIN_EXE_dynamic_name_collides_with_inventory"))
        .output()
        .expect("spawn dynamic_name_collides_with_inventory");
    assert!(!out.status.success(), "expected a startup panic, got success");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("\"clash\""),
        "expected the duplicate name 'clash'; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("::clash"),
        "expected the inventory test's origin named in the panic; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("dynamically-added"),
        "expected the dynamic test's origin described in the panic; stderr:\n{stderr}"
    );
}

#[test]
fn duplicate_check_fires_even_when_every_colliding_entry_is_ignored() {
    // The check validates the *set* of final trial names, not which of them
    // would actually run: an inventory test and a dynamic test, both
    // `#[ignore]`d, sharing a name must still panic at startup.
    let out = Command::new(env!("CARGO_BIN_EXE_duplicate_names_ignored"))
        .output()
        .expect("spawn duplicate_names_ignored");
    assert!(!out.status.success(), "expected a startup panic, got success");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("\"clash\""),
        "expected the duplicate name 'clash'; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("::clash"),
        "expected the inventory test's origin named in the panic; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("dynamically-added"),
        "expected the dynamic test's origin described in the panic; stderr:\n{stderr}"
    );
}
