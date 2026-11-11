//! End-to-end subprocess tests for the modes of what the coordination directory is made of, via
//! the `register_probe` binary: `.skuld/` and `running/` are 0777 and a registration's
//! `<id>.lock` and `<id>.json` are 0644, whatever the umask, so that whichever uid gets there
//! first does not lock out the rest.
//!
//! Both cases need genuine subprocesses, for different reasons:
//! `registration_creates_open_directories_and_files_despite_umask` needs one because `umask` is
//! process-global, and mutating it in this test binary's own process would corrupt every other
//! test running concurrently in the same `cargo test` binary;
//! `concurrent_registrations_converge_on_one_open_directory` needs several because the
//! coordination directory is in production created by genuinely separate OS processes (shared
//! across all test binaries in a workspace).
#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, Stdio};

fn probe(coord_dir: &Path, umask: Option<&str>) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_register_probe"));
    cmd.env("SKULD_REGISTER_PROBE_COORD_DIR", coord_dir);
    // An inherited SKULD_DEBUG=1 would make skuld's own debug output go to stderr. This driver
    // doesn't start reading stderr until `wait_with_output`, after the handshake; if enough bytes
    // accumulated to fill the pipe's kernel buffer, the child's write would block and it would
    // never reach the handshake, deadlocking against this driver.
    cmd.env_remove("SKULD_DEBUG");
    match umask {
        Some(mask) => {
            cmd.env("SKULD_REGISTER_PROBE_UMASK", mask);
        }
        None => {
            cmd.env_remove("SKULD_REGISTER_PROBE_UMASK");
        }
    }
    cmd
}

/// Read one handshake byte from `child`'s stdout and assert it equals `expected`. On any failure —
/// a read error (most commonly `UnexpectedEof` because the child died and closed its end) or an
/// unexpected byte — folds the child's stderr into the panic message instead of reporting a bare
/// `UnexpectedEof` with no indication of what actually went wrong.
///
/// Takes `&mut Child` (not by value) so callers can keep using the same `Vec<Child>` afterwards;
/// `wait_with_output` isn't available on a borrowed `Child`, so failure draining uses
/// `stderr.read_to_string` plus `wait` instead.
fn read_signal(child: &mut std::process::Child, expected: u8, what: &str) {
    let mut byte = [0u8; 1];
    let read_result = child.stdout.as_mut().expect("stdout was piped").read_exact(&mut byte);

    if read_result.is_ok() && byte[0] == expected {
        return;
    }

    // Something's wrong. Drop our end of stdin first: if the child is still alive and blocked
    // reading a release byte that this failure means it will never get, closing stdin gives it
    // EOF so its own read errors and it exits promptly instead of this test hanging on the reads
    // below.
    drop(child.stdin.take());

    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let status = child.wait().expect("wait register_probe after a failed handshake read");

    match read_result {
        Err(e) => panic!("failed to read register_probe's {what} signal: {e} (exit: {status})\nstderr:\n{stderr}"),
        Ok(()) => panic!(
            "register_probe must signal {what} with {:?}, got {:?} (exit: {status})\nstderr:\n{stderr}",
            expected as char, byte[0] as char
        ),
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("{path:?} must exist: {e}"))
        .mode()
        & 0o777
}

/// Assert the directory modes, and that `running/` holds `registered` registrations, each with a
/// 0644 `<id>.lock` and `<id>.json`.
fn assert_modes(coord_dir: &Path, registered: usize) {
    assert_eq!(mode(coord_dir), 0o777, "{coord_dir:?} must be 0777");
    let running = coord_dir.join("running");
    assert_eq!(mode(&running), 0o777, "{running:?} must be 0777");

    let (mut locks, mut jsons) = (0, 0);
    for entry in std::fs::read_dir(&running).unwrap() {
        let path = entry.unwrap().path();
        match path.extension().and_then(|e| e.to_str()) {
            Some("lock") => locks += 1,
            Some("json") => jsons += 1,
            _ => panic!("unexpected file {path:?} in {running:?} while every probe is registered"),
        }
        assert_eq!(mode(&path), 0o644, "{path:?} must be 0644");
    }
    assert_eq!(
        (locks, jsons),
        (registered, registered),
        "one .lock and one .json per registration"
    );
}

/// umask 077 (tighter than a typical dev/CI default of 022) gives a umask-driven regression the
/// largest possible gap to show up in.
///
/// Must inspect the files while `register_probe` is still registered — see its module doc —
/// rather than after it exits, which deletes them.
#[test]
fn registration_creates_open_directories_and_files_despite_umask() {
    let dir = skuld::TempDir::new().unwrap();
    let coord_dir = dir.path().join(".skuld");

    let mut cmd = probe(&coord_dir, Some("077"));
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn register_probe");

    read_signal(&mut child, b'C', "registered");
    assert_modes(&coord_dir, 1);

    child
        .stdin
        .as_mut()
        .expect("stdin was piped")
        .write_all(b"G")
        .expect("release register_probe to exit");

    let out = child.wait_with_output().expect("wait register_probe");
    assert!(
        out.status.success(),
        "register_probe failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn concurrent_registrations_converge_on_one_open_directory() {
    const REGISTRANTS: usize = 8;
    let dir = skuld::TempDir::new().unwrap();
    let coord_dir = dir.path().join(".skuld");

    // Every registrant starts against the same missing directory, so they race to create
    // `.skuld/` and `running/`. Every one must end up registered without panicking, in the one
    // directory the winner published, at 0777.
    //
    // Spawning 8 children in quick succession does not by itself guarantee they race: the window
    // between the first child creating the directory and later ones reaching their own
    // registration is narrow next to exec/dynamic-linking jitter. Each child instead blocks on
    // `SKULD_REGISTER_PROBE_BARRIER` (see `register_probe`'s doc) until it has signaled readiness
    // on stdout; only once every child has done so does the driver release them via stdin,
    // holding every registrant at the same starting line first.
    let mut children: Vec<std::process::Child> = (0..REGISTRANTS)
        .map(|_| {
            let mut cmd = probe(&coord_dir, None);
            cmd.env("SKULD_REGISTER_PROBE_BARRIER", "1");
            cmd.stdin(Stdio::piped());
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());
            cmd.spawn().expect("spawn register_probe")
        })
        .collect();

    for child in &mut children {
        read_signal(child, b'R', "ready");
    }
    for child in &mut children {
        child
            .stdin
            .as_mut()
            .expect("stdin was piped")
            .write_all(b"G")
            .expect("release register_probe from the barrier");
    }

    // Second round: every registrant registers, then blocks again while registered (see
    // `register_probe`'s module doc). Wait for every child to signal registered before
    // inspecting, so the files of every registration are there to see.
    for child in &mut children {
        read_signal(child, b'C', "registered");
    }
    assert_modes(&coord_dir, REGISTRANTS);

    for child in &mut children {
        child
            .stdin
            .as_mut()
            .expect("stdin was piped")
            .write_all(b"G")
            .expect("release register_probe to exit");
    }

    for (i, child) in children.into_iter().enumerate() {
        let out = child.wait_with_output().expect("wait register_probe");
        assert!(
            out.status.success(),
            "registrant {i} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
