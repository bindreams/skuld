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
#[cfg(test)]
mod migrate_tests;
mod moved_db;
#[cfg(all(test, unix))]
mod moved_db_tests;
#[cfg(unix)]
mod publish;
#[cfg(all(test, unix))]
mod publish_tests;
#[cfg(test)]
mod test_hooks;
#[cfg(test)]
mod test_hooks_tests;

use crate::label::{Label, LabelFilter};

use moved_db::{DbIdentity, Failure};

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
/// actually uses: [`open_db`] is the only one, calling it directly under its
/// own longer-lived lock acquisition (composing a second, nested
/// [`lock::with_init_lock`] call — which this function itself does — inside
/// a closure that already holds the lock would self-deadlock, since
/// `flock`/`LockFileEx` locks are scoped to the open file description, not
/// the process — a second open on the same path blocks even from the very
/// thread already holding the first). [`TestRegistration`] connects exactly
/// once too, but indirectly, through [`coordinate`]'s own `open_db` call —
/// it never reconnects on drop; see its own doc for why that's load-bearing,
/// not incidental. This standalone wrapper exists only for tests that want
/// [`connect_locked`]'s exact contract — including the lock acquisition —
/// without also paying for [`open_db`]'s schema initialization.
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
/// A `.skuld.db` deleted mid-run is *not* what makes *this function* panic,
/// no matter how many times it happens: every absence the open discovers,
/// including a repeat one after `ensure_published` already ran once, gets
/// recreated fresh at 0666, same as the very first connection of the run.
/// Recreation gets a new inode, so any connection that opened the file
/// *before* the deletion — including one held open across the deletion
/// itself, since POSIX doesn't invalidate an open fd on unlink — is now
/// bound to that old, detached inode while this fresh `connect` sees the
/// new one. This function itself has no opinion about that: it just
/// recreates and returns a connection to whatever is at `path` right now.
///
/// What a connection does *after* that split matters a great deal, though:
/// writing through the old, detached one would silently corrupt whatever
/// now exists at `path` (`-wal`/`-shm` are identified by path, so a
/// recreated file's companions get mixed with the old connection's writes).
/// Skuld does not tolerate that. Every write against a connection held open
/// across more than one operation — [`coordinate`]'s own loop,
/// [`TestRegistration`]'s cleanup on drop, and [`open_db`]'s own schema
/// initialization and migration, all the way down to each row a migration
/// touches — checks [`DbIdentity::has_moved`] (see [`DbIdentity::panic_if_moved`])
/// immediately before every write attempt, including every attempt inside
/// an uncapped retry loop, not just once before entering one, and refuses
/// to write and panics loudly, naming `path`, the moment that connection's
/// file has been swapped out from under it. A `.skuld.db` deleted mid-run
/// is a contract violation Skuld surfaces as a failure, not a condition it
/// silently works around.
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
    lock::with_init_lock(path, |token| connect_locked(path, token))
}

