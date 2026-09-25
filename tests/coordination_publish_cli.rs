//! End-to-end subprocess tests for the coordination DB's atomic-publish
//! behavior, via the `publish_probe` binary. Both cases need genuine
//! subprocesses rather than in-process simulation, but for different
//! reasons: `publish_creates_three_0666_files_despite_umask` needs one
//! because `umask` is process-global, and mutating it in this test binary's
//! own process would corrupt every other test running concurrently in the
//! same `cargo test` binary; `concurrent_publishers_all_converge_on_one_0666_file`
//! needs several because the coordination DB is in production published by
//! genuinely separate OS processes (`db_path`'s doc in `src/coordination.rs`:
//! the DB is "shared across all test binaries in a workspace"). SQLite's own
//! locking is per-process, and connections opened by threads within one
//! process share that process's `-shm` mapping and lock state, so racing
//! them in-process wouldn't exercise the same cross-process lock contention
//! in `open_db`, nor the same fresh `-wal`/`-shm` creation, that separate
//! processes racing to publish `.skuld.db` for the first time do. See
//! `src/coordination/publish.rs`'s module doc for the one requirement this
//! machinery exists for. The remaining publish tests are in-process and
//! live in `src/coordination/publish_tests.rs`.
#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, Stdio};

fn probe(db_path: &Path, umask: Option<&str>) -> std::process::Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_publish_probe"));
    cmd.env("SKULD_PUBLISH_PROBE_DB", db_path);
    // An inherited SKULD_DEBUG=1 would make skuld's own `debug!` machinery
    // write to stderr — `create_publish_temp_with` already does this on a
    // candidate-name collision, a real reachable case per its own doc, not
    // a theoretical one — so strip it unconditionally rather than rely on
    // that output staying small. This driver doesn't start reading stderr
    // until `wait_with_output`, after the handshake; any unread bytes sit
    // in the pipe until then, and if enough of them accumulate to fill the
    // pipe's kernel buffer, the child's write blocks and it never reaches
    // the handshake, deadlocking against this driver.
    cmd.env_remove("SKULD_DEBUG");
    match umask {
        Some(mask) => {
            cmd.env("SKULD_PUBLISH_PROBE_UMASK", mask);
        }
        None => {
            cmd.env_remove("SKULD_PUBLISH_PROBE_UMASK");
        }
    }
    cmd
}

fn companion(db_path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut os = db_path.as_os_str().to_os_string();
    os.push(suffix);
    std::path::PathBuf::from(os)
}

/// Read one handshake byte from `child`'s stdout and assert it equals
/// `expected`. On any failure — a read error (most commonly `UnexpectedEof`
/// because the child died and closed its end) or an unexpected byte — folds
/// the child's stderr into the panic message instead of reporting a bare
/// `UnexpectedEof` with no indication of what actually went wrong.
///
/// Takes `&mut Child` (not by value) so callers can keep using the same
/// `Vec<Child>` afterwards; `wait_with_output` isn't available on a
/// borrowed `Child`, so failure draining uses `stderr.read_to_string` plus
/// `wait` instead.
fn read_signal(child: &mut std::process::Child, expected: u8, what: &str) {
    let mut byte = [0u8; 1];
    let read_result = child.stdout.as_mut().expect("stdout was piped").read_exact(&mut byte);

    if read_result.is_ok() && byte[0] == expected {
        return;
    }

    // Something's wrong. Drop our end of stdin first: if the child is still
    // alive and blocked reading a release byte that this failure means it
    // will never get, closing stdin gives it EOF so its own read errors and
    // it exits promptly instead of this test hanging on the reads below.
    drop(child.stdin.take());

    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let status = child.wait().expect("wait publish_probe after a failed handshake read");

    match read_result {
        Err(e) => panic!("failed to read publish_probe's {what} signal: {e} (exit: {status})\nstderr:\n{stderr}"),
        Ok(()) => panic!(
            "publish_probe must signal {what} with {:?}, got {:?} (exit: {status})\nstderr:\n{stderr}",
            expected as char, byte[0] as char
        ),
    }
}

