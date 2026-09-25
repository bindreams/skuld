//! SQLite-based cross-process test coordination for serial execution.
//!
//! Every test (serial or not) registers itself in a shared SQLite database
//! before running. Serial tests use this registry to block until their
//! serialization constraints are satisfied.

#[cfg(test)]
mod coordination_tests;
#[cfg(unix)]
mod publish;
#[cfg(all(test, unix))]
mod publish_tests;

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

/// Current schema version. Bumped to 1 when LabelFilter canonicalization landed.
/// Older DBs may contain non-canonical `serial_filter` strings
/// from pre-canonicalization skuld; the migration in [`migrate_schema`] scrubs
/// them on first open after upgrade.
const SCHEMA_VERSION: i64 = 1;

/// Open a connection to `path`. On Unix this asks forgiveness rather than
/// permission: the open (no `SQLITE_OPEN_CREATE`) is tried first, and
/// [`publish::ensure_published`] only runs — followed by a retried open —
/// when that open fails with `SQLITE_CANTOPEN` *and* nothing is at `path`
/// (`symlink_metadata` reports `NotFound`). The loop keeps going for as
/// long as the failure keeps being genuine absence, with no attempt cap, so
/// a concurrent delete anywhere in the open-publish-open sequence — even
/// one landing in the retry's own gap — just costs another round trip
/// instead of a panic: each iteration re-derives "is it really absent?"
/// from the open syscall itself, not from a stat taken earlier. Every
/// caller in this module goes through this one helper, including
/// [`TestRegistration::drop`].
///
/// A `.skuld.db` deleted mid-run is *not* what makes this panic: any
/// absence the open discovers gets recreated fresh at 0666, same as the
/// very first connection of the run. Recreation gets a new inode; any
/// process that still holds a connection open to the deleted file's old
/// inode (POSIX doesn't invalidate an open fd on unlink) keeps operating on
/// that old inode, coordinating separately from processes that connect
/// afterward and see the new one. That split is accepted under this
/// module's minimal-publish design (see the module doc): there's no
/// verification of what's actually at `path` beyond "something is," so
/// nothing here would notice the swap to tell the two groups apart, let
/// alone reconcile them.
///
/// What *does* panic: an open failure that isn't plain absence. A dangling
/// symlink at `path` is the one shape `ensure_published` can't turn into a
/// usable file: its no-replace rename reports `EEXIST` against the symlink
/// regardless of what it points to (or that it points to nothing), so
/// publishing no-ops, and `symlink_metadata` (which doesn't follow the
/// link) reports the link itself rather than `NotFound` — so this isn't
/// treated as absence. A directory at `path` is the same shape. Neither is
/// a transient condition to wait out; both are external interference, and
/// the open's repeated failure eventually panics loudly, naming the path.
///
/// "Eventually" is one extra attempt, not zero: a bare `symlink_metadata`
/// call right after a failed open cannot tell a genuinely broken entry
/// apart from a plain race, where a *concurrent* connection's publish
/// lands in the gap between this thread's failed open and its own
/// recheck — both present identically as "CANTOPEN, then something's
/// there." The two are told apart behaviorally: the loop always gives a
/// not-plain-absence result one retried open before it panics, since a
/// race resolves on that retry (the file is genuinely there now) while a
/// broken entry doesn't. That grace attempt is spent once per *round* of
/// not-absence, not once per call — a fresh round of genuine absence
/// (`ensure_published` ran again) always gets its own.
///
/// Windows is unchanged: no Windows lane mixes uids, so there's nothing for
/// the publish step to protect against there, and the open keeps its
/// default `SQLITE_OPEN_CREATE`.
pub(crate) fn connect(path: &std::path::Path) -> rusqlite::Connection {
    #[cfg(unix)]
    {
        connect_with(path, publish::ensure_published)
    }

    #[cfg(not(unix))]
    rusqlite::Connection::open(path)
        .unwrap_or_else(|e| panic!("skuld: failed to open coordination DB at {path:?}: {e}"))
}

