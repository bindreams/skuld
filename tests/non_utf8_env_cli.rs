//! A `SKULD_*` variable that is not valid UTF-8 must not be read as unset:
//! for `SKULD_LABELS` that would run every test. `SKULD_DEBUG` and
//! `SKULD_LABELS` panic naming the variable; `SKULD_NEXTEST_METADATA_PATH` is a
//! path, so a non-UTF-8 value is legal and honored.

use std::ffi::OsString;
use std::process::Command;

fn fixture_list(vars: &[(&str, OsString)]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_label_filter_fixture"));
    cmd.arg("--list");
    for key in [
        "SKULD_LABELS",
        "SKULD_DEBUG",
        "SKULD_NEXTEST_METADATA_PATH",
        "RUST_TEST_THREADS",
        "RUST_TEST_NOCAPTURE",
        "RUST_LOG",
        "RUST_BACKTRACE",
        "NEXTEST_EXECUTION_MODE",
        "NEXTEST_RUN_ID",
        "NEXTEST_BIN_EXE_NAME",
    ] {
        cmd.env_remove(key);
    }
    for (k, v) in vars {
        cmd.env(k, v);
    }
    cmd
}

fn non_utf8() -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(b"fa\xFFst".to_vec())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        // An unpaired surrogate is invalid UTF-16.
        OsString::from_wide(&[0x66, 0xD800])
    }
}

#[track_caller]
fn assert_startup_error_naming(var: &str) {
    let out = fixture_list(&[(var, non_utf8())])
        .output()
        .expect("spawn label_filter_fixture");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a non-UTF-8 {var} must fail; stdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr.contains(var) && stderr.contains("not valid UTF-8"),
        "the panic must name {var}; stderr:\n{stderr}"
    );
}

#[test]
fn non_utf8_skuld_labels_is_a_startup_error() {
    assert_startup_error_naming("SKULD_LABELS");
}

#[test]
fn non_utf8_skuld_debug_is_a_startup_error() {
    assert_startup_error_naming("SKULD_DEBUG");
}

// Linux only: macOS filesystems reject non-UTF-8 names.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_nextest_metadata_path_is_honored() {
    use std::os::unix::ffi::OsStringExt;

    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join(OsString::from_vec(b"d\xFFir".to_vec()));
    std::fs::create_dir(&dir).expect("create non-UTF-8 directory");
    let path = dir.join("meta.json");

    let out = fixture_list(&[("SKULD_NEXTEST_METADATA_PATH", path.clone().into_os_string())])
        .output()
        .expect("spawn label_filter_fixture");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dump = std::fs::read_to_string(&path).expect("the dump must be written to the non-UTF-8 path");
    assert!(dump.contains("\"tests\""), "unexpected dump: {dump}");
}