/// This is the measurement the atomic-publish design's simplified scope
/// depends on: only `.skuld.db` itself goes through Skuld's atomic-publish
/// dance (`src/coordination/publish.rs`) — the `-wal`/`-shm` companions are
/// never pre-created or `fchmod`ed by Skuld at all. That's only correct if
/// SQLite's own Unix VFS independently produces `0666` companions once the
/// main DB file is already `0666`, even under a restrictive umask. The
/// bundled SQLite's `findCreateFileMode` derives a new file's mode from an
/// existing db file's `st_mode` when one is available, `unixOpenSharedMemory`
/// uses that same derivation for `-shm`, and `robust_open`'s `fchmod` counters
/// the umask — umask 077 (tighter than a typical dev/CI default of 022) is
/// used here specifically to give a umask-driven regression the largest
/// possible gap to show up in.
///
/// Must stat the companions while `publish_probe` still holds its connection
/// open — see its module doc — rather than after it exits: without
/// `SQLITE_FCNTL_PERSIST_WAL` set, SQLite deletes `-wal`/`-shm` when the
/// connection holding them closes, and this probe's connection is the only
/// one there is.
///
/// `probe_coordination_connect` also goes through `open_db`, which takes
/// `db_path`'s init lock before doing anything else — but that lock is
/// `db_path`'s own parent directory on Unix (see
/// `src/coordination/lock.rs`'s module doc), not a file this test's umask
/// scenario could affect, so there's no fourth companion to check here.
#[test]
fn publish_creates_three_0666_files_despite_umask() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join(".skuld.db");

    let mut cmd = probe(&db_path, Some("077"));
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn publish_probe");

    read_signal(&mut child, b'C', "connected");

    for suffix in ["", "-wal", "-shm"] {
        let path = companion(&db_path, suffix);
        let meta = std::fs::metadata(&path).unwrap_or_else(|e| panic!("{path:?} must exist: {e}"));
        assert!(meta.file_type().is_file(), "{path:?} must be a regular file");
        assert_eq!(
            meta.mode() & 0o777,
            0o666,
            "{path:?} must be published at 0666 despite umask 077, got {:04o}",
            meta.mode() & 0o777
        );
    }

    child
        .stdin
        .as_mut()
        .expect("stdin was piped")
        .write_all(b"G")
        .expect("release publish_probe to exit");

    let out = child.wait_with_output().expect("wait publish_probe");
    assert!(
        out.status.success(),
        "publish_probe failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn concurrent_publishers_all_converge_on_one_0666_file() {
    const PUBLISHERS: usize = 8;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join(".skuld.db");

    // Every publisher starts against the same empty directory: whichever one
    // wins the race to publish `.skuld.db` itself, every one of them must
    // still end up connected without panicking, and the file each one sees
    // (the winner's own, or the winner's once its own attempt lost) must be
    // `0666`.
    //
    // Spawning 8 children in quick succession does not by itself guarantee
    // they contend: the real race window (open+fchmod+rename, all inside
    // `probe_coordination_connect`) is narrow next to exec/dynamic-linking
    // jitter, so without synchronization genuine contention is a matter of
    // scheduling luck, not something this test reliably exercises. Each
    // child instead blocks on `SKULD_PUBLISH_PROBE_BARRIER` (see
    // `publish_probe`'s doc) until it has signaled readiness on stdout; only
    // once every child has done so does the driver release them via stdin,
    // holding every publisher at the same starting line first. Even so,
    // this test's own assertions never depend on the EEXIST path actually
    // being taken on a given run — the deterministic case for that is
    // `a_lost_publish_race_uses_the_winners_file` in
    // `src/coordination/publish_tests.rs`.
    let mut children: Vec<std::process::Child> = (0..PUBLISHERS)
        .map(|_| {
            let mut cmd = probe(&db_path, None);
            cmd.env("SKULD_PUBLISH_PROBE_BARRIER", "1");
            cmd.stdin(Stdio::piped());
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());
            cmd.spawn().expect("spawn publish_probe")
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
            .expect("release publish_probe from the barrier");
    }

    // Second round: every publisher connects, then blocks again holding its
    // connection open (see `publish_probe`'s module doc). Wait for every
    // child to signal connected before stat-ing the companions, so the stat
    // is guaranteed to observe at least one live connection to the
    // database — without `SQLITE_FCNTL_PERSIST_WAL`, SQLite deletes
    // `-wal`/`-shm` once the connection holding them closes.
    for child in &mut children {
        read_signal(child, b'C', "connected");
    }

    for suffix in ["", "-wal", "-shm"] {
        let path = companion(&db_path, suffix);
        let meta = std::fs::metadata(&path).unwrap_or_else(|e| panic!("{path:?} must exist: {e}"));
        assert!(meta.file_type().is_file(), "{path:?} must be a regular file");
        assert_eq!(
            meta.mode() & 0o777,
            0o666,
            "{path:?} must be published at 0666, got {:04o}",
            meta.mode() & 0o777
        );
    }

    for child in &mut children {
        child
            .stdin
            .as_mut()
            .expect("stdin was piped")
            .write_all(b"G")
            .expect("release publish_probe to exit");
    }

    for (i, child) in children.into_iter().enumerate() {
        let out = child.wait_with_output().expect("wait publish_probe");
        assert!(
            out.status.success(),
            "publisher {i} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