/// [`connect`]'s body, run by both [`connect`] and [`open_db`] while each
/// already holds `path`'s init lock — the [`lock::InitLockHeld`] token
/// proves at compile time that *some* init lock is held (a caller with none
/// at all has no token to pass), and the `debug_assert_eq!` below checks
/// it's actually *this* `path`'s, not silently accepting a token for a
/// different one. A shared inner helper so [`open_db`] can keep its own
/// connect-then-initialize sequence under one lock acquisition instead of
/// two, which would otherwise leave the gap between them unprotected again.
fn connect_locked(path: &std::path::Path, init_lock: &lock::InitLockHeld<'_>) -> rusqlite::Connection {
    debug_assert_eq!(
        init_lock.path(),
        path,
        "connect_locked: called with a path different from the one whose init lock is held"
    );
    #[cfg(unix)]
    let conn = connect_with(path, publish::ensure_published);

    #[cfg(windows)]
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::default() | rusqlite::OpenFlags::SQLITE_OPEN_PRIVATE_CACHE,
    )
    .unwrap_or_else(|e| panic!("skuld: failed to open coordination DB at {path:?}: {e}"));

    // rusqlite's own `InnerConnection::open_with_flags` calls
    // `sqlite3_busy_timeout(db, 5000)` unconditionally on every connection it
    // opens (`inner_connection.rs`) — a fixed, capped, time-based wait this
    // crate does not want on *any* connection: every real caller retries
    // `SQLITE_BUSY` itself, uncapped and gated on the error code alone
    // ([`retry_busy`], [`coordinate`]'s own loop), so SQLite's own internal
    // busy handler blocking underneath that — silently, for up to 5 s,
    // before either loop ever sees the error to retry — is exactly the
    // "expiry as proof of failure" shape this crate's retry logic is built
    // to avoid, whether or not anything here calls `Connection::busy_timeout`
    // explicitly. Disabling it here, once, for every connection this
    // function ever returns (both platforms), is what actually makes this
    // crate's own retry loops the only wait in play.
    conn.busy_timeout(Duration::ZERO)
        .unwrap_or_else(|e| panic!("skuld: failed to disable rusqlite's default busy_timeout: {e}"));
    conn
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
    // `SQLITE_OPEN_PRIVATE_CACHE`: shared-cache mode is a *process-global*
    // toggle (`sqlite3_enable_shared_cache`, deprecated but not removed) —
    // once anything in the process turns it on, it applies to every
    // connection that doesn't explicitly opt out, and this process also
    // runs arbitrary user test code Skuld doesn't control. `is_retryable`'s
    // doc explains why that matters: `SQLITE_LOCKED` is only ever a
    // same-connection self-conflict here *because* no connection this crate
    // opens uses shared-cache mode — without this flag, user code enabling
    // it process-wide would make that no longer true, and a `SQLITE_LOCKED`
    // this crate then silently ignores as non-retryable could actually be a
    // legitimate shared-cache lock from another connection, resolvable by
    // retrying, not a bug.
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        | rusqlite::OpenFlags::SQLITE_OPEN_URI
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
        | rusqlite::OpenFlags::SQLITE_OPEN_PRIVATE_CACHE;
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
/// error [`retry_busy`] retries past, nor one a fresh connection retrying
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
/// [`lock::with_init_lock`] removes that specific race outright: this whole
/// function — [`connect_locked`] plus the WAL pragma, schema creation and
/// migration below — runs while holding `path`'s init lock, so no other
/// connection anywhere in the system can be negotiating the same cold-start
/// `-shm` creation concurrently.
///
/// It does not remove ordinary `SQLITE_BUSY` contention from a connection
/// that never takes this init lock at all — every real
/// [`coordinate`] caller past its own `open_db` call is exactly that. The
/// `execute_batch` below is normally a same-schema no-op once any process
/// has created the tables once (SQLite doesn't need a lock to re-affirm a
/// schema that's already there), so in ordinary all-Skuld operation this
/// contention only bites the very first schema creation ever, racing
/// something outside Skuld's own locking discipline that happens to hold a
/// competing lock on the file at that instant. [`retry_busy`] handles it
/// regardless of source: [`connect_locked`] disables rusqlite's own default
/// 5 s `busy_timeout` on this connection, so a transient busy error
/// surfaces immediately instead of first blocking inside that internal,
/// capped handler, and is retried here, uncapped, on the error code alone.
/// Returns the connection plus the [`DbIdentity`] recorded for it — see
/// [`DbIdentity::record_main`] (the main file, recorded before schema init) and
/// [`DbIdentity::with_companions`] (`-wal`/`-shm`, recorded after: see its own doc for
/// why they don't reliably exist before then). Callers that need to keep
/// checking this connection's identity across more than one later
/// operation — [`coordinate`], [`TestRegistration`] — use this returned
/// value directly rather than deriving their own: a *second*, independent
/// `stat` of `path` done later, even immediately after this call returns,
/// would reopen exactly the race [`DbIdentity::record_main`]'s own doc explains,
/// just moved to a different call site instead of fixed.
pub(crate) fn open_db(path: &std::path::Path) -> (rusqlite::Connection, DbIdentity) {
    lock::with_init_lock(path, |token| {
        let conn = connect_locked(path, token);
        #[cfg(test)]
        test_hooks::run_seam(test_hooks::Seam::Open);
        let identity = DbIdentity::record_main(&conn, path);
        ensure_schema_locked(path, &conn, &identity, token);
        #[cfg(test)]
        test_hooks::run_seam(test_hooks::Seam::SchemaInit);
        let identity = identity.with_companions(&conn, path);
        (conn, identity)
    })
}