/// [`connect`]'s Unix implementation, parameterized over the publish step so
/// `coordination_tests` can prove the retry loop genuinely repeats — not
/// just tolerates a single race — by controlling exactly when `.skuld.db`
/// comes back into existence.
///
/// A single `symlink_metadata` call after a failed open cannot tell two
/// shapes apart, because both present identically — `CANTOPEN`, then
/// `symlink_metadata` says something is there: a genuinely broken entry
/// (dangling symlink, directory), and a plain race, where the path was
/// absent at the moment the open failed and a concurrent publisher's
/// rename landed in the window between that failure and this recheck. The
/// only way to tell them apart is behavioral: retry the open once more. A
/// race resolves (the file is there for real; the retry succeeds); a
/// genuinely broken entry doesn't (the retry fails the same way). Only a
/// CANTOPEN that stays unresolved through that single extra attempt panics
/// — `retried_after_existing` tracks whether this round already got that
/// grace attempt, and only the *not-absent* branch consumes it: the
/// genuine-absence branch below always resets it, so every fresh round of
/// real absence gets its own grace attempt too, with no cap on how many
/// rounds of genuine absence the loop as a whole will retry through.
#[cfg(unix)]
fn connect_with(path: &std::path::Path, mut ensure_published: impl FnMut(&std::path::Path)) -> rusqlite::Connection {
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        | rusqlite::OpenFlags::SQLITE_OPEN_URI
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let mut retried_after_existing = false;
    loop {
        match rusqlite::Connection::open_with_flags(path, flags) {
            Ok(conn) => return conn,
            Err(e) => {
                if !is_cantopen(&e) {
                    panic!("skuld: could not open coordination DB {path:?}: {e}");
                }
                if path_is_absent(path) {
                    retried_after_existing = false;
                    ensure_published(path);
                    continue;
                }
                if retried_after_existing {
                    panic!("skuld: could not open coordination DB {path:?}: {e}");
                }
                retried_after_existing = true;
                continue;
            }
        }
    }
}

/// True when `err` is SQLite's own `SQLITE_CANTOPEN`.
#[cfg(unix)]
fn is_cantopen(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(inner, _) if inner.code == rusqlite::ErrorCode::CannotOpen
    )
}

