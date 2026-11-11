//! N processes released at once, each opening the database, running
//! skuld-like init and inserting one row. Every reported success must leave
//! a row.
//!
//! Variants:
//! - `fresh`: the file does not exist yet; nothing serializes creation.
//! - `precreated`: the driver creates and initializes the file first.
//! - `init-locked`: like `fresh`, but open + init run under an OS file lock,
//!   as skuld's init lock does; the insert runs outside it.

use crate::db;
use crate::report::{check, info};
use std::io::BufRead;
use std::path::Path;

const INIT: &str = "PRAGMA journal_mode = WAL; CREATE TABLE IF NOT EXISTS t (pid INTEGER NOT NULL);";

pub fn run(n: usize, variant: &str, rounds: usize) {
    let s = format!("cold[{variant}, {n} procs x {rounds} rounds]");
    let mut lost_rounds = 0;
    let mut failed_workers = 0;
    for _ in 0..rounds {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cold.db");
        let lock = dir.path().join("init.lock");
        std::fs::File::create(&lock).unwrap();
        if variant == "precreated" {
            db::open(&path).unwrap().batch(INIT).unwrap();
        }
        let args = [path.to_str().unwrap(), lock.to_str().unwrap(), variant];
        let mut children: Vec<_> = (0..n).map(|_| super::spawn("w-cold", &args)).collect();
        let outs: Vec<_> = children.iter_mut().map(|c| super::await_line(c, "READY").0).collect();
        for c in &mut children {
            super::release(c);
        }
        let mut ok = 0;
        let failed_before = failed_workers;
        let mut log = Vec::new();
        for (c, out) in children.iter_mut().zip(outs) {
            log.extend(out.lines().map(|l| l.unwrap()));
            if c.wait().unwrap().success() {
                ok += 1;
            } else {
                failed_workers += 1;
            }
        }
        let rows = db::open(&path).and_then(|d| d.int("SELECT count(*) FROM t"));
        let lost = !matches!(rows, Ok(r) if r == ok);
        lost_rounds += usize::from(lost);
        if lost || failed_workers != failed_before {
            info(
                &s,
                format!("{ok} workers reported success, rows={rows:?}; log: {log:?}"),
            );
        }
    }
    check(&s, failed_workers == 0, format!("{failed_workers} worker(s) failed"));
    check(
        &s,
        lost_rounds == 0,
        format!("{lost_rounds}/{rounds} rounds lost a reported write"),
    );
}

pub fn worker(args: &[String]) {
    let (path, lock, variant) = (Path::new(&args[0]), Path::new(&args[1]), args[2].as_str());
    let lock = std::fs::OpenOptions::new().read(true).write(true).open(lock).unwrap();
    super::signal_and_wait("READY");
    let mut stage = "open";
    let (r, busy) = db::retry_busy(|| {
        let locked = variant == "init-locked";
        if locked {
            lock.lock().unwrap();
        }
        let opened: db::Result<db::Db> = (|| {
            stage = "open";
            let d = db::open(path)?;
            stage = "init";
            d.batch(INIT)?;
            Ok(d)
        })();
        if locked {
            lock.unlock().unwrap();
        }
        let d = opened?;
        stage = "insert";
        let (r, busy) = db::retry_busy(|| d.exec("INSERT INTO t VALUES (?1)", (std::process::id() as i64,)));
        if busy > 0 {
            println!("{} insert busy retries", std::process::id());
        }
        r.map(|_| ())
    });
    if busy > 0 {
        println!("{} busy retries; last at {stage}", std::process::id());
    }
    if let Err(e) = r {
        println!("{} error at {stage}: {e:?}", std::process::id());
        std::process::exit(1);
    }
}
