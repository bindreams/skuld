//! A rusqlite (C SQLite) process and a Turso process on one database file:
//! two skuld versions sharing `.skuld.db`. Each case runs in separate
//! processes, since POSIX record locks never conflict within one process.

use crate::db;
use crate::report::{check, info};
use std::path::Path;
use std::time::Duration;

const S: &str = "interop";

fn c_open(path: &Path) -> rusqlite::Connection {
    let c = rusqlite::Connection::open(path).unwrap();
    c.busy_timeout(Duration::ZERO).unwrap();
    c
}

fn c_count(path: &Path) -> String {
    let c = c_open(path);
    let n: rusqlite::Result<i64> = c.query_row("SELECT count(*) FROM t", [], |r| r.get(0));
    let ic: rusqlite::Result<String> = c.query_row("PRAGMA integrity_check", [], |r| r.get(0));
    format!("C sees rows={n:?}, integrity_check={ic:?}")
}

pub fn run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("i.db");
    let p = path.to_str().unwrap();

    // C creates a WAL database; Turso reads it.
    {
        let c = c_open(&path);
        c.execute_batch("PRAGMA journal_mode = WAL; CREATE TABLE t (x TEXT NOT NULL); INSERT INTO t VALUES ('c');")
            .unwrap();
    }
    let r = db::open(&path).and_then(|d| d.int("SELECT count(*) FROM t"));
    check(S, matches!(r, Ok(1)), format!("Turso reads a C-created WAL db: {r:?}"));

    // C holds the write lock; Turso tries to write.
    let mut child = super::spawn("w-interop-c-hold", &[p]);
    let _out = super::await_line(&mut child, "HELD");
    let r = db::open(&path).and_then(|d| {
        let begin = d.batch("BEGIN EXCLUSIVE");
        if begin.is_ok() {
            let w = d
                .exec("INSERT INTO t VALUES ('t-during-c')", ())
                .and_then(|_| d.batch("COMMIT"));
            return Ok(format!(
                "BEGIN EXCLUSIVE succeeded while C held the lock; write+commit: {w:?}"
            ));
        }
        Ok(format!("BEGIN EXCLUSIVE: {begin:?}"))
    });
    let excluded = match &r {
        Ok(s) => !s.contains("succeeded"),
        Err(_) => true,
    };
    check(
        S,
        excluded,
        format!("Turso is excluded while C holds the write lock: {r:?}"),
    );
    super::release(&mut child);
    info(S, format!("C holder exit: {}", child.wait().unwrap()));
    info(S, c_count(&path));

    // Turso holds the write lock; C tries to write.
    let mut child = super::spawn("w-interop-t-hold", &[p, "txn"]);
    let _out = super::await_line(&mut child, "HELD");
    let r = {
        let c = c_open(&path);
        match c.execute_batch("BEGIN EXCLUSIVE") {
            Ok(()) => {
                let w = c.execute_batch("INSERT INTO t VALUES ('c-during-t'); COMMIT;");
                format!("BEGIN EXCLUSIVE succeeded while Turso held the lock; write+commit: {w:?}")
            }
            Err(e) => format!("BEGIN EXCLUSIVE: {e}"),
        }
    };
    check(
        S,
        !r.contains("succeeded"),
        format!("C is excluded while Turso holds the write lock: {r}"),
    );
    super::release(&mut child);
    info(S, format!("Turso holder exit: {}", child.wait().unwrap()));
    info(S, c_count(&path));

    // Turso holds the db open, idle; C commits; Turso must see it.
    let mut child = super::spawn("w-interop-t-hold", &[p, "idle"]);
    let (out, mut lines) = super::await_line(&mut child, "HELD");
    let w = c_open(&path).execute("INSERT INTO t VALUES ('c-while-t-idle')", []);
    info(S, format!("C write while Turso idles: {w:?}"));
    super::release(&mut child);
    use std::io::BufRead;
    lines.extend(out.lines().map(|l| l.unwrap()));
    info(
        S,
        format!(
            "Turso holder after C's write: {lines:?}, exit {}",
            child.wait().unwrap()
        ),
    );
    let before = lines.iter().find_map(|l| l.strip_prefix("BEFORE ")).map(str::to_owned);
    let after = lines.iter().find_map(|l| l.strip_prefix("AFTER ")).map(str::to_owned);
    let saw = matches!((before.as_deref().map(str::parse::<i64>), after.as_deref().map(str::parse::<i64>)),
        (Some(Ok(b)), Some(Ok(a))) if a == b + 1);
    check(
        S,
        saw,
        format!("idle Turso sees C's commit: before={before:?} after={after:?}"),
    );
    info(S, c_count(&path));
}

pub fn worker(which: &str, args: &[String]) {
    let path = Path::new(&args[0]);
    match which {
        "w-interop-c-hold" => {
            let c = c_open(path);
            c.execute_batch("BEGIN EXCLUSIVE; INSERT INTO t VALUES ('c-held');")
                .unwrap();
            super::signal_and_wait("HELD");
            c.execute_batch("COMMIT").unwrap();
        }
        "w-interop-t-hold" => {
            let d = db::open(path).unwrap();
            if args[1] == "txn" {
                d.batch("BEGIN EXCLUSIVE").unwrap();
                d.exec("INSERT INTO t VALUES ('t-held')", ()).unwrap();
                super::signal_and_wait("HELD");
                d.batch("COMMIT").unwrap();
            } else {
                println!("BEFORE {}", d.int("SELECT count(*) FROM t").unwrap());
                super::signal_and_wait("HELD");
                match d.int("SELECT count(*) FROM t") {
                    Ok(n) => println!("AFTER {n}"),
                    Err(e) => println!("AFTER-ERROR {e:?}"),
                }
            }
        }
        other => panic!("unknown worker {other}"),
    }
}
