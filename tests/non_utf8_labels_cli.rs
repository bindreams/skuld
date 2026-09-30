//! A `SKULD_LABELS` that is not valid Unicode is an error, not "unset": an
//! unset filter runs every test, which would turn a corrupted label lane into
//! a full run.

use std::process::Command;

#[test]
fn non_utf8_skuld_labels_is_a_startup_error() {
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
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        cmd.env("SKULD_LABELS", std::ffi::OsStr::from_bytes(b"fa\xFFst"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        // An unpaired surrogate (0xD800) is invalid UTF-16, so not valid Unicode.
        cmd.env("SKULD_LABELS", std::ffi::OsString::from_wide(&[0x66, 0xD800]));
    }

    let out = cmd.output().expect("spawn label_filter_fixture");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a non-UTF-8 SKULD_LABELS must fail, not run everything; stdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr.contains("SKULD_LABELS") && stderr.contains("not valid UTF-8"),
        "the panic must name the variable; stderr:\n{stderr}"
    );
}
