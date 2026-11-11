//! skuld's per-test lifecycle: open + init under an OS lock, register (insert),
//! unregister (delete), close outside the lock. N processes x R cycles, from
//! a file that does not exist yet. Every cycle also leaves a permanent row,
//! so lost commits show up in the final count.

use crate::db;
use crate::report::{check, info};
use std::io::BufRead;
use std::path::Path;

const INIT: &str = "PRAGMA journal_mode = WAL;
     CREATE TABLE IF NOT EXISTS running (pid INTEGER NOT NULL);
     CREATE TABLE IF NOT EXISTS done (pid INTEGER NOT NULL);";

pub fn run(n: usize, cycles: usize) {
    let s = format!("churn[{n} procs x {cycles} cycles]");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("churn.db");
    let lock = dir.path().join("init.lock");
    std::fs::File::create(&lock).unwrap();
    let (p, l, c) = (path.to_str().unwrap(), lock.to_str().unwrap(), cycles.to_string());
    let mut children: Vec<_> = (0..n).map(|_| super::spawn("w-churn", &[p, l, &c])).collect();
    let outs: Vec<_> = children.iter_mut().map(|c| super::await_line(c, "READY").0).collect();
    for c in &mut children {
        super::release(c);
    }
    let mut failed = 0;
    for (c, out) in children.iter_mut().zip(outs) {
        for line in out.lines() {
            info(&s, format!("worker: {}", line.unwrap()));
        }
        failed += usize::from(!c.wait().unwrap().success());
    }
    check(&s, failed == 0, format!("{failed} worker(s) failed"));
    let d = db::open(&path).unwrap();
    let r = d.int("SELECT count(*) FROM done");
    check(
        &s,
        matches!(r, Ok(x) if x == (n * cycles) as i64),
        format!("done rows: {r:?}, want {}", n * cycles),
    );
    let r = d.int("SELECT count(*) FROM running");
    check(&s, matches!(r, Ok(0)), format!("running rows left: {r:?}"));
    let r = d.text("PRAGMA integrity_check");
    check(&s, matches!(r.as_deref(), Ok("ok")), format!("integrity_check: {r:?}"));
}

pub fn worker(args: &[String]) {
    let (path, lock) = (Path::new(&args[0]), Path::new(&args[1]));
    let cycles: usize = args[2].parse().unwrap();
    let lock = std::fs::OpenOptions::new().read(true).write(true).open(lock).unwrap();
    let pid = std::process::id() as i64;
    super::signal_and_wait("READY");
    for i in 0..cycles {
        // skuld retries a busy init under the lock, never releasing it between tries.
        lock.lock().unwrap();
        let (opened, _) = db::retry_busy(|| {
            let d = db::open(path)?;
            d.batch(INIT)?;
            Ok(d)
        });
        lock.unlock().unwrap();
        let d = match opened {
            Ok(d) => d,
            Err(e) => {
                println!("{pid} cycle {i}: open/init: {e:?}");
                std::process::exit(1);
            }
        };
        let steps = [
            "INSERT INTO running VALUES (?1)",
            "INSERT INTO done VALUES (?1)",
            "DELETE FROM running WHERE pid = ?1",
        ];
        for sql in steps {
            let (r, _) = db::retry_busy(|| d.exec(sql, (pid,)));
            if let Err(e) = r {
                println!("{pid} cycle {i}: {sql}: {e:?}");
                std::process::exit(1);
            }
        }
        drop(d);
    }
}