/// The schema-creation SQL [`ensure_schema_locked`] runs. A `const`, not a
/// local inside it, so its shape is visible without inlining the function.
const INIT_SQL: &str = "PRAGMA journal_mode = WAL;
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

/// Ensure `conn`'s schema exists and is migrated to [`SCHEMA_VERSION`].
/// **Requires proof the caller already holds `path`'s init lock** — the
/// [`lock::InitLockHeld`] token, obtainable only from inside
/// [`lock::with_init_lock`]'s own closure — since this does not acquire the
/// lock itself: composing a second, nested `with_init_lock` call inside a
/// closure that already holds it would self-deadlock (see [`connect`]'s
/// doc). The token only proves *some* lock is held at compile time; the
/// `debug_assert_eq!` below is what actually checks it's the lock for
/// `path`, not merely a token for a different one passed by mistake — `path`
/// itself, not `conn.path()`, since `Connection::path()` returns SQLite's
/// own canonicalized filename (resolving `.`, `..`, and symlinks — e.g.
/// macOS's `/tmp` → `/private/tmp`), which can legitimately differ from the
/// exact string a caller passed in even when both name the same file, and a
/// caller's own `path` is what `init_lock.path()` was actually built from.
/// [`open_db`] is this function's only caller: every real connection this
/// crate hands out goes through `open_db` once, up front, and keeps using
/// that same connection afterward (see [`TestRegistration`]'s own doc for
/// why it never reconnects), so this only ever runs once per connection's
/// lifetime, not on every operation against it.
///
/// Idempotent regardless: `CREATE TABLE IF NOT EXISTS` is a same-schema
/// no-op once any process has run this once (see [`open_db`]'s doc).
///
/// `identity` — [`DbIdentity`], recorded by
/// [`open_db`] right after `conn` was opened — is checked (via
/// [`DbIdentity::panic_if_moved`]) before every write here, inside
/// [`retry_busy`]'s retried closure rather than once outside it: this
/// schema-init write can retry for an uncapped amount of time under
/// contention, and `.skuld.db` deleted or replaced mid-retry is exactly the
/// hazard [`TestRegistration::drop`]'s own cleanup already guards this way.
fn ensure_schema_locked(
    path: &std::path::Path,
    conn: &rusqlite::Connection,
    identity: &DbIdentity,
    init_lock: &lock::InitLockHeld<'_>,
) {
    debug_assert_eq!(
        init_lock.path(),
        path,
        "ensure_schema_locked: called with a path different from the one whose init lock is held"
    );
    or_panic(
        conn,
        path,
        identity,
        "failed to initialize coordination DB",
        retry_busy(conn, || {
            identity.panic_if_moved(conn, path);
            panic_on_split_lock(init_lock);
            conn.execute_batch(INIT_SQL)
        }),
    );
    // Defense for the unavoidable window between the last successful write
    // above and this check: a move landing exactly there must still end
    // loud, not slip through because nothing checked again afterward.
    identity.panic_if_moved(conn, path);
    panic_on_split_lock(init_lock);
    migrate_schema(conn, path, identity);
    panic_on_split_lock(init_lock);
}