/// True when nothing exists at `path` (`symlink_metadata`, not `exists()` —
/// a dangling symlink is still "something is there," same distinction
/// `ensure_published`'s own fast path draws).
#[cfg(unix)]
fn path_is_absent(path: &std::path::Path) -> bool {
    matches!(
        std::fs::symlink_metadata(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    )
}

/// Open a connection to the coordination database, creating it and the schema
/// if necessary. Each call returns a fresh connection suitable for single-thread
/// use.
///
/// Schema initialization requires a write lock. Under heavy concurrent access
/// (many connections opening the same freshly-published, still-empty DB at
/// once), busy_timeout alone isn't enough: `PRAGMA journal_mode = WAL`'s own
/// cold-start negotiation over the (also just-being-created) `-shm` file can
/// report `SQLITE_READONLY` to whichever connection loses that particular
/// race, same as `SQLITE_BUSY`/`SQLITE_LOCKED` for an ordinary write lock —
/// all three mean "someone else has it right now," not "this connection
/// can't write here." `is_transient_init_error` checks the error *code*, not
/// its message text, so this isn't classifying by wording that could shift
/// between SQLite versions.
///
/// A loser of that negotiation reopens a fresh connection before retrying,
/// rather than retrying `execute_batch` on the one it already has: measured
/// (`connect_survives_many_threads_racing_the_same_absent_path`, run
/// repeatedly), a connection that lands `SQLITE_READONLY` here stays
/// readonly for its own lifetime no matter how many times the same
/// statement is retried on it — the cold-start negotiation resolves the
/// *file*, not this connection's already-formed opinion of it, so nothing
/// short of a new `sqlite3_open` call ever sees the resolution. `connect`,
/// not a bare retry, is what actually observes the winner's progress.
pub(crate) fn open_db(path: &std::path::Path) -> rusqlite::Connection {
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
    let mut conn = connect(path);
    for attempt in 0..50 {
        conn.busy_timeout(Duration::from_secs(5))
            .unwrap_or_else(|e| panic!("skuld: failed to set busy_timeout: {e}"));
        match conn.execute_batch(init_sql) {
            Ok(()) => {
                migrate_schema(&conn);
                return conn;
            }
            Err(e) if is_transient_init_error(&e) && attempt < 49 => {
                std::thread::sleep(Duration::from_millis(100));
                conn = connect(path);
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

/// Returns true for transient errors `open_db`'s schema-initialization retry
/// loop should retry past, rather than panic on. A superset of
/// [`is_retryable`]'s lock-contention codes: `SQLITE_READONLY` joins them
/// here because switching a freshly-created, still-empty database into WAL
/// mode makes every racing connection negotiate creation of the same
/// `-shm` file, and a connection that loses that particular negotiation
/// sees `SQLITE_READONLY`, not `SQLITE_BUSY` — transient for the same
/// reason, just a different code. This is deliberately not folded into
/// `is_retryable` itself: that function's own contract and test
/// (`is_retryable_matches_busy_and_locked_only`) are about ordinary query
/// lock contention, a narrower claim than "retry during schema init."
fn is_transient_init_error(err: &rusqlite::Error) -> bool {
    is_retryable(err) || matches!(err.sqlite_error_code(), Some(rusqlite::ErrorCode::ReadOnly))
}

// Schema migration =====

/// Run all pending schema migrations. Gated by `PRAGMA user_version` and
/// performed inside `BEGIN IMMEDIATE` so concurrent test binaries don't race.
/// Once a migration completes, the version pragma is bumped and subsequent
/// connections skip the work.
fn migrate_schema(conn: &rusqlite::Connection) {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap_or(0);
    if current >= SCHEMA_VERSION {
        return;
    }
    // Take an immediate write lock so two processes don't both start scrubbing.
    if let Err(e) = conn.execute_batch("BEGIN IMMEDIATE") {
        eprintln!("[skuld] warning: failed to acquire migration lock: {e}");
        return;
    }
    // Re-check inside the transaction in case another process beat us to it.
    let inside_tx: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap_or(0);
    if inside_tx >= SCHEMA_VERSION {
        let _ = conn.execute_batch("COMMIT");
        return;
    }
    if current < 1 {
        scrub_serial_filters_v1(conn);
    }
    if let Err(e) = conn.execute(&format!("PRAGMA user_version = {SCHEMA_VERSION}"), []) {
        eprintln!("[skuld] warning: failed to bump schema version: {e}");
    }
    if let Err(e) = conn.execute_batch("COMMIT") {
        eprintln!("[skuld] warning: failed to commit schema migration: {e}");
    }
}

/// Migration v0 → v1: rewrite every `serial_filter` to its canonical Display
/// form, collapsing `Const(true)` and `Const(false)` to the `*` and `""`
/// sentinels respectively. Rows that fail to parse are LEFT ALONE if their
/// owning instance is alive (touching them might break a running test's
/// serialization invariants); only the standard `clean_stale_entries` path
/// removes them later.
fn scrub_serial_filters_v1(conn: &rusqlite::Connection) {
    let mut stmt =
        match conn.prepare("SELECT id, instance_id, serial_filter FROM running WHERE serial_filter NOT IN ('', ?1)") {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[skuld] warning: schema scrub: prepare failed: {e}");
                return;
            }
        };
    let rows: Vec<(i64, String, String)> = match stmt.query_map([SERIAL_ALL], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    }) {
        Ok(it) => it.filter_map(|r| r.ok()).collect(),
        Err(e) => {
            eprintln!("[skuld] warning: schema scrub: query failed: {e}");
            return;
        }
    };
    for (id, iid, raw) in rows {
        match LabelFilter::parse(&raw) {
            Ok(filter) => {
                let canonical = if filter.is_tautology() {
                    SERIAL_ALL.to_string()
                } else if filter.is_contradiction() {
                    SERIAL_NONE.to_string()
                } else {
                    filter.to_string()
                };
                if canonical != raw {
                    if let Err(e) = conn.execute(
                        "UPDATE running SET serial_filter = ?1 WHERE id = ?2",
                        rusqlite::params![canonical, id],
                    ) {
                        eprintln!("[skuld] warning: schema scrub: update id={id} failed: {e}");
                    }
                }
            }
            Err(e) => {
                let alive = pid_from_instance_id(&iid).is_some_and(is_pid_alive);
                if alive {
                    eprintln!(
                        "[skuld] warning: schema scrub: leaving unparseable serial_filter \
                         {raw:?} on running id={id} (owning instance {iid} alive): {e}"
                    );
                }
                // Dead-owner rows fall through to clean_stale_entries on the
                // next coordinate() call — no action here.
            }
        }
    }
}

/// Canonicalize a raw `serial_filter` string for storage in the DB.
///
/// Sentinels (`""`, `"*"`) pass through unchanged. Otherwise the string is
/// parsed (must be valid — `validate_serial_filters` guarantees this for
/// startup-registered tests) and the result of `LabelFilter::Display` is
/// returned, with `Const(true)` collapsed to `SERIAL_ALL` and `Const(false)`
/// collapsed to `SERIAL_NONE` so two semantically-equivalent declarations
/// (e.g. `serial = "a | !a"` and `serial = "*"`) share storage representation.
pub(crate) fn to_storage(serial_filter: &str) -> String {
    if serial_filter == SERIAL_NONE || serial_filter == SERIAL_ALL {
        return serial_filter.to_string();
    }
    let f = LabelFilter::parse(serial_filter)
        .expect("to_storage: serial filter must parse — guarded by validate_serial_filters");
    if f.is_tautology() {
        SERIAL_ALL.to_string()
    } else if f.is_contradiction() {
        SERIAL_NONE.to_string()
    } else {
        f.to_string()
    }
}

// Stale entry cleanup =====

/// Extract the PID from an instance_id string ("{pid}:{timestamp}").
fn pid_from_instance_id(instance_id: &str) -> Option<u32> {
    instance_id.split(':').next()?.parse().ok()
}

/// Check whether a process with the given PID is still alive.
#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    // `pid as i32` below is only meaningful for `0 < pid < 2^31`: `0` casts
    // to `kill(0, 0)`, which checks the *caller's own process group* (always
    // "exists") instead of a single process, and anything `>= 2^31` wraps
    // negative — `-1` (only `u32::MAX`) makes `kill` check every process the
    // caller may signal, and any other negative value makes it check the
    // process *group* whose id is the absolute value — neither is "this one
    // process still running". Every
    // caller of this function (the schema scrub, `clean_stale_entries`)
    // feeds it a `pid_from_instance_id`-parsed value, which only ever holds
    // a real `std::process::id()` in ordinary use and can't produce either
    // value — but the DB is world-writable, so any local uid can
    // insert a row with a `"0:..."` or out-of-range instance_id, and a
    // corrupted row could hold one too. No real process can have that PID,
    // so it's always safe to report it as not alive rather than let the
    // cast reinterpret it as a different check entirely.
    if pid == 0 || pid >= (1u32 << 31) {
        return false;
    }
    // kill(pid, 0) checks existence without sending a signal.
    // Returns 0 on success, or EPERM if the process exists but we lack permission.
    let ret = unsafe { libc::kill(pid as i32, 0) };
    if ret == 0 {
        return true;
    }
    let err = std::io::Error::last_os_error();
    err.raw_os_error() == Some(libc::EPERM)
}

/// Classify a failed `OpenProcess`'s error as meaning the process is gone,
/// or something else. Mirrors Unix's `EPERM` handling above: only "no such
/// process" means dead. `OpenProcess` reports a nonexistent PID as
/// `ERROR_INVALID_PARAMETER`; anything else — `ERROR_ACCESS_DENIED`
/// included, e.g. a process owned by another user, or a
/// protected/elevated one — means the process exists but we can't query
/// it, same shape as Unix's EPERM. A small pure function, taking just the
/// `HRESULT` rather than the live `windows::core::Error`, so this
/// classification can be unit-tested without needing a real failing
/// `OpenProcess` call.
///
/// **Known hang hazard, same class as Unix `EPERM` above, not fixed here:**
/// `clean_stale_entries` treats an "exists but we can't query it" PID as
/// alive and leaves its `running` row in place. If that PID was actually
/// this instance's *own* long-dead owner, and the OS has since reused the
/// number for an unrelated process this uid can't `OpenProcess` (protected,
/// elevated, or another user's) — `ERROR_ACCESS_DENIED` — the row is never
/// cleaned up. A later test whose serial filter conflicts with that row then
/// blocks in `coordinate`'s loop for as long as the unrelated process lives,
/// which can be indefinitely. Fixing this needs a way to tell "this PID,
/// this instance" apart from "this PID, coincidentally reused," which
/// `instance_id`'s `"{pid}:{timestamp}"` format doesn't do across a reuse —
/// the same gap the existing cross-namespace `is_pid_alive` issue tracks.
/// That's an owner design question, not addressed by this change.
#[cfg(windows)]
fn win32_open_process_error_means_dead(code: windows::core::HRESULT) -> bool {
    code == windows::Win32::Foundation::ERROR_INVALID_PARAMETER.to_hresult()
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
        Err(e) => !win32_open_process_error_means_dead(e.code()),
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
            // Newly-inserted rows are canonicalized via `to_storage`, so parse
            // failures here indicate a legacy row that the schema scrub left
            // behind (owner alive, can't safely DELETE). Warn and skip; the
            // scrub will retry once the owner exits.
            match LabelFilter::parse(filter_str) {
                Ok(filter) => {
                    if filter.matches(my_labels) {
                        return Ok(false);
                    }
                }
                Err(e) => eprintln!(
                    "[skuld] warning: skipping unparseable serial_filter {filter_str:?} from running table: {e}"
                ),
            }
        }
    }

    // (c) If I'm global-serial, is anything running?
    if my_serial_filter == SERIAL_ALL {
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM running", [], |row| row.get(0))?;
        return Ok(count == 0);
    }

    // (d) If I have a filter, does any running test's labels match it?
    //
    // `my_serial_filter` always reaches us already canonical (coordinate()
    // routes through to_storage before INSERT, and validate_serial_filters
    // proves it parseable at startup). An Err here is a logic bug, not a
    // runtime condition — surface it loudly.
    if !my_serial_filter.is_empty() && my_serial_filter != SERIAL_ALL {
        let filter = LabelFilter::parse(my_serial_filter)
            .expect("can_start: my_serial_filter must parse — guarded by validate_serial_filters");
        let sql = format!("SELECT EXISTS (SELECT 1 FROM running r WHERE {})", filter.to_sql());
        let blocked: bool = conn.query_row(&sql, [], |row| row.get(0))?;
        if blocked {
            return Ok(false);
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
            let conn = connect(&self.db_path);
            conn.busy_timeout(Duration::from_secs(5))?;
            conn.execute_batch("PRAGMA foreign_keys = ON")?;
            conn.execute("DELETE FROM running WHERE id = ?1", [self.id])?;
            Ok(())
        };

        // `connect()` can panic (a publish failure, or SQLite itself
        // rejecting the file — e.g. it's been replaced by a directory —
        // is loud by design). Ordinarily that panic should propagate: a
        // genuinely broken DB is worth failing loudly over. But if this
        // drop is running because the *thread* is already unwinding
        // from a different, unrelated panic (e.g. the test itself failed), a
        // second uncaught panic here is a panic during a panic — Rust turns
        // that into `std::process::abort()` (`SIGABRT`), killing the whole
        // process rather than just this one failing test. `catch_unwind`
        // this call so we can tell those two cases apart and only let the
        // panic through in the case where it's safe to.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(cleanup));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                eprintln!("[skuld] warning: failed to unregister test from coordination DB: {e}");
            }
            Err(payload) => {
                if std::thread::panicking() {
                    // Already unwinding from another panic: downgrade to a
                    // loud warning instead of letting this one escape and
                    // aborting the process.
                    eprintln!("{}", downgraded_warning_message(&payload));
                } else {
                    // Normal drop, no concurrent unwind: this is the only
                    // panic in flight, so it's safe to let it through and
                    // fail loudly as designed.
                    std::panic::resume_unwind(payload);
                }
            }
        }
    }
}

