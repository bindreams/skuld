//! SQLite-based cross-process test coordination for serial execution.
//!
//! Every test (serial or not) registers itself in a shared SQLite database
//! before running. Serial tests use this registry to block until their
//! serialization constraints are satisfied.

#[cfg(test)]
mod coordination_tests;

use crate::label::{Label, LabelFilter};

use std::path::PathBuf;
use std::time::{Duration, Instant};

// Sentinel values for the `serial` field on TestDef / FixtureDef =====

/// Test is not serial — runs concurrently with everything.
pub const SERIAL_NONE: &str = "";

/// Test is serial with everything — no other test may run concurrently.
pub const SERIAL_ALL: &str = "*";

// Database path =====

/// Path to the shared coordination database, resolved at compile time from the
/// build profile directory (shared across all test binaries in a workspace).
pub(crate) fn db_path() -> PathBuf {
    std::path::Path::new(env!("SKULD_TARGET_PROFILE_DIR")).join(".skuld.db")
}

// Instance identity =====

/// A process-unique identifier combining PID and a monotonic timestamp.
/// Handles PID reuse on Windows by including a per-process unique value.
fn instance_id() -> String {
    use std::sync::OnceLock;
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let pid = std::process::id();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{pid}:{ts}")
    })
    .clone()
}

// Database initialization =====

/// Open a connection to the coordination database, creating it and the schema
/// if necessary. Each call returns a fresh connection suitable for single-thread
/// use.
pub(crate) fn open_db(path: &std::path::Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path)
        .unwrap_or_else(|e| panic!("skuld: failed to open coordination DB at {path:?}: {e}"));
    conn.busy_timeout(Duration::from_secs(5))
        .unwrap_or_else(|e| panic!("skuld: failed to set busy_timeout: {e}"));
    // Schema initialization requires a write lock. Under heavy concurrent access
    // (multiple test threads opening the DB simultaneously), the PRAGMA journal_mode
    // change may not honor busy_timeout on all platforms. Retry on "database is locked".
    let init_sql = "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS running (
             id            INTEGER PRIMARY KEY AUTOINCREMENT,
             instance_id   TEXT    NOT NULL,
             name          TEXT    NOT NULL,
             serial_filter TEXT    NOT NULL DEFAULT ''
         );
         CREATE TABLE IF NOT EXISTS labels (
             running_id INTEGER NOT NULL REFERENCES running(id) ON DELETE CASCADE,
             label      TEXT    NOT NULL
         );";
    for attempt in 0..50 {
        match conn.execute_batch(init_sql) {
            Ok(()) => return conn,
            Err(e) if e.to_string().contains("database is locked") && attempt < 49 => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => panic!("skuld: failed to initialize coordination DB at {path:?}: {e}"),
        }
    }
    unreachable!()
}

// Transient error classification =====

/// Returns true for transient SQLite errors that callers should retry.
///
/// `SQLITE_BUSY` (code 5) means another connection holds a lock that prevents
/// progress; `SQLITE_LOCKED` (code 6) means a shared-cache / table-level lock
/// blocks progress. rusqlite collapses extended codes (`SQLITE_BUSY_SNAPSHOT`,
/// `SQLITE_LOCKED_SHAREDCACHE`, etc.) onto these primary variants, so a
/// primary-code match covers all transient lock-contention errors.
pub(crate) fn is_retryable(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

// Stale entry cleanup =====

/// Extract the PID from an instance_id string ("{pid}:{timestamp}").
fn pid_from_instance_id(instance_id: &str) -> Option<u32> {
    instance_id.split(':').next()?.parse().ok()
}

/// Check whether a process with the given PID is still alive.
#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    // kill(pid, 0) checks existence without sending a signal.
    // Returns 0 on success, or EPERM if the process exists but we lack permission.
    let ret = unsafe { libc::kill(pid as i32, 0) };
    if ret == 0 {
        return true;
    }
    let err = std::io::Error::last_os_error();
    err.raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn is_pid_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let result = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) };
    match result {
        Ok(handle) => {
            let _ = unsafe { CloseHandle(handle) };
            true
        }
        Err(_) => false,
    }
}

/// Delete entries from the `running` table whose process is no longer alive.
fn clean_stale_entries(conn: &rusqlite::Connection) -> Result<(), rusqlite::Error> {
    let our_instance = instance_id();
    let mut stmt = conn.prepare("SELECT DISTINCT instance_id FROM running WHERE instance_id != ?1")?;
    let stale_instances: Vec<String> = stmt
        .query_map([&our_instance], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|iid| pid_from_instance_id(iid).is_none_or(|pid| !is_pid_alive(pid)))
        .collect();

    for iid in &stale_instances {
        conn.execute("DELETE FROM running WHERE instance_id = ?1", [iid])?;
    }
    Ok(())
}

// Blocking checks =====

