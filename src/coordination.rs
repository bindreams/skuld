//! SQLite-based cross-process test coordination for serial execution.
//!
//! Every test (serial or not) registers itself in a shared SQLite database
//! before running. Serial tests use this registry to block until their
//! serialization constraints are satisfied.

#[cfg(test)]
mod coordination_tests;
mod lock;
#[cfg(test)]
mod lock_tests;
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

/// Open a connection to `path`, serialized against every other
/// `connect`/`open_db` call for the same path by [`lock::with_init_lock`].
/// On Unix this asks forgiveness rather than permission: the open (no
/// `SQLITE_OPEN_CREATE`) is tried first, and [`publish::ensure_published`]
/// only runs — followed by another open — when that open fails with
/// `SQLITE_CANTOPEN` *and* nothing is at `path` (`symlink_metadata` reports
/// `NotFound`). [`connect_locked`] — this function's body, and the same
/// ask-forgiveness retry described below — is what every real caller
/// actually uses: [`open_db`] calls it directly under its own longer-lived
/// lock acquisition, and so does [`TestRegistration::drop`]'s cleanup, for
/// the same reason: composing a second, nested [`lock::with_init_lock`] call
/// (which this function itself does) inside a closure that already holds
/// the lock would self-deadlock, since `flock`/`LockFileEx` locks are scoped
/// to the open file description, not the process — a second open on the
/// same path blocks even from the very thread already holding the first.
/// This standalone wrapper exists only for tests that want [`connect_locked`]'s
/// exact contract — including the lock acquisition — without also paying for
/// [`open_db`]'s schema initialization.
///
/// Holding the init lock for the whole sequence removes any *Skuld* process
/// as a source of that `CANTOPEN`+absence: while it's held, this call is the
/// only create-or-publish attempt for `path` anywhere in the system, so a
/// `symlink_metadata` recheck that still finds nothing there cannot be a
/// concurrent Skuld publisher's rename landing in the gap — it is either
/// genuine absence (something outside Skuld deleted `path`; `ensure_published`
/// republishes and the open is retried, uncapped, for as long as that keeps
/// happening) or a genuinely broken entry (a dangling symlink or a
/// directory, which `ensure_published`'s no-replace rename can't turn into a
/// usable file and `symlink_metadata` reports as present rather than
/// `NotFound`) — external interference either way, not a Skuld-internal race
/// to retry through. A broken entry panics loudly, naming the path, rather
/// than being waited out.
///
/// A `.skuld.db` deleted mid-run is *not* what makes this panic, no matter
/// how many times it happens: every absence the open discovers, including a
/// repeat one after `ensure_published` already ran once, gets recreated
/// fresh at 0666, same as the very first connection of the run. Recreation
/// gets a new inode; any process that still holds a connection open to the
/// deleted file's old inode (POSIX doesn't invalidate an open fd on unlink)
/// keeps operating on that old inode, coordinating separately from
/// processes that connect afterward and see the new one. That split is
/// accepted under this module's minimal-publish design (see the module
/// doc): there's no verification of what's actually at `path` beyond
/// "something is," so nothing here would notice the swap to tell the two
/// groups apart, let alone reconcile them.
///
/// Windows is unchanged: no Windows lane mixes uids, so there's nothing for
/// the publish step to protect against there, and the open keeps its
/// default `SQLITE_OPEN_CREATE`. It still goes through the init lock, since
/// [`open_db`] needs that regardless of platform (see its doc) and a
/// single lock covering every `connect` call, not just the ones that could
/// race, keeps this function's contract uniform.
///
/// `#[cfg(all(test, unix))]`, not just `#[cfg(test)]`: its only caller,
/// `connect_panics_loudly_on_a_dangling_symlink_instead_of_recreating_the_db`,
/// is itself Unix-only (dangling symlinks and `ensure_published`'s
/// no-replace rename are both Unix-only concepts), so on a Windows test
/// build this would be dead code even under `cfg(test)`.
#[cfg(all(test, unix))]
pub(crate) fn connect(path: &std::path::Path) -> rusqlite::Connection {
    lock::with_init_lock(path, || connect_locked(path))
}

/// [`connect`]'s body, run by both [`connect`] and [`open_db`] while each
/// already holds `path`'s init lock — a shared inner helper so [`open_db`]
/// can keep its own connect-then-initialize sequence under one lock
/// acquisition instead of two, which would otherwise leave the gap between
/// them unprotected again.
fn connect_locked(path: &std::path::Path) -> rusqlite::Connection {
    #[cfg(unix)]
    {
        connect_with(path, publish::ensure_published)
    }

    #[cfg(not(unix))]
    rusqlite::Connection::open(path)
        .unwrap_or_else(|e| panic!("skuld: failed to open coordination DB at {path:?}: {e}"))
}

/// [`connect_locked`]'s Unix implementation, parameterized over the publish
/// step so `coordination_tests` can drive it without a real filesystem
/// race.
///
/// A single `symlink_metadata` call after a failed open can't, on its own,
/// tell a genuinely broken entry apart from genuine absence — see
/// [`connect`]'s doc for how holding `path`'s init lock resolves that
/// ambiguity.
///
/// Absence loops, uncapped, republishing and reopening each time: nothing
/// bounds how many times something outside Skuld can delete `path` between
/// this loop's publish and open, and giving up after one round would panic
/// on exactly the case [`connect`]'s own doc promises recreates fresh. A
/// broken entry never loops — it can't become un-broken by retrying — and
/// panics immediately.
#[cfg(unix)]
fn connect_with(path: &std::path::Path, ensure_published: impl FnMut(&std::path::Path)) -> rusqlite::Connection {
    connect_with_hooks(path, |_| {}, ensure_published)
}

