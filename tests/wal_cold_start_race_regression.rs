//! Regression guard for the `SQLITE_READONLY` WAL cold-start race documented
//! on `open_db` in `src/coordination.rs`, via the `wal_race_probe` binary.
//!
//! Uses genuine subprocesses, not in-process threads: SQLite's own unix VFS
//! serializes `-shm` creation across every thread *of one process* through a
//! process-local mutex, so a connection landing stuck `SQLITE_READONLY` from
//! a lost negotiation can only show up between separate OS processes — the
//! same reasoning `tests/coordination_publish_cli.rs`'s module doc gives for
//! its own publish-race tests. Confirmed *not* sufficient on its own,
//! though: an in-process 64-thread x 100-round version of this same
//! write-based probe never reproduced a failure against pre-lock code, as
//! expected, but neither did this subprocess version at up to 64 processes
//! x 30 rounds on this machine (macOS/APFS on local SSD) — the race window
//! this targets is real (SQLite's own WAL cold-start negotiation genuinely
//! can leave a losing connection stuck readonly for its own lifetime, per
//! `open_db`'s doc), but forcing it deterministically, or even reliably,
//! within a local stress test was not achieved here. This is kept as a
//! best-effort hardening probe — real production code path, real
//! concurrent creation, real write assertion — rather than a proven
//! failing-on-old-code repro; a CI environment with different filesystem/
//! scheduling characteristics (or a from-source, non-bundled SQLite build)
//! may exercise it more readily than local development did.
//!
//! Each round starts from a fresh, empty temp directory — never the real,
//! shared `.skuld.db` this crate's own test run uses (see `db_path`'s doc in
//! `src/coordination.rs`) — so this never touches state any concurrently
//! running test relies on.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

const PROBES: usize = 16;
const ROUNDS: usize = 20;

fn probe(db_path: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wal_race_probe"));
    cmd.env("SKULD_WAL_RACE_PROBE_DB", db_path);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd
}

#[test]
fn open_db_connections_survive_many_processes_racing_wal_cold_start() {
    for round in 0..ROUNDS {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join(".skuld.db");

        let mut children: Vec<std::process::Child> = (0..PROBES)
            .map(|_| probe(&db_path).spawn().expect("spawn wal_race_probe"))
            .collect();

        // Barrier: wait for every probe to signal ready before releasing
        // any of them, so all PROBES start open_db's cold-start negotiation
        // together rather than depending on process-launch scheduling to
        // overlap.
        for (i, child) in children.iter_mut().enumerate() {
            let mut byte = [0u8; 1];
            child
                .stdout
                .as_mut()
                .expect("stdout was piped")
                .read_exact(&mut byte)
                .unwrap_or_else(|e| panic!("round {round} probe {i}: failed to read ready signal: {e}"));
            assert_eq!(
                byte[0], b'R',
                "round {round} probe {i}: unexpected ready byte {:?}",
                byte[0] as char
            );
        }
        for (i, child) in children.iter_mut().enumerate() {
            child
                .stdin
                .as_mut()
                .expect("stdin was piped")
                .write_all(b"G")
                .unwrap_or_else(|e| panic!("round {round} probe {i}: failed to release: {e}"));
        }

        for (i, child) in children.into_iter().enumerate() {
            let out = child.wait_with_output().expect("wait wal_race_probe");
            assert!(
                out.status.success(),
                "round {round} probe {i}: wal_race_probe failed (likely a connection stuck \
                 SQLITE_READONLY from a lost WAL cold-start negotiation): {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
