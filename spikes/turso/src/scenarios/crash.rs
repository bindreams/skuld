//! A process dies holding the write lock: peers see busy while it lives, then
//! get the lock back, and only its committed writes survive.

use crate::db;
use crate::report::check;
use std::path::Path;

const S: &str = "crash";

pub fn run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("k.db");
    let d = db::open(&path).unwrap();
    d.batch("PRAGMA journal_mode = WAL; CREATE TABLE t (x TEXT NOT NULL);")
        .unwrap();
    let mut child = super::spawn("w-crash", &[path.to_str().unwrap()]);
    let _out = super::await_line(&mut child, "HELD");
    let r = d.batch("BEGIN EXCLUSIVE");
    check(
        S,
        matches!(&r, Err(e) if db::is_busy(e)),
        format!("BEGIN EXCLUSIVE while the holder lives: {r:?}"),
    );
    if r.is_ok() {
        let _ = d.batch("ROLLBACK");
    }
    super::release(&mut child);
    let status = child.wait().unwrap();
    check(S, !status.success(), format!("holder aborted: {status}"));
    let (r, busy) = db::retry_busy(|| d.batch("BEGIN EXCLUSIVE"));
    check(
        S,
        r.is_ok(),
        format!("BEGIN EXCLUSIVE after the holder died: {r:?} ({busy} busy retries)"),
    );
    let _ = d.batch("ROLLBACK");
    let r = d.int("SELECT count(*) FROM t WHERE x = 'committed'");
    check(S, matches!(r, Ok(1)), format!("committed row survives: {r:?}"));
    let r = d.int("SELECT count(*) FROM t WHERE x = 'uncommitted'");
    check(S, matches!(r, Ok(0)), format!("uncommitted row is gone: {r:?}"));
    // A fresh open, as the next test binary would do.
    drop(d);
    let r = db::open(&path).and_then(|d| d.text("PRAGMA integrity_check"));
    check(
        S,
        matches!(r.as_deref(), Ok("ok")),
        format!("fresh open + integrity_check: {r:?}"),
    );
}

pub fn worker(args: &[String]) {
    let d = db::open(Path::new(&args[0])).unwrap();
    d.exec("INSERT INTO t VALUES ('committed')", ()).unwrap();
    d.batch("BEGIN EXCLUSIVE").unwrap();
    d.exec("INSERT INTO t VALUES ('uncommitted')", ()).unwrap();
    super::signal_and_wait("HELD");
    std::process::abort();
}