/// [`connect_with`]'s actual implementation, additionally parameterized
/// over a hook run right after a failed open but before the
/// `symlink_metadata` recheck that follows it. Exists only so
/// `coordination_tests` can land a real publish in that exact gap and prove
/// `connect_with` panics — rather than silently succeeding — when a
/// concurrent, lock-unexcluded publisher wins that race; every real caller
/// goes through [`connect_with`] above, which passes a no-op here.
#[cfg(unix)]
fn connect_with_hooks(
    path: &std::path::Path,
    mut before_recheck: impl FnMut(&std::path::Path),
    mut ensure_published: impl FnMut(&std::path::Path),
) -> rusqlite::Connection {
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        | rusqlite::OpenFlags::SQLITE_OPEN_URI
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    loop {
        match rusqlite::Connection::open_with_flags(path, flags) {
            Ok(conn) => return conn,
            Err(e) => {
                if !is_cantopen(&e) {
                    panic!("skuld: could not open coordination DB {path:?}: {e}");
                }
                before_recheck(path);
                if !path_is_absent(path) {
                    panic!("skuld: could not open coordination DB {path:?}: {e}");
                }
                ensure_published(path);
                // Loop back and reopen; if something outside Skuld keeps
                // deleting `path` between publish and open, keep
                // republishing — see this function's doc.
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
/// Schema initialization requires a write lock. Under heavy concurrent
/// access, `PRAGMA journal_mode = WAL`'s own cold-start negotiation over the
/// (also just-being-created) `-shm` file can report `SQLITE_READONLY` to
/// whichever connection loses that particular race — not a lock-contention
/// error `busy_timeout` retries past, nor one a fresh connection retrying
/// the same `PRAGMA` reliably resolves either: the negotiation resolves the
/// *file*'s `-shm`, not any one connection's already-formed opinion of it,
/// so a connection that lands `SQLITE_READONLY` here can stay readonly for
/// its own lifetime regardless of retries on that connection. This race
/// itself has no direct reproduction in this crate's own test suite — see
/// the comment in `coordination_tests.rs` just above the Windows
/// `is_pid_alive` tests for what was tried and why it didn't reproduce
/// against pre-lock code. `tests/lock_contention_regression.rs` instead
/// guards the mechanism that removes the race deterministically:
/// [`lock::with_init_lock`]'s mutual exclusion itself, not the race's own
/// historical symptom.
///
/// [`lock::with_init_lock`] removes the race outright instead of retrying
/// past it: this whole function — [`connect_locked`] plus the WAL pragma,
/// schema creation and migration below — runs while holding `path`'s init
/// lock, so no other connection anywhere in the system can be negotiating
/// the same cold-start `-shm` creation concurrently. With nothing left to
/// race, one `execute_batch` attempt is enough.
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
    lock::with_init_lock(path, || {
        let conn = connect_locked(path);
        conn.busy_timeout(Duration::from_secs(5))
            .unwrap_or_else(|e| panic!("skuld: failed to set busy_timeout: {e}"));
        conn.execute_batch(init_sql)
            .unwrap_or_else(|e| panic!("skuld: failed to initialize coordination DB at {path:?}: {e}"));
        migrate_schema(&conn);
        conn
    })
}

// Test probe hooks =====

/// Probe hook for Skuld's own test suite (`tests/lock_contention_regression.rs`,
/// via the `lock_hold_probe` support binary): hold `path`'s real init lock —
/// the same [`lock::with_init_lock`] `connect`/`open_db` use — for exactly the
/// duration of `while_held`, so a driver process can deterministically prove
/// a second process's `try_lock` on the same lock file reports `WouldBlock`
/// while this one is running, then succeeds once it returns.
pub(crate) fn probe_hold_init_lock(path: &std::path::Path, while_held: impl FnOnce()) {
    lock::with_init_lock(path, while_held)
}

/// Probe hook for Skuld's own test suite (`tests/lock_contention_regression.rs`,
/// via the `lock_try_probe` support binary): open a *fresh* handle on `path`'s
/// lock target — never the one [`probe_hold_init_lock`] or any real
/// `connect`/`open_db` call holds — and attempt a non-blocking `try_lock` on
/// it, returning the raw result. A fresh handle matters here the same way it
/// does in `lock_tests.rs`'s in-process `try_lock` test: `flock`/`LockFileEx`
/// locks are scoped to the open file description/handle, not the process, so
/// only a genuinely separate handle (here, in a genuinely separate process)
/// can observe contention against the held lock.
pub(crate) fn probe_try_init_lock(path: &std::path::Path) -> Result<(), std::fs::TryLockError> {
    lock::try_lock_exclusive(&lock::open_lock_target(path))
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
        // Runs the connect *and* the first statement on it under `db_path`'s
        // init lock (like `open_db` does), not just a raw `connect()`: a
        // fresh connection's first write against a WAL database still
        // touches the `-shm` mapping, so this cleanup is itself a
        // participant in the cold-start negotiation `open_db`'s doc
        // describes, not a bystander exempt from it.
        let cleanup = || -> Result<(), rusqlite::Error> {
            lock::with_init_lock(&self.db_path, || {
                let conn = connect_locked(&self.db_path);
                conn.busy_timeout(Duration::from_secs(5))?;
                conn.execute_batch("PRAGMA foreign_keys = ON")?;
                conn.execute("DELETE FROM running WHERE id = ?1", [self.id])?;
                Ok(())
            })
        };

        // `connect_locked` can panic (a publish failure, or SQLite itself
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
