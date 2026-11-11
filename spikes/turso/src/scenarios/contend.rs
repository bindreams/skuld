//! N processes x M read-modify-write transactions on one counter.
//!
//! Mutual exclusion is checked three ways: no lost updates (counter == N*M),
//! the `holders` table never has another row inside a transaction, and an OS
//! file lock taken inside each transaction is never already held.

use crate::db;
use crate::report::{check, info};
use std::io::BufRead;
use std::path::Path;

pub fn run(begin: &str, n: usize, m: usize) {
    let s = format!("contend[{begin}, {n}x{m}]");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.db");
    let witness = dir.path().join("witness.lock");
    std::fs::File::create(&witness).unwrap();
    let d = db::open(&path).unwrap();
    d.batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE counter (n INTEGER NOT NULL);
         INSERT INTO counter VALUES (0);
         CREATE TABLE holders (pid INTEGER NOT NULL);",
    )
    .unwrap();
    // The driver's connection stays open throughout, like a long-lived peer.
    let (p, w, ms) = (path.to_str().unwrap(), witness.to_str().unwrap(), m.to_string());
    let mut children: Vec<_> = (0..n).map(|_| super::spawn("w-contend", &[p, w, &ms, begin])).collect();
    let (mut busy, mut violations, mut errors) = (0u64, 0u64, 0u64);
    for c in &mut children {
        let out = std::io::BufReader::new(c.stdout.take().unwrap());
        for line in out.lines() {
            let line = line.unwrap();
            if let Some(rest) = line.strip_prefix("RESULT ") {
                let v: Vec<u64> = rest.split(' ').map(|x| x.parse().unwrap()).collect();
                busy += v[0];
                violations += v[1];
                errors += v[2];
            } else {
                println!("  worker: {line}");
            }
        }
        let status = c.wait().unwrap();
        check(&s, status.success(), format!("worker exit: {status}"));
    }
    let total = d.int("SELECT n FROM counter");
    check(
        &s,
        matches!(total, Ok(t) if t == (n * m) as i64),
        format!("no lost updates: counter={total:?}, want {}", n * m),
    );
    check(
        &s,
        violations == 0,
        format!("no overlapping holders: {violations} overlap(s)"),
    );
    check(&s, errors == 0, format!("no non-busy errors: {errors}"));
    check(&s, busy > 0, format!("contention was real: {busy} busy retries"));
    let r = d.text("PRAGMA integrity_check");
    check(&s, matches!(r.as_deref(), Ok("ok")), format!("integrity_check: {r:?}"));
    info(&s, format!("files: {:?}", list(dir.path())));
}

fn list(dir: &Path) -> Vec<String> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

pub fn worker(args: &[String]) {
    let (path, witness) = (Path::new(&args[0]), Path::new(&args[1]));
    let (m, begin) = (args[2].parse::<usize>().unwrap(), args[3].as_str());
    let d = db::open(path).unwrap();
    let witness = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(witness)
        .unwrap();
    let pid = std::process::id() as i64;
    let (mut busy, mut violations, mut errors) = (0u64, 0u64, 0u64);
    for _ in 0..m {
        let txn = || -> db::Result<u64> {
            d.batch(begin)?;
            let mut v = 0;
            // Taken only after BEGIN succeeds; released before COMMIT.
            if witness.try_lock().is_err() {
                v += 1;
            }
            if d.int("SELECT count(*) FROM holders")? != 0 {
                v += 1;
            }
            d.exec("INSERT INTO holders VALUES (?1)", (pid,))?;
            let n = d.int("SELECT n FROM counter")?;
            d.exec("UPDATE counter SET n = ?1", (n + 1,))?;
            d.exec("DELETE FROM holders WHERE pid = ?1", (pid,))?;
            let _ = witness.unlock();
            d.batch("COMMIT")?;
            Ok(v)
        };
        let (r, b) = db::retry_busy(|| {
            let r = txn();
            if r.is_err() {
                let _ = witness.unlock();
                let _ = d.batch("ROLLBACK");
            }
            r
        });
        busy += b;
        match r {
            Ok(v) => violations += v,
            Err(e) => {
                errors += 1;
                println!("error: {e:?}");
            }
        }
    }
    println!("RESULT {busy} {violations} {errors}");
}