/// Panic loudly if `init_lock`'s own target has split (see
/// [`lock::InitLockHeld::target_has_split`]) — the same "this handle no
/// longer excludes what a fresh opener would" hazard [`DbIdentity::panic_if_moved`]
/// guards against for the DB file itself, applied to the profile directory
/// the init lock is held on. Checked at the same points as the DB-file
/// moved check throughout [`ensure_schema_locked`]'s writes: before every
/// retried attempt, and once more after, so a wholesale directory
/// replacement landing anywhere during schema init — the only window this
/// lock is ever actually held across more than an instant — still ends
/// loud.
fn panic_on_split_lock(init_lock: &lock::InitLockHeld<'_>) {
    if init_lock.target_has_split() {
        panic!(
            "skuld: coordination DB init lock for {:?} was split — its profile directory was \
             replaced wholesale mid-run",
            init_lock.path()
        );
    }
}

/// `result`'s value, or a panic naming `path` (see
/// [`DbIdentity::failure_message`]). The failure is captured before anything
/// else touches `conn`.
fn or_panic<T>(
    conn: &rusqlite::Connection,
    path: &std::path::Path,
    identity: &DbIdentity,
    context: &str,
    result: Result<T, rusqlite::Error>,
) -> T {
    result.unwrap_or_else(|e| {
        let failure = Failure::capture(conn, e);
        panic!("{}", identity.failure_message(conn, &failure, path, context))
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
    lock::with_init_lock(path, |_token| while_held())
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

/// Returns true for the one SQLite error retrying can actually resolve:
/// `SQLITE_BUSY` (primary code 5) — another connection holds a lock that
/// prevents progress, and will eventually release it. rusqlite collapses
/// extended codes (`SQLITE_BUSY_SNAPSHOT`, `SQLITE_BUSY_RECOVERY`, etc.) onto
/// this primary variant, so a primary-code match covers every shape of it.
///
/// `SQLITE_LOCKED` (code 6) is deliberately *not* included, even though it's
/// also nominally a lock-contention code: it means a shared-cache-mode
/// table-level lock held by a *different* connection sharing that cache, or
/// — the only way it can arise here — a conflict with a statement still
/// pending on the *same* connection (e.g. a prepared statement that hasn't
/// been fully stepped or reset, still holding a cursor open). No connection
/// this crate ever opens enables shared-cache mode, so every `SQLITE_LOCKED`
/// reachable here is the same-connection case: permanent for as long as that
/// pending statement stays open, and retrying the same failing call cannot
/// make it go away — only finishing or resetting that other statement can.
/// Treating it as retryable would spin uselessly against a bug in this
/// crate's own code, not wait out a real external condition.
pub(crate) fn is_retryable(err: &rusqlite::Error) -> bool {
    matches!(err.sqlite_error_code(), Some(rusqlite::ErrorCode::DatabaseBusy))
}

/// Retry `f` for as long as it fails with a transient [`is_retryable`] error,
/// backing off the same way [`coordinate`]'s own polling loop does (10 ms →
/// 200 ms, doubling), uncapped: the condition being waited on — some other
/// connection's transaction releasing the lock it holds — is real and will
/// resolve, so nothing here treats elapsed time as proof of anything.
/// `is_retryable`'s error-code check is the only thing that decides whether
/// to keep going. A non-retryable `Err`, or eventual `Ok`, is returned
/// immediately as-is.
///
/// Exists so callers that need a lock/write to eventually succeed under
/// SQLite-level contention (schema creation in [`open_db`],
/// [`TestRegistration`] cleanup, [`migrate_schema`]) aren't at the mercy of
/// rusqlite's own default: every connection it opens gets a hardcoded 5 s
/// `sqlite3_busy_timeout` unless something turns it off ([`connect_locked`]
/// does, for exactly this reason) — a fixed, capped, time-based wait that
/// gives up and reports `SQLITE_BUSY` once its budget elapses, which is
/// exactly the "expiry as proof of failure" shape this crate's retry logic
/// elsewhere (this function included) is built to avoid.
///
/// **Precondition:** `conn` must be in autocommit mode (no transaction of
/// its own already open) whenever this is called, checked on entry and
/// before every retry. `SQLITE_BUSY_SNAPSHOT` — the one `is_retryable` case
/// this matters for — is reported when a WAL read transaction's snapshot
/// can't be promoted to a write past concurrent writes elsewhere; that's a
/// property of the snapshot the transaction already started with, which
/// retrying the same statement cannot change without ending that
/// transaction first. Every real call site here always calls this outside
/// any transaction it holds open itself, so the precondition costs nothing
/// to keep.
fn retry_busy<T>(
    conn: &rusqlite::Connection,
    mut f: impl FnMut() -> Result<T, rusqlite::Error>,
) -> Result<T, rusqlite::Error> {
    debug_assert!(
        conn.is_autocommit(),
        "retry_busy: conn must be in autocommit mode — see this function's doc for why \
         SQLITE_BUSY_SNAPSHOT is permanent inside an open transaction"
    );
    let mut backoff = Duration::from_millis(10);
    let max_backoff = Duration::from_millis(200);
    loop {
        match f() {
            Err(ref e) if is_retryable(e) => {
                debug_assert!(
                    conn.is_autocommit(),
                    "retry_busy: conn must still be in autocommit mode on a retry — see this function's doc"
                );
                #[cfg(test)]
                test_hooks::signal_retry();
                // Emitted on every retry, not just the first: deduplicating
                // repeated identical lines is a log handler's job, not this
                // loop's — collapsing them here would mean carrying state
                // whose only purpose is to decide what NOT to say, which is
                // itself a small piece of policy this function has no
                // business owning. Nothing about the retry loop itself
                // changes based on how many times this has already printed.
                skuld_debug_eprintln!(
                    "coordination: retrying a transient busy error against {:?} — uncapped, so if \
                     this never resolves, something is holding a lock on that database \
                     indefinitely",
                    conn.path().unwrap_or("<unknown>")
                );
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(max_backoff);
            }
            other => {
                if let Err(ref e) = other {
                    debug_assert!(
                        e.sqlite_error_code() != Some(rusqlite::ErrorCode::DatabaseLocked),
                        "retry_busy: got SQLITE_LOCKED against {:?}, which this crate never treats \
                         as retryable — see is_retryable's doc for why: no connection here enables \
                         shared-cache mode, so this can only be a same-connection self-conflict \
                         (a bug in this crate's own code), not an external condition worth waiting \
                         out: {e}",
                        conn.path().unwrap_or("<unknown>")
                    );
                }
                return other;
            }
        }
    }
}

// Schema migration =====

/// Run all pending schema migrations. Gated by `PRAGMA user_version` and
/// performed inside `BEGIN IMMEDIATE` so concurrent test binaries don't race.
/// Once a migration completes, the version pragma is bumped and subsequent
/// connections skip the work.
///
/// Every failure panics, naming `path` ([`or_panic`]): nothing is read as a
/// default or warned past, and a failed `COMMIT` rolls back first so the
/// connection is never left inside its transaction.
///
/// `identity` is checked before and after each write, not once on entry: the
/// `BEGIN IMMEDIATE` retry is uncapped and the writes after it still follow.
fn migrate_schema(conn: &rusqlite::Connection, path: &std::path::Path, identity: &DbIdentity) {
    // In `retry_busy` because `SQLITE_BUSY_RECOVERY` can still surface on a
    // plain WAL read: it is reported to a connection that must wait while
    // another runs WAL recovery.
    let current: i64 = or_panic(
        conn,
        path,
        identity,
        "failed to read the schema version",
        retry_busy(conn, || conn.query_row("PRAGMA user_version", [], |row| row.get(0))),
    );
    if current >= SCHEMA_VERSION {
        return;
    }
    // An immediate write lock, so two processes don't both scrub. Unlike the
    // read above this genuinely contends with a live `coordinate`'s
    // `BEGIN EXCLUSIVE`.
    or_panic(
        conn,
        path,
        identity,
        "failed to acquire the migration lock",
        retry_busy(conn, || {
            identity.panic_if_moved(conn, path);
            conn.execute_batch("BEGIN IMMEDIATE")
        }),
    );
    identity.panic_if_moved(conn, path);
    // Re-check inside the transaction in case another process beat us to it.
    let inside_tx: i64 = or_panic(
        conn,
        path,
        identity,
        "failed to re-read the schema version",
        conn.query_row("PRAGMA user_version", [], |row| row.get(0)),
    );
    if inside_tx >= SCHEMA_VERSION {
        commit_migration(conn, path, identity);
        return;
    }
    if current < 1 {
        scrub_serial_filters_v1(conn, path, identity);
    }
    identity.panic_if_moved(conn, path);
    or_panic(
        conn,
        path,
        identity,
        "failed to bump the schema version",
        conn.execute(&format!("PRAGMA user_version = {SCHEMA_VERSION}"), []),
    );
    identity.panic_if_moved(conn, path);
    commit_migration(conn, path, identity);
    identity.panic_if_moved(conn, path);
}

/// `COMMIT` the migration transaction. On failure, roll back (SQLite only
/// *might* have) and panic, so the connection is never handed on mid-transaction.
fn commit_migration(conn: &rusqlite::Connection, path: &std::path::Path, identity: &DbIdentity) {
    if let Err(e) = conn.execute_batch("COMMIT") {
        let failure = Failure::capture(conn, e);
        if !conn.is_autocommit() {
            // Best effort: the panic below is what reports the failure.
            let _ = conn.execute_batch("ROLLBACK");
        }
        panic!(
            "{}",
            identity.failure_message(conn, &failure, path, "failed to commit the schema migration")
        );
    }
}

/// Migration v0 → v1: rewrite every `serial_filter` to its canonical Display
/// form, collapsing `Const(true)` and `Const(false)` to the `*` and `""`
/// sentinels respectively. Rows that fail to parse are LEFT ALONE if their
/// owning instance is alive (touching them might break a running test's
/// serialization invariants); only the standard `clean_stale_entries` path
/// removes them later.
///
/// `identity` is checked before each row's `UPDATE`: this loops over an
/// arbitrary number of rows, and a move partway through must be caught before
/// the next one.
fn scrub_serial_filters_v1(conn: &rusqlite::Connection, path: &std::path::Path, identity: &DbIdentity) {
    let mut stmt = or_panic(
        conn,
        path,
        identity,
        "schema scrub: failed to prepare",
        conn.prepare("SELECT id, instance_id, serial_filter FROM running WHERE serial_filter NOT IN ('', ?1)"),
    );
    let rows: Vec<(i64, String, String)> = or_panic(
        conn,
        path,
        identity,
        "schema scrub: failed to read the running table",
        stmt.query_map([SERIAL_ALL], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .and_then(|it| it.collect::<Result<Vec<_>, _>>()),
    );
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
                    identity.panic_if_moved(conn, path);
                    or_panic(
                        conn,
                        path,
                        identity,
                        &format!("schema scrub: failed to update running id={id}"),
                        conn.execute(
                            "UPDATE running SET serial_filter = ?1 WHERE id = ?2",
                            rusqlite::params![canonical, id],
                        ),
                    );
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

/// RAII guard that unregisters the test from the coordination database on
/// drop. Ensures cleanup even on panic (during stack unwinding).
///
/// Keeps the very same [`rusqlite::Connection`] [`coordinate`] registered
/// `id` on — not just `id` and a path to reconnect with — and deletes
/// through that connection on drop, never a fresh one. That matters beyond
/// the connection split `.skuld.db` deleted mid-run causes (see
/// [`connect`]'s doc): reconnecting can delete a *different* test's row
/// outright. `id` comes from `running`'s `AUTOINCREMENT` column, and
/// `AUTOINCREMENT`'s no-reuse guarantee is scoped to one schema's lifetime,
/// tracked in `sqlite_sequence` — a `.skuld.db` deleted and recreated
/// mid-run starts that sequence over from 1. Two tests registered against
/// two different incarnations of the file can end up with the same numeric
/// `id`, and a reconnect-then-`DELETE FROM running WHERE id = ?` keyed on
/// that id alone has no way to tell which incarnation it's actually
/// deleting from — it deletes whatever currently has that id, correct
/// target or not. Holding the original connection sidesteps the ambiguity
/// instead of trying to detect it: this connection's open file descriptor
/// still points at the exact inode `id` was minted against (POSIX doesn't
/// invalidate an open fd on unlink), so every operation through it —
/// including this cleanup — is unambiguously scoped to that one
/// incarnation, whatever anyone else has since done to the path.
///
/// Scoped to the right incarnation is not the same as safe to write to,
/// though: if the path has moved on to a *different* incarnation, this
/// connection's own file is a detached, orphaned inode nobody else can see
/// or coordinate through — deleting the row here accomplishes nothing real,
/// and any write at all risks corrupting whatever now lives at the path (a
/// recreated file's `-wal`/`-shm`, identified by path rather than inode,
/// getting mixed with this connection's own writes). So `Drop` checks
/// [`DbIdentity::panic_if_moved`] before every `DELETE` attempt — inside the
/// retry loop, not just once outside it, and once more after — and
/// refuses to write once that's true, panicking loudly instead — Skuld
/// fails a mid-run `.skuld.db` deletion, it does not silently work around
/// it.
pub(crate) struct TestRegistration {
    conn: rusqlite::Connection,
    id: i64,
    /// The raw path `coordinate` was called with — not derived from
    /// `conn.path()`, which is SQLite's own canonicalized filename and can
    /// legitimately differ from the exact string a caller passed in (see
    /// `ensure_schema_locked`'s doc for the same distinction).
    path: std::path::PathBuf,
    /// [`DbIdentity`], recorded by [`open_db`] — see [`db_or_companions_have_moved`]'s
    /// doc for what this catches that `SQLITE_FCNTL_HAS_MOVED` alone
    /// doesn't.
    identity: DbIdentity,
}

impl Drop for TestRegistration {
    fn drop(&mut self) {
        // No reconnect, no init lock: `self.conn` is the exact connection
        // `coordinate` registered `id` on (see this struct's own doc for why
        // that's load-bearing, not just an optimization), so this cleanup
        // needs nothing beyond retrying past transient contention on it.
        // `PRAGMA foreign_keys = ON` isn't re-set here either — `ensure_schema_locked`
        // already set it on this exact connection when `coordinate` first
        // opened it, and that's a per-connection setting, not something a
        // fresh statement needs to re-assert.
        //
        // The DELETE can still genuinely contend with another,
        // already-initialized process mid-`BEGIN EXCLUSIVE` inside
        // `coordinate` — so [`retry_busy`] retries past a transient busy
        // error, uncapped, and panics loudly (via `unwrap_or_else`) on
        // anything else: a failure that's genuinely possible here is worth
        // surfacing, not just warning about.
        //
        // The moved-DB check runs *inside* the retried closure, before every
        // attempt — not once before entering `retry_busy` — because a file
        // moved partway through an uncapped retry loop is exactly as real a
        // hazard as one moved before `drop` was ever called; checking only
        // once outside the loop would write silently through a connection
        // that moved out from under it mid-retry
        // (`registration_drop_fails_loudly_when_the_db_moves_mid_retry` is
        // the regression test). A second check right after, once
        // `retry_busy` returns successfully, closes the one window neither
        // that nor the per-attempt check can: a move landing between the
        // last successful write and this function returning must still end
        // loud, not slip through unnoticed.
        let cleanup = || {
            or_panic(
                &self.conn,
                &self.path,
                &self.identity,
                "failed to unregister test from coordination DB",
                retry_busy(&self.conn, || {
                    self.identity.panic_if_moved(&self.conn, &self.path);
                    self.conn.execute("DELETE FROM running WHERE id = ?1", [self.id])
                }),
            );
            #[cfg(test)]
            test_hooks::run_seam(test_hooks::Seam::Delete);
            self.identity.panic_if_moved(&self.conn, &self.path);
        };

        // `cleanup` can panic on a non-retryable DB error. Ordinarily that
        // should propagate: an unregister that genuinely fails is worth
        // failing loudly over. But if this drop is running because the
        // *thread* is already unwinding from a different, unrelated panic
        // (e.g. the test itself failed), a second uncaught panic here is a
        // panic during a panic — Rust turns that into
        // `std::process::abort()` (`SIGABRT`), killing the whole process
        // rather than just this one failing test. `catch_unwind` this call
        // so we can tell those two cases apart and only let the panic
        // through in the case where it's safe to.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(cleanup));
        match result {
            Ok(()) => {}
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
pub(crate) fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> &str {
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
/// Under lock contention (`SQLITE_BUSY`), retries via the outer exponential
/// backoff loop (10 ms → 200 ms cap). Emits a debug warning after 60 s of
/// continuous contention.
///
/// This is the main entry point called by the test runner for every test.
pub(crate) fn coordinate(
    db_path: &std::path::Path,
    name: &str,
    labels: &[Label],
    serial_filter: &str,
) -> TestRegistration {
    // `open_db` returns the `DbIdentity` it already recorded — not
    // re-derived here via a second, independent `stat`: see
    // `record_identity`'s doc for why a second stat, done separately from
    // the open it's supposed to describe, reopens exactly the race that
    // function exists to close, just moved to a different call site.
    let (conn, identity) = open_db(db_path);
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
        // Before every coordination step, not just the first: a retry loop
        // can spend an arbitrary (uncapped) amount of time here under
        // contention, and `.skuld.db` deleted or replaced mid-retry is just
        // as real a hazard as one deleted before `coordinate` was ever
        // called.
        identity.panic_if_moved(&conn, db_path);

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
                #[cfg(test)]
                test_hooks::run_seam(test_hooks::Seam::Commit);
                identity.panic_if_moved(&conn, db_path);
                return TestRegistration {
                    conn,
                    id,
                    path: db_path.to_path_buf(),
                    identity,
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
                // `connect_locked` (via `open_db`) disabled rusqlite's own
                // default `busy_timeout` on `conn`, so this branch — not an
                // internal SQLite busy handler — is the only thing retrying
                // a transient busy error here; see `retry_busy`'s doc for
                // why. Not routed through `retry_busy` itself: this loop
                // already has its own uncapped backoff below (shared with
                // the semantic "blocked on a serial constraint" case above),
                // and re-runs the whole `BEGIN EXCLUSIVE` transaction on each
                // pass rather than just retrying a single statement.
                #[cfg(test)]
                test_hooks::signal_retry();
                skuld_debug_eprintln!(
                    "coordination: {name} is retrying a transient busy error against {:?} — \
                     uncapped, so if this never resolves, something is holding a lock on that \
                     database indefinitely",
                    conn.path().unwrap_or("<unknown>")
                );
            }
            Err(e) => {
                // Captured before the ROLLBACK, whose own failure would
                // overwrite the errno.
                let failure = Failure::capture(&conn, e);
                let _ = conn.execute_batch("ROLLBACK");
                // See `is_retryable`'s doc: `SQLITE_LOCKED` here can only be
                // a same-connection self-conflict, a bug in this crate's own
                // code, not a condition worth having retried past.
                debug_assert!(
                    failure.error().sqlite_error_code() != Some(rusqlite::ErrorCode::DatabaseLocked),
                    "coordinate: got SQLITE_LOCKED against {:?}: {}",
                    conn.path().unwrap_or("<unknown>"),
                    failure.error()
                );
                panic!(
                    "{}",
                    identity.failure_message(&conn, &failure, db_path, "coordination DB error")
                );
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