/// Build the downgraded-warning line for a panic payload caught from the
/// cleanup closure above, for the case where the drop is already running
/// during another panic's unwind. Pulled out of `Drop::drop` as its own
/// function, taking `payload` by the same `&Box<dyn Any + Send>` shape
/// `Drop::drop` holds it in, so unit tests exercise the exact call
/// convention production code uses — not a stand-in for it. That matters
/// because of a real footgun: `Box<dyn Any + Send>` is itself `Any` via the
/// blanket impl, so `&payload` coerces to `&dyn Any` *over the Box*, not its
/// contents, and `downcast_ref` inside `panic_payload_message` would then
/// always miss. `payload.as_ref()` derefs through the Box first.
fn downgraded_warning_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    let msg = panic_payload_message(payload.as_ref());
    format!(
        "[skuld] warning: coordination DB cleanup panicked while already unwinding from another \
         panic (not re-raised, to avoid aborting the process): {msg}"
    )
}

/// Best-effort extraction of a panic payload's message, for the downgraded
/// warning path above. Panics are conventionally `&'static str` (from
/// `panic!("literal")`) or `String` (from `panic!("{}", ...)` and friends);
/// anything else prints as a fixed placeholder rather than guessing.
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.as_str()
    } else {
        "<non-string panic payload>"
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
    // Canonicalize once up front so every comparison and INSERT in the loop
    // below operates on the storage form (e.g. "a | !a" → "*").
    let canonical_filter = to_storage(serial_filter);

    let mut backoff = Duration::from_millis(10);
    let max_backoff = Duration::from_millis(200);
    let warn_after = Duration::from_secs(60);
    let started = Instant::now();
    let mut warned = false;
    let mut logged_first_wait = false;

    loop {
        let txn = || -> Result<Option<i64>, rusqlite::Error> {
            conn.execute_batch("BEGIN EXCLUSIVE")?;
            clean_stale_entries(&conn)?;
            if can_start(&conn, labels, &canonical_filter)? {
                let id = register(&conn, name, labels, &canonical_filter)?;
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
            Ok(None) => {
                if !logged_first_wait {
                    logged_first_wait = true;
                    skuld_debug_eprintln!(
                        "coordination: {name} is blocked on a serial constraint and will wait \
                         (if this run used a generated nextest tool-config-file, this means \
                         nextest scheduled it concurrently with a conflicting test anyway — the \
                         config may be stale; re-run `cargo skuld nextest gen --check`)"
                    );
                }
            }
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
