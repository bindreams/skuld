//! End-to-end tests pinning fixture teardown behavior for `should_panic`
//! tests, for both the bare (`Yes`) and message-checked (`WithMessage`)
//! arms. Out-of-harness because the read has to survive past the
//! subprocess that took it — a subprocess is also required so a would-be
//! regression (a real double panic, if the probe's own `Drop` ever
//! panicked) can't take this driver down with it.
//!
//! Each test below runs exactly one trial from `panicking_at_drop_probe`
//! via `--exact <name>`, since `body_completes_but_fixture_drop_panics*`
//! are expected to fail the process and must not affect the others' exit
//! status.

use std::process::Command;

fn run_probe_trial(name: &str, out_path: Option<&std::path::Path>) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_panicking_at_drop_probe"));
    cmd.arg(name).arg("--exact");
    if let Some(out_path) = out_path {
        cmd.env("SKULD_PANICKING_AT_DROP_PROBE_OUT", out_path);
    }
    cmd.output()
        .unwrap_or_else(|e| panic!("spawn panicking_at_drop_probe --exact {name}: {e}"))
}

fn assert_drop_order(out_path: &std::path::Path) {
    let recorded =
        std::fs::read_to_string(out_path).unwrap_or_else(|e| panic!("probe did not write its output file: {e}"));
    let lines: Vec<&str> = recorded.lines().collect();
    assert_eq!(
        lines,
        vec!["variable-scoped panicking=true", "test-scoped panicking=true"],
        "the Variable-scoped fixture (dependent) must drop before the Test-scoped one \
         (tracked) it borrowed from, and both must see std::thread::panicking() == true \
         while the test's own panic is still unwinding through __scope; got: {recorded:?}"
    );
}

#[test]
fn should_panic_satisfied_reports_panicking_during_scope_drop() {
    let out_file = tempfile::NamedTempFile::new().expect("create temp file for the probe's output");
    let out = run_probe_trial("panics_with_tracked_fixture", Some(out_file.path()));

    assert!(
        out.status.success(),
        "expected the should_panic test to be satisfied (process success); stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_drop_order(out_file.path());
}

#[test]
fn should_panic_with_message_satisfied_reports_panicking_during_scope_drop() {
    let out_file = tempfile::NamedTempFile::new().expect("create temp file for the probe's output");
    let out = run_probe_trial("panics_with_tracked_fixture_msg", Some(out_file.path()));

    assert!(
        out.status.success(),
        "expected the should_panic = \"...\" test to be satisfied (process success); stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_drop_order(out_file.path());
}

#[test]
fn should_panic_not_satisfied_by_teardown_panic() {
    let out = run_probe_trial("body_completes_but_fixture_drop_panics", None);

    assert!(
        !out.status.success(),
        "a fixture Drop panicking during teardown must not satisfy should_panic when the \
         test body itself never panicked; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn should_panic_with_message_not_satisfied_by_teardown_panic() {
    let out = run_probe_trial("body_completes_but_fixture_drop_panics_msg", None);

    assert!(
        !out.status.success(),
        "a fixture Drop panicking during teardown must not satisfy should_panic = \"...\" \
         when the test body itself never panicked, even if the teardown panic's message \
         happens to match the expected substring; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
