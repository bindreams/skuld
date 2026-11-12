//! End-to-end subprocess test for the panic-during-unwind hazard in
//! `TestRegistration::drop`, via the `drop_panic_during_unwind_probe`
//! binary. `Drop::drop`'s own cleanup panics loudly when the coordination directory no longer
//! matches what the registration made (its directory was retargeted) — so a test that panics for an
//! unrelated reason, while its registration has independently been broken, causes a *second*
//! panic during the first one's unwind. An uncaught panic during an active unwind is Rust's
//! "double panic": `std::process::abort()` (`SIGABRT` on Unix), killing the
//! whole test process rather than just failing the one test. This needs a
//! genuine subprocess: the abort, if it happens, must not take this driver
//! test binary down with it.
//!
//! Runs on every platform: the `Drop::drop` fix it exercises is not
//! platform-gated, and Skuld's CI has a Windows lane. The driver sabotages the probe's
//! registration while the probe waits for it: it retargets the directory link above the
//! coordination directory (a symlink on Unix, a junction on Windows, which needs no privilege),
//! which Windows allows although it refuses to delete a running test's entry. That fails the
//! cleanup on both platforms. The test
//! asserts that the process exits via a single ordinary panic (not an abort),
//! and that the downgraded warning names the coordination directory and the
//! cause rather than the `panic_payload_message` fallback placeholder: during
//! an unwind the warning is the only signal left of the failure.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// Make `link` a directory link to `target`: a symlink on Unix, a junction on Windows.
fn link_dir(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    junction::create(target, link).unwrap();
}

#[test]
fn a_second_panic_during_drop_does_not_abort_the_process() {
    let dir = skuld::TempDir::new().unwrap();
    // Mirrors `linked_coord` in `src/coordination/registry_tests.rs`, which a test of the library
    // cannot share with this one.
    let (real1, real2, link) = (
        dir.path().join("real1"),
        dir.path().join("real2"),
        dir.path().join("link"),
    );
    std::fs::create_dir(&real1).unwrap();
    std::fs::create_dir(&real2).unwrap();
    link_dir(&real1, &link);
    let coord_dir = link.join(".skuld");

    let mut child = Command::new(env!("CARGO_BIN_EXE_drop_panic_during_unwind_probe"))
        .env("SKULD_DROP_PANIC_PROBE_COORD_DIR", &coord_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn drop_panic_during_unwind_probe");

    // The probe signals once registered and then waits to be released: a blocking-I/O handshake.
    let mut signal = [0u8; 1];
    let ready = child.stdout.as_mut().expect("stdout was piped").read_exact(&mut signal);
    if ready.is_err() || signal[0] != b'R' {
        drop(child.stdin.take());
        let out = child.wait_with_output().expect("wait the probe");
        panic!(
            "the probe must signal registered with 'R', got {ready:?} {signal:?} (exit: {})
stderr:
{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // Retarget the link: the probe's registration is now behind the old target.
    #[cfg(unix)]
    std::fs::remove_file(&link).unwrap();
    #[cfg(windows)]
    std::fs::remove_dir(&link).unwrap();
    link_dir(&real2, &link);
    child
        .stdin
        .as_mut()
        .expect("stdin was piped")
        .write_all(b"G")
        .expect("release the probe");

    let out = child.wait_with_output().expect("wait the probe");

    let stderr = String::from_utf8_lossy(&out.stderr);

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            out.status.signal(),
            None,
            "the process must exit via its single propagated panic, not be killed by a signal \
             (a non-None signal here means Drop's own panic escaped an active unwind and Rust \
             aborted the whole process); stderr:\n{stderr}"
        );
    }
    assert_eq!(
        out.status.code(),
        Some(101),
        "a single propagated panic exits with libstd's standard code 101 on every platform; an \
         abort (SIGABRT on Unix, a runtime-abort exit code on Windows) would not produce 101; \
         stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("probe: artificial panic to trigger unwind"),
        "the original panic's message must reach stderr: {stderr}"
    );
    let warning_line = stderr
        .lines()
        .find(|l| l.contains("coordination cleanup panicked while already unwinding"))
        .unwrap_or_else(|| panic!("the downgraded second-panic warning must reach stderr: {stderr}"));
    // The default panic hook prints the second panic's raw message too
    // (before our `catch_unwind` even sees it), so checking `stderr` as a
    // whole would pass even if the downgraded warning's own extracted message
    // were wrong: check the warning line itself.
    assert!(
        warning_line.contains(&format!("{coord_dir:?}")) && warning_line.contains("moved"),
        "the downgraded warning must name the coordination directory and the cause: {warning_line:?}"
    );
    assert!(
        !warning_line.contains("<non-string panic payload>"),
        "the downgraded warning must carry the second panic's actual message, not the \
         non-string-payload fallback placeholder (regression guard for passing `&payload` \
         instead of `payload.as_ref()` to `panic_payload_message`, which silently downcasts \
         the wrong type and always falls back): {warning_line:?}"
    );
}
