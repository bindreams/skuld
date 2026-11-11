//! Single process: the statements skuld's schema and migration code use.

use crate::db;
use crate::report::{check, info};

const S: &str = "smoke";

pub fn run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("smoke.db");
    let d = match db::open(&path) {
        Ok(d) => d,
        Err(e) => return check(S, false, format!("open in multiprocess mode: {e}")),
    };
    check(S, true, "open in multiprocess mode (pollster block_on completes)");
    match d.text("PRAGMA journal_mode = WAL") {
        Ok(m) => check(
            S,
            m.eq_ignore_ascii_case("wal"),
            format!("PRAGMA journal_mode = WAL -> {m}"),
        ),
        Err(e) => check(S, false, format!("PRAGMA journal_mode = WAL: {e}")),
    }
    let r = d.batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS running (id INTEGER PRIMARY KEY, instance_id TEXT NOT NULL, name TEXT NOT NULL, serial_filter TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS labels (running_id INTEGER NOT NULL REFERENCES running(id) ON DELETE CASCADE, label TEXT NOT NULL, PRIMARY KEY (running_id, label));",
    );
    check(
        S,
        r.is_ok(),
        format!("skuld-like schema with FK + ON DELETE CASCADE: {r:?}"),
    );
    let r = d
        .batch("PRAGMA user_version = 3")
        .and_then(|()| d.int("PRAGMA user_version"));
    check(S, matches!(r, Ok(3)), format!("PRAGMA user_version round-trip: {r:?}"));
    for begin in ["BEGIN EXCLUSIVE", "BEGIN IMMEDIATE"] {
        let r = d.batch(begin).and_then(|()| {
            let auto = d.conn.is_autocommit()?;
            d.batch("ROLLBACK")?;
            Ok(auto)
        });
        check(
            S,
            matches!(r, Ok(false)),
            format!("{begin} opens a transaction: autocommit={r:?}"),
        );
    }
    let r = d
        .exec(
            "INSERT INTO running (instance_id, name, serial_filter) VALUES ('p', 'n', '*')",
            (),
        )
        .and_then(|_| d.exec("INSERT INTO labels VALUES (last_insert_rowid(), 'L')", ()))
        .and_then(|_| d.exec("DELETE FROM running", ()))
        .and_then(|_| d.int("SELECT count(*) FROM labels"));
    check(
        S,
        matches!(r, Ok(0)),
        format!("ON DELETE CASCADE removes labels: {r:?}"),
    );
    let missing = dir.path().join("no-such-dir").join("x.db");
    match db::open(&missing) {
        Ok(_) => info(S, "open under a missing directory succeeded"),
        Err(e) => info(S, format!("open under a missing directory: {e:?}")),
    }
}