/// Determine whether test T (with labels L and serial filter F) can start
/// running, given the current state of the `running` table.
fn can_start(
    conn: &rusqlite::Connection,
    my_labels: &[Label],
    my_serial_filter: &str,
) -> Result<bool, rusqlite::Error> {
    // (a) Is a global-serial test running?
    let global_running: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM running WHERE serial_filter = ?1)",
        [SERIAL_ALL],
        |row| row.get(0),
    )?;
    if global_running {
        return Ok(false);
    }

    // (b) Does any running serial test's filter match my labels?
    {
        let mut stmt =
            conn.prepare("SELECT serial_filter FROM running WHERE serial_filter != '' AND serial_filter != ?1")?;
        let filters: Vec<String> = stmt
            .query_map([SERIAL_ALL], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for filter_str in &filters {
            if let Ok(filter) = LabelFilter::parse(filter_str) {
                if filter.matches(my_labels) {
                    return Ok(false);
                }
            }
        }
    }

    // (c) If I'm global-serial, is anything running?
    if my_serial_filter == SERIAL_ALL {
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM running", [], |row| row.get(0))?;
        return Ok(count == 0);
    }

    // (d) If I have a filter, does any running test's labels match it?
    if !my_serial_filter.is_empty() && my_serial_filter != SERIAL_ALL {
        if let Ok(filter) = LabelFilter::parse(my_serial_filter) {
            let sql = format!("SELECT EXISTS (SELECT 1 FROM running r WHERE {})", filter.to_sql());
            let blocked: bool = conn.query_row(&sql, [], |row| row.get(0))?;
            if blocked {
                return Ok(false);
            }
        }
    }

    Ok(true)
}

// Registration =====

/// Register a running test in the coordination database.
/// Returns the row ID used for cleanup.
///
/// Must be called inside an active transaction: the two INSERTs are not atomic
/// at the function level, and a mid-call failure leaves a half-inserted row
/// that the caller's surrounding txn must roll back.
fn register(
    conn: &rusqlite::Connection,
    name: &str,
    labels: &[Label],
    serial_filter: &str,
) -> Result<i64, rusqlite::Error> {
    conn.execute(
        "INSERT INTO running (instance_id, name, serial_filter) VALUES (?1, ?2, ?3)",
        rusqlite::params![instance_id(), name, serial_filter],
    )?;
    let id = conn.last_insert_rowid();
    for label in labels {
        conn.execute(
            "INSERT INTO labels (running_id, label) VALUES (?1, ?2)",
            rusqlite::params![id, label.name()],
        )?;
    }
    Ok(id)
}

// RAII guard =====

/// RAII guard that unregisters the test from the coordination database on drop.
/// Ensures cleanup even on panic (during stack unwinding).
pub(crate) struct TestRegistration {
    id: i64,
    db_path: PathBuf,
}

impl Drop for TestRegistration {
    fn drop(&mut self) {
        let cleanup = || -> Result<(), rusqlite::Error> {
            let conn = rusqlite::Connection::open(&self.db_path)?;
            conn.busy_timeout(Duration::from_secs(5))?;
            conn.execute_batch("PRAGMA foreign_keys = ON")?;
            conn.execute("DELETE FROM running WHERE id = ?1", [self.id])?;
            Ok(())
        };
        if let Err(e) = cleanup() {
            eprintln!("[skuld] warning: failed to unregister test from coordination DB: {e}");
        }
    }
}

// Public coordination API =====

/// Coordinate test execution: block until the test can start, register it,
/// and return a guard that unregisters it on drop.
///
/// Under lock contention (`SQLITE_BUSY` / `SQLITE_LOCKED`), retries via the
/// outer exponential backoff loop (10 ms → 200 ms cap). Emits a debug warning
/// after 60 s of continuous contention.
///
/// This is the main entry point called by the test runner for every test.
pub(crate) fn coordinate(
    db_path: &std::path::Path,
    name: &str,
    labels: &[Label],
    serial_filter: &str,
) -> TestRegistration {
    let conn = open_db(db_path);

    let mut backoff = Duration::from_millis(10);
    let max_backoff = Duration::from_millis(200);
    let warn_after = Duration::from_secs(60);
    let started = Instant::now();
    let mut warned = false;

    loop {
        let txn = || -> Result<Option<i64>, rusqlite::Error> {
            conn.execute_batch("BEGIN EXCLUSIVE")?;
            clean_stale_entries(&conn)?;
            if can_start(&conn, labels, serial_filter)? {
                let id = register(&conn, name, labels, serial_filter)?;
                conn.execute_batch("COMMIT")?;
                Ok(Some(id))
            } else {
                conn.execute_batch("ROLLBACK")?;
                Ok(None)
            }
        };

        match txn() {
            Ok(Some(id)) => {
                return TestRegistration {
                    id,
                    db_path: db_path.to_path_buf(),
                };
            }
            Ok(None) => { /* serial conflict — fall through to backoff */ }
            Err(ref e) if is_retryable(e) => {
                // Best-effort rollback. If the inner ROLLBACK already ran (e.g.
                // a row-iteration failed after the closure's ROLLBACK on the
                // can_start=false path) this is a no-op that returns
                // SQLITE_ERROR ("no transaction is active"); harmlessly discarded.
                let _ = conn.execute_batch("ROLLBACK");
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                panic!("skuld: coordination DB error: {e}");
            }
        }

        if !warned && started.elapsed() > warn_after {
            warned = true;
            skuld_debug_eprintln!("coordination: {name} has been waiting >60s for serial constraints");
        }

        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(max_backoff);
    }
}

macro_rules! skuld_debug_eprintln {
    ($($arg:tt)*) => {
        if crate::runner::skuld_debug() {
            eprintln!("[skuld-debug] {}", format_args!($($arg)*));
        }
    };
}
use skuld_debug_eprintln;
