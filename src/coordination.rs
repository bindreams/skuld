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
#[cfg(all(test, unix))]
mod moved_db_tests;
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
/// touches — checks [`db_has_moved`] (see [`panic_on_moved_db`])
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
/// [`record_identity`] (the main file, recorded before schema init) and
/// [`record_companions`] (`-wal`/`-shm`, recorded after: see its own doc for
/// why they don't reliably exist before then). Callers that need to keep
/// checking this connection's identity across more than one later
/// operation — [`coordinate`], [`TestRegistration`] — use this returned
/// value directly rather than deriving their own: a *second*, independent
/// `stat` of `path` done later, even immediately after this call returns,
/// would reopen exactly the race [`record_identity`]'s own doc explains,
/// just moved to a different call site instead of fixed.
pub(crate) fn open_db(path: &std::path::Path) -> (rusqlite::Connection, DbIdentity) {
    lock::with_init_lock(path, |token| {
        let conn = connect_locked(path, token);
        let main = record_identity(&conn, path);
        ensure_schema_locked(path, &conn, main, token);
        let identity = DbIdentity {
            main,
            #[cfg(unix)]
            companions: record_companions(path),
        };
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
/// `identity` — [`FileIdentity::of(path)`](FileIdentity::of), recorded by
/// [`open_db`] right after `conn` was opened — is checked (via
/// [`panic_on_moved_db`]) before every write here, inside
/// [`retry_busy`]'s retried closure rather than once outside it: this
/// schema-init write can retry for an uncapped amount of time under
/// contention, and `.skuld.db` deleted or replaced mid-retry is exactly the
/// hazard [`TestRegistration::drop`]'s own cleanup already guards this way.
fn ensure_schema_locked(
    path: &std::path::Path,
    conn: &rusqlite::Connection,
    identity: FileIdentity,
    init_lock: &lock::InitLockHeld<'_>,
) {
    debug_assert_eq!(
        init_lock.path(),
        path,
        "ensure_schema_locked: called with a path different from the one whose init lock is held"
    );
    retry_busy(conn, || {
        panic_on_moved_db(conn, path, identity);
        panic_on_split_lock(init_lock);
        conn.execute_batch(INIT_SQL)
    })
    .unwrap_or_else(|e| {
        if let Some(msg) = moved_db_message_for(conn, &e, path, identity) {
            panic!("{msg}");
        }
        panic!(
            "skuld: failed to initialize coordination DB at {:?}: {e}",
            conn.path().unwrap_or("<unknown>")
        )
    });
    // Defense for the unavoidable window between the last successful write
    // above and this check: a move landing exactly there must still end
    // loud, not slip through because nothing checked again afterward.
    panic_on_moved_db(conn, path, identity);
    panic_on_split_lock(init_lock);
    migrate_schema(conn, path, identity);
    panic_on_split_lock(init_lock);
}

/// Panic loudly if `init_lock`'s own target has split (see
/// [`lock::InitLockHeld::target_has_split`]) — the same "this handle no
/// longer excludes what a fresh opener would" hazard [`panic_on_moved_db`]
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

// Moved-database detection =====

/// A file's identity, following symlinks/reparse points — the same
/// resolution `stat` (Unix) or opening the path fresh (Windows) performs,
/// and the same resolution a fresh `connect`/`open_db` call on that path
/// would follow to reach a file. Skuld's own record, independent of
/// whatever SQLite's VFS tracks internally (see [`db_has_moved`]'s doc for
/// why that alone isn't enough): recorded once, right after a successful
/// open, while still holding `path`'s init lock.
///
/// That lock is narrower than it might sound: it only excludes *other
/// Skuld processes/threads* from creating or publishing at `path` (see
/// [`lock::with_init_lock`]'s doc) — it says nothing about a symlink or
/// junction ancestor of `path` being retargeted by something outside Skuld
/// entirely, which is a real, reachable case on both platforms (confirmed
/// on Windows by a throwaway CI probe — see [`record_identity`], this
/// type's only constructor for a live connection's own identity, for how
/// it's actually made safe against that).
///
/// On Unix, device+inode (`stat`). On Windows, volume serial number +
/// 64-bit file index (`GetFileInformationByHandle` on a *fresh* open of
/// the path, not the connection's own already-open handle — a held handle,
/// like a held Unix fd, doesn't observe a later retarget of an ancestor at
/// all, since the retarget only changes what a *new* path resolution would
/// reach). `FILE_SHARE_DELETE` being withheld on Windows (see
/// [`db_has_moved`]'s doc) only rules out the main file *itself* being
/// deleted or renamed while held open; it does nothing about an ancestor
/// reparse point, which is a property of the path, not of any open handle
/// on the file it currently resolves to — this is a real gap
/// `FILE_SHARE_DELETE` alone cannot close, not a redundant check.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(windows)]
    volume_serial: u32,
    #[cfg(windows)]
    file_index: u64,
}

impl FileIdentity {
    /// `None` means nothing this crate can resolve exists at `path` right
    /// now — that's as much "moved" as a mismatched identity is (see
    /// [`db_has_moved`]'s use of this).
    #[cfg(unix)]
    fn of(path: &std::path::Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| Self {
            dev: m.dev(),
            ino: m.ino(),
        })
    }

    /// A fresh `CreateFileW`-equivalent open (via `std::fs::File::open`,
    /// which follows reparse points, same as SQLite's own `winOpen` and
    /// Unix's `stat`), not the connection's own handle — see this type's
    /// own doc for why that distinction is what makes this catch an
    /// ancestor retarget at all.
    #[cfg(windows)]
    fn of(path: &std::path::Path) -> Option<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};

        let file = std::fs::File::open(path).ok()?;
        let handle = windows::Win32::Foundation::HANDLE(file.as_raw_handle());
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // Safety: `handle` is a valid, currently-open handle for as long as
        // `file` is alive, which outlives this call; `&mut info` is a valid
        // `*mut BY_HANDLE_FILE_INFORMATION` for the call to write into.
        unsafe { GetFileInformationByHandle(handle, &mut info) }.ok()?;
        Some(Self {
            volume_serial: info.dwVolumeSerialNumber,
            file_index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        })
    }
}

/// True once `conn`'s underlying main database file has been deleted,
/// renamed, or replaced since `recorded` was captured for `path` (see
/// [`FileIdentity::of`]) — two independent checks, both must pass for this
/// to answer `false`.
///
/// On Unix, the first is `SQLITE_FCNTL_HAS_MOVED`, which compares the
/// file's current identity against the one *SQLite itself* recorded at open
/// time by re-`stat`ing the exact path string it was opened with. That
/// path-string re-stat is also this check's blind spot: it only ever
/// re-resolves the literal string SQLite recorded, not `path` as this
/// crate's own caller understands it — a symlink *ancestor* of `path`
/// retargeted after open (`registration_drop_fails_loudly_when_a_parent_symlink_is_retargeted_mid_run`-shaped:
/// e.g. a per-run `link -> real1` swapped to `link -> real2` mid-run)
/// changes what `path` now means without SQLite's own re-stat necessarily
/// observing it, and the inode-only comparison SQLite does (`sqlite3.c`'s
/// `fileHasMoved`, comparing only `st_ino`) also never checks
/// *device* — two files on different filesystems can share an inode
/// number, a false-negative "not moved" on setups spanning multiple
/// devices/mounts. The second, independent check closes both: a fresh
/// `FileIdentity::of(path)`, comparing dev *and* ino against `recorded`.
/// Either check answering "moved" is enough — this crate does not need both
/// to agree, only needs to not miss a real one.
///
/// What neither check can ever catch, by construction, not a bug: content
/// overwritten *in place* at the same path, same device, same inode (e.g.
/// `cp other.db .skuld.db`, or any write that doesn't change the file's
/// identity) is indistinguishable from this connection's own ordinary
/// writes without fingerprinting the content itself, which neither check
/// attempts. "Moved" here means exactly "the path now names a different
/// file, or nothing" — not "the file changed."
///
/// On Windows, SQLite's VFS (`winFileControl`) has no case for
/// `SQLITE_FCNTL_HAS_MOVED` at all and always answers `SQLITE_NOTFOUND` —
/// confirmed against the `bundled` `sqlite3.c` this crate compiles, and
/// empirically by CI (windows/arm64 failed every coordination test with
/// that exact code once this check started running unconditionally). So on
/// Windows this function skips the file-control call entirely and relies
/// on `FileIdentity` alone — which, on Windows, is *not* redundant with
/// `winOpen`'s withheld `FILE_SHARE_DELETE`: that share mode rules out the
/// main file *itself* being deleted or renamed while any connection
/// (ours) holds it open, so the file a held-open connection is writing to
/// is provably the same *file* it opened
/// (`windows_open_db_file_blocks_delete_and_rename_while_held` exercises
/// this directly; if a future SQLite/rusqlite ever changes the share
/// mode, that test — not a corrupted database — catches it) — but it says
/// nothing about a symlink or junction *ancestor* of `path` being
/// retargeted, which changes what the *path* resolves to without
/// touching the file itself at all. A throwaway CI probe confirmed this
/// gap is real before `FileIdentity` got a real Windows implementation:
/// `TestRegistration::drop` succeeded silently after exactly this
/// retarget.
///
/// A single, fast, synchronous file-control call plus one `stat` on Unix;
/// a fresh handle open plus one `GetFileInformationByHandle` call on
/// Windows — no blocking, so checking this before every write costs
/// nothing worth avoiding it for.
fn db_has_moved(conn: &rusqlite::Connection, path: &std::path::Path, recorded: FileIdentity) -> bool {
    #[cfg(unix)]
    if has_moved_via_fcntl(conn) {
        return true;
    }
    FileIdentity::of(path) != Some(recorded)
}

/// `SQLITE_FCNTL_HAS_MOVED` alone — the first of [`db_has_moved`]'s two
/// checks, split out so [`record_identity`] can also use it, at record
/// time, independent of the [`FileIdentity`] comparison it's cross-checking
/// against.
#[cfg(unix)]
fn has_moved_via_fcntl(conn: &rusqlite::Connection) -> bool {
    let mut has_moved: std::os::raw::c_int = 0;
    let main = c"main";
    // Safety: `conn.handle()` is a valid, currently-open `sqlite3*` for as
    // long as `conn` is borrowed, which outlives this call; `main` is a
    // NUL-terminated C string naming the (only) attached database Skuld
    // ever uses; `&mut has_moved` is a valid `*mut c_int` for SQLite to
    // write its 0-or-1 answer into, matching what `SQLITE_FCNTL_HAS_MOVED`
    // documents it expects.
    let rc = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            conn.handle(),
            main.as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
            (&raw mut has_moved).cast(),
        )
    };
    assert_eq!(
        rc,
        rusqlite::ffi::SQLITE_OK,
        "skuld: SQLITE_FCNTL_HAS_MOVED file-control failed with code {rc} — this unix SQLite VFS \
         was expected to implement it, which means Skuld can no longer tell a moved database \
         apart from a live one and must not be trusted to keep writing"
    );
    has_moved != 0
}

/// Record `conn`'s main-file identity, right after a successful open, while
/// still holding `path`'s init lock.
///
/// **Not** derived from a fresh `stat` of `path` — deriving it from
/// `conn.path()` instead (the path string SQLite itself recorded
/// synchronously as part of the open that just completed) is the whole
/// point: a `stat` of `path` done independently, as a *separate* syscall
/// after the open, has no connection to what `conn` actually opened, and a
/// symlink ancestor retargeted in the gap between [`connect_locked`]'s open
/// and this call landing would silently record the *new* target's identity
/// as if it were this connection's own — with `db_has_moved` then agreeing
/// "not moved" against that wrong value for the rest of the connection's
/// life, since both the fcntl check (against SQLite's own, separately
/// recorded identity) and a fresh comparison against this wrong one would
/// keep passing.
///
/// The init lock held here does not rule this out by itself: it only
/// excludes *other Skuld processes/threads* from publishing at `path` — it
/// says nothing about a symlink ancestor of `path` being retargeted by
/// something outside Skuld entirely, which is exactly the case above.
///
/// So this cross-checks instead of merely trusting `conn.path()`: if
/// [`has_moved_via_fcntl`] already disagrees, or a fresh
/// [`FileIdentity::of`] of the caller's own `path` (what every later
/// [`db_has_moved`] call keeps checking against) doesn't match the
/// identity derived from `conn.path()`, something has already moved in the
/// open-to-record window — panics immediately, naming `path`, rather than
/// silently recording a value that would report "not moved" forever
/// regardless of what actually happened.
fn record_identity(conn: &rusqlite::Connection, path: &std::path::Path) -> FileIdentity {
    #[cfg(unix)]
    {
        assert!(
            !has_moved_via_fcntl(conn),
            "skuld: coordination DB {path:?} was already reported moved immediately after being \
             opened — something retargeted it in the open-to-record window"
        );
        let sqlite_path = conn
            .path()
            .unwrap_or_else(|| panic!("skuld: coordination DB connection for {path:?} has no path"));
        let identity = FileIdentity::of(std::path::Path::new(sqlite_path))
            .unwrap_or_else(|| panic!("skuld: coordination DB {path:?} vanished immediately after being opened"));
        assert_eq!(
            FileIdentity::of(path),
            Some(identity),
            "skuld: coordination DB {path:?} disagreed with the connection just opened through \
             it — something retargeted it in the open-to-record window"
        );
        identity
    }
    #[cfg(windows)]
    {
        // No `SQLITE_FCNTL_HAS_MOVED` equivalent to cross-check against on
        // Windows (see `db_has_moved`'s doc) — but the same open-to-record
        // race `record_identity`'s own doc describes for Unix is just as
        // real here, so this still cross-checks against the connection's
        // own resolved path rather than merely trusting a single `stat` of
        // the caller's `path`.
        let sqlite_path = conn
            .path()
            .unwrap_or_else(|| panic!("skuld: coordination DB connection for {path:?} has no path"));
        let identity = FileIdentity::of(std::path::Path::new(sqlite_path))
            .unwrap_or_else(|| panic!("skuld: coordination DB {path:?} vanished immediately after being opened"));
        assert_eq!(
            FileIdentity::of(path),
            Some(identity),
            "skuld: coordination DB {path:?} disagreed with the connection just opened through \
             it — something retargeted it in the open-to-record window"
        );
        identity
    }
}

/// `-wal`/`-shm` companions' identities, unix-only — see [`DbIdentity`]'s
/// doc for why Windows needs neither.
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct CompanionIdentities {
    wal: FileIdentity,
    shm: FileIdentity,
}

/// `path` with `suffix` appended verbatim to the filename (not
/// [`std::path::Path::with_extension`], which would replace `.skuld.db`'s
/// existing `db` extension instead of appending) — `-wal`/`-shm` name
/// their main file's `-wal`/`-shm` companions exactly this way.
#[cfg(unix)]
fn companion_path(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    std::path::PathBuf::from(name)
}

/// Record `path`'s `-wal`/`-shm` companions' identities, called by
/// [`open_db`] right after [`ensure_schema_locked`] completes — not
/// before, and not merely from opening the connection: a database whose
/// only prior connection already closed has *neither* file (SQLite deletes
/// both when the last connection to a database closes —
/// `crate::probe::probe_coordination_connect`'s own doc already relies on
/// this), and gets them back only once something re-touches WAL mode.
/// `ensure_schema_locked`'s `INIT_SQL` runs `PRAGMA journal_mode = WAL`
/// unconditionally, every `open_db` call, so by the time it returns both
/// companions are guaranteed to exist — verified empirically (a fresh
/// connection re-running that exact idempotent PRAGMA, even against a
/// database whose table already existed, recreated both files) — so their
/// absence here is a broken precondition, not a normal state to tolerate,
/// and panics.
///
/// Stability once recorded — verified empirically, on both Linux and
/// macOS, before relying on it — is what makes this the same one-time
/// recording that [`record_identity`] does for the main file, not
/// something that needs re-establishing on every check: `-wal`/`-shm` keep
/// their inode for as long as *any* connection (this one included) keeps
/// the database open, across checkpoints of every mode (`PASSIVE`, `FULL`,
/// `RESTART`, `TRUNCATE` — `TRUNCATE` resets the file's *size*, not its
/// identity), `journal_size_limit`, thousands of ordinary
/// one-transaction-per-commit writes (this crate's own usage pattern), and
/// a second, independent connection concurrently writing and checkpointing
/// the same database. SQLite truncates and rewrites both files in place;
/// it does not unlink and recreate them while any connection holds the
/// database open. No case was found where it legitimately does otherwise —
/// if one is ever found, that is a reason to revisit this function, not a
/// case to paper over here.
#[cfg(unix)]
fn record_companions(path: &std::path::Path) -> CompanionIdentities {
    let wal = FileIdentity::of(&companion_path(path, "-wal")).unwrap_or_else(|| {
        panic!(
            "skuld: coordination DB {path:?}'s -wal companion is missing right after schema \
             init, where PRAGMA journal_mode=WAL having just run unconditionally should \
             guarantee it exists"
        )
    });
    let shm = FileIdentity::of(&companion_path(path, "-shm")).unwrap_or_else(|| {
        panic!(
            "skuld: coordination DB {path:?}'s -shm companion is missing right after schema \
             init, where PRAGMA journal_mode=WAL having just run unconditionally should \
             guarantee it exists"
        )
    });
    CompanionIdentities { wal, shm }
}

/// Everything [`open_db`] records to keep checking a connection's identity
/// against for the rest of its life: the main file (see
/// [`record_identity`]) plus, on Unix, its `-wal`/`-shm` companions (see
/// [`record_companions`]) — deleting only a companion leaves the main
/// file's own identity untouched, so [`FileIdentity`] alone has nothing to
/// catch for that case.
///
/// No companion tracking on Windows: SQLite's Windows VFS opens every file
/// it touches — `-wal`/`-shm` included, through the same `winOpen` — with
/// the same withheld `FILE_SHARE_DELETE` [`db_has_moved`]'s own doc
/// explains for the main file, so nothing on Windows can delete or rename
/// either companion out from under a connection that holds the database
/// open either. An ancestor retarget affecting them is already covered
/// transitively: it changes what the main file's own path resolves to
/// too, which the main file's own (real, on Windows too) [`FileIdentity`]
/// check catches on its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct DbIdentity {
    main: FileIdentity,
    #[cfg(unix)]
    companions: CompanionIdentities,
}

/// [`db_has_moved`], extended to also check `identity`'s `-wal`/`-shm`
/// companions (see [`DbIdentity`]) — call this (via
/// [`panic_on_moved_db_full`]) everywhere past [`open_db`]'s own schema
/// init, where companions are established: [`coordinate`]'s loop,
/// [`TestRegistration`]'s cleanup. `ensure_schema_locked`,
/// `migrate_schema`, and `scrub_serial_filters_v1` keep using
/// [`db_has_moved`] directly, with just the main file's [`FileIdentity`]:
/// companions aren't established yet during schema init (see
/// [`record_companions`]'s doc), and there is nothing companion-related to
/// protect during that phase.
fn db_or_companions_have_moved(conn: &rusqlite::Connection, path: &std::path::Path, identity: &DbIdentity) -> bool {
    if db_has_moved(conn, path, identity.main) {
        return true;
    }
    #[cfg(unix)]
    {
        FileIdentity::of(&companion_path(path, "-wal")) != Some(identity.companions.wal)
            || FileIdentity::of(&companion_path(path, "-shm")) != Some(identity.companions.shm)
    }
    #[cfg(windows)]
    false
}

/// Panic loudly, naming `path`, if `conn`'s database has moved (see
/// [`db_has_moved`]) — call this immediately before any write through a
/// connection [`open_db`]'s own schema initialization and migration has
/// held open across more than one operation, so a `.skuld.db` deleted or
/// replaced mid-run is caught here instead of corrupting whatever now
/// exists at `path`. Main file only — see [`panic_on_moved_db_full`] for
/// the companion-aware version used past schema init.
fn panic_on_moved_db(conn: &rusqlite::Connection, path: &std::path::Path, recorded: FileIdentity) {
    if db_has_moved(conn, path, recorded) {
        panic!("skuld coordination DB {path:?} was deleted or replaced mid-run");
    }
}

/// [`panic_on_moved_db`], extended to also check `identity`'s `-wal`/`-shm`
/// companions (see [`db_or_companions_have_moved`]) — call this
/// everywhere past [`open_db`]'s own schema init: [`coordinate`]'s loop,
/// [`TestRegistration`]'s cleanup.
fn panic_on_moved_db_full(conn: &rusqlite::Connection, path: &std::path::Path, identity: &DbIdentity) {
    if db_or_companions_have_moved(conn, path, identity) {
        panic!("skuld coordination DB {path:?} was deleted or replaced mid-run");
    }
}

/// True for SQLite's broad "system I/O failed" family (`SQLITE_IOERR` and
/// every extended variant, e.g. `SQLITE_IOERR_SHORT_READ` — rusqlite
/// collapses all of them onto the one primary-code variant this matches).
/// A write against a connection whose `-wal`/`-shm` companion vanished out
/// from under it — deleted without touching the main file's own identity,
/// so [`db_has_moved`] alone has nothing to catch — tends to surface
/// exactly this way instead: SQLite discovers the missing companion
/// mid-operation and reports it as an opaque I/O failure, not as "moved."
/// But *any* I/O failure takes this same opaque shape, `ENOSPC` on a full
/// filesystem included — this alone is not evidence of a move, only a
/// reason to check (see [`moved_db_message_for`]/[`moved_db_message_for_full`]).
fn is_io_error(err: &rusqlite::Error) -> bool {
    matches!(err.sqlite_error_code(), Some(rusqlite::ErrorCode::SystemIoFailure))
}

/// The message to panic with for an I/O-class error (see [`is_io_error`])
/// that isn't a confirmed move: `err` reported as-is, naming `path`,
/// SQLite's own extended error code, and the OS errno behind it
/// (`sqlite3_system_errno`) — deliberately *not* the "moved" message,
/// since nothing here confirmed that.
fn io_error_message(conn: &rusqlite::Connection, err: &rusqlite::Error, path: &std::path::Path) -> String {
    let extended = err.sqlite_extended_error_code();
    // Safety: `conn.handle()` is a valid, currently-open `sqlite3*` for as
    // long as `conn` is borrowed.
    let system_errno = unsafe { rusqlite::ffi::sqlite3_system_errno(conn.handle()) };
    format!(
        "skuld: coordination DB I/O error at {path:?}: {err} (extended code: {extended:?}, \
         system errno: {system_errno})"
    )
}

/// If `err` is I/O-class (see [`is_io_error`]), the message to panic with —
/// re-checking, at this exact moment, whether the DB has actually moved
/// (main file only; call this during schema init, before companions are
/// established — see [`moved_db_message_for_full`] for the companion-aware
/// version used past that) and picking accordingly: the same clear "was
/// deleted or replaced mid-run" message every other moved-DB detection
/// uses if [`db_has_moved`] confirms it right now, or [`io_error_message`]
/// if not — an I/O error with no such confirmation must not be mislabelled
/// as a move. `None` for every non-I/O-class error: this is deliberately
/// narrow, not a blanket reinterpretation of arbitrary SQLite failures.
fn moved_db_message_for(
    conn: &rusqlite::Connection,
    err: &rusqlite::Error,
    path: &std::path::Path,
    recorded: FileIdentity,
) -> Option<String> {
    if !is_io_error(err) {
        return None;
    }
    Some(if db_has_moved(conn, path, recorded) {
        format!("skuld coordination DB {path:?} was deleted or replaced mid-run: {err}")
    } else {
        io_error_message(conn, err, path)
    })
}

/// [`moved_db_message_for`], extended to also check `identity`'s
/// `-wal`/`-shm` companions (see [`db_or_companions_have_moved`]) — call
/// this everywhere past [`open_db`]'s own schema init.
fn moved_db_message_for_full(
    conn: &rusqlite::Connection,
    err: &rusqlite::Error,
    path: &std::path::Path,
    identity: &DbIdentity,
) -> Option<String> {
    if !is_io_error(err) {
        return None;
    }
    Some(if db_or_companions_have_moved(conn, path, identity) {
        format!("skuld coordination DB {path:?} was deleted or replaced mid-run: {err}")
    } else {
        io_error_message(conn, err, path)
    })
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
                signal_test_retry_hook();
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

/// Signal [`retry_busy`]'s (or [`coordinate`]'s own retry arm's)
/// thread-scoped test hook, if the calling thread has activated one via
/// [`set_test_retry_hook`]. A no-op everywhere else. See [`TEST_RETRY_HOOK`]'s
/// doc for why this is thread-scoped rather than a process-wide counter.
#[cfg(test)]
fn signal_test_retry_hook() {
    TEST_RETRY_HOOK.with(|c| {
        if let Some(tx) = c.borrow().as_ref() {
            // Unbounded channel: never blocks. Dropped receiver (test already
            // gave up / moved on) just means the send is discarded.
            let _ = tx.send(());
        }
    });
}

// Test-only, *thread-scoped* retry hook: not a process-wide counter like
// `lock::EINTR_RETRIES`, because `retry_busy` (and `coordinate`'s own retry
// arm) isn't only reachable from the one call a test deliberately drives
// into contention — `retry_busy` also runs inside every `TestRegistration`'s
// cleanup on drop, for every test in this binary, and libtest runs those
// concurrently on their own threads by default. A process-wide signal would
// let an unrelated, concurrently-running test's own incidental contention
// wake this one up, so a test waiting on it could stop and release its own
// held lock before its *own* call under test ever actually retried — passing
// without ever exercising what it claims to. A thread-local avoids that:
// only the thread that calls `set_test_retry_hook` receives anything on the
// channel it was given, so a test's own dedicated thread (spawned solely to
// make the one call under test) can never observe another test's unrelated
// retries, no matter how many other tests are running concurrently in the
// same process.
//
// An `mpsc::Sender`, not a counter: a test blocks on the paired `Receiver`'s
// `recv()` — no spin loop, no CPU burned waiting — and `recv()` itself
// becomes the failure signal, not just the success one: it returns `Err`
// the moment every `Sender` (here, the one moved whole into the worker
// thread's closure via `set_test_retry_hook`, never cloned) is dropped
// without ever sending. That happens on ordinary return from the closure,
// and on a panic too: `std::thread::spawn`'s own wrapper catches the
// unwinding panic (to convert it into the `Err` a `JoinHandle::join()`
// reports) before the thread actually exits, and thread-local destructors —
// this `Sender` included — run as part of that exit, strictly after the
// catch, not "during" the unwind itself.
//
// That only covers the worker *exiting* without ever retrying: `recv()`
// fails outright if the worker exits without retrying, but a worker that's
// merely blocked — stuck retrying forever against a condition that never
// resolves, or hung on something unrelated, without ever exiting — holds
// `tx` open the whole time, and `recv()` waits right along with it. A test
// hung that way has no bound from this mechanism; the CI job's own runner
// timeout is the only backstop left, same as it always was for a thread
// that simply never finishes.
#[cfg(test)]
thread_local! {
    static TEST_RETRY_HOOK: std::cell::RefCell<Option<std::sync::mpsc::Sender<()>>> = const { std::cell::RefCell::new(None) };
}

/// Activate `tx` as [`retry_busy`]'s (and [`coordinate`]'s) retry signal for
/// the *calling thread only* — see [`TEST_RETRY_HOOK`]'s doc. Meant to be the
/// first thing a freshly spawned, single-purpose test thread does, before
/// making the one call it exists to drive into contention: such a thread is
/// always discarded (joined, never reused for anything else) once that call
/// returns, so there is nothing to deactivate afterward — the thread exiting
/// drops `tx` on its own. Panics (via `debug_assert!`) if called twice on the
/// same thread without an intervening thread exit: every real test spawns a
/// fresh, single-purpose thread for this, so a non-empty slot here means a
/// test is reusing a thread or activating two hooks at once — a test bug
/// this exists to catch, not a scenario to silently overwrite.
#[cfg(test)]
pub(crate) fn set_test_retry_hook(tx: std::sync::mpsc::Sender<()>) {
    TEST_RETRY_HOOK.with(|c| {
        let mut slot = c.borrow_mut();
        debug_assert!(
            slot.is_none(),
            "set_test_retry_hook: called twice on the same thread without an intervening exit"
        );
        *slot = Some(tx);
    });
}

// Test-only, thread-scoped seam for the one window no contention can open
// deterministically: between a write's success and the post-write moved-DB
// check that follows it. Same discipline as `TEST_RETRY_HOOK` — a
// single-purpose test thread installs it, and only that thread ever runs it.
#[cfg(test)]
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AfterWriteSite {
    /// `coordinate`, right after its COMMIT succeeded.
    Coordinate,
    /// `TestRegistration::drop`, right after its DELETE succeeded.
    Drop,
}

#[cfg(test)]
type AfterWriteHook = (AfterWriteSite, Box<dyn FnOnce()>);

#[cfg(test)]
thread_local! {
    static TEST_AFTER_WRITE_HOOK: std::cell::RefCell<Option<AfterWriteHook>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` on the calling thread once, at the next `site` write.
#[cfg(test)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn set_test_after_write_hook(site: AfterWriteSite, f: impl FnOnce() + 'static) {
    TEST_AFTER_WRITE_HOOK.with(|c| {
        let mut slot = c.borrow_mut();
        debug_assert!(slot.is_none(), "set_test_after_write_hook: already set on this thread");
        *slot = Some((site, Box::new(f)));
    });
}

#[cfg(test)]
fn run_test_after_write_hook(site: AfterWriteSite) {
    let hook = TEST_AFTER_WRITE_HOOK.with(|c| {
        let mut slot = c.borrow_mut();
        match slot.take() {
            Some((s, f)) if s == site => Some(f),
            other => {
                *slot = other;
                None
            }
        }
    });
    if let Some(f) = hook {
        f();
    }
}

// Schema migration =====

/// Run all pending schema migrations. Gated by `PRAGMA user_version` and
/// performed inside `BEGIN IMMEDIATE` so concurrent test binaries don't race.
/// Once a migration completes, the version pragma is bumped and subsequent
/// connections skip the work.
///
/// `identity` — see [`ensure_schema_locked`]'s doc, its only caller, for
/// where this comes from and why every write step below checks it (via
/// [`panic_on_moved_db`]) immediately before running, not just once on
/// entry: this function's two `retry_busy` calls can each retry for an
/// uncapped amount of time, and every other write here — the scrub, the
/// version bump, the commit — still runs after them, so a move landing at
/// any point along the way must still be caught before the next write
/// trusts a connection that's no longer pointed at `path`.
fn migrate_schema(conn: &rusqlite::Connection, path: &std::path::Path, identity: FileIdentity) {
    // The read below is wrapped in `retry_busy` not because a concurrent
    // `BEGIN EXCLUSIVE` elsewhere would block it — in WAL mode a plain read
    // like this one doesn't contend with another connection's write lock at
    // all, exclusive or not — but because `SQLITE_BUSY_RECOVERY` can still
    // surface here: it's reported to a connection that has to *wait* because
    // some *other* connection is the one currently running WAL recovery (the
    // hot-journal cleanup after that other connection's own unclean exit) —
    // this connection is the one blocked, not the one recovering. `.unwrap_or(0)`
    // is unchanged and only reached once a genuinely non-retryable error
    // comes back. (Nothing in this crate's own test suite reproduces
    // `SQLITE_BUSY_RECOVERY` — it needs a genuinely killed process leaving a
    // hot WAL and a second connection racing the recovery, not a live
    // contending connection alone — so this path relies on code review
    // rather than a regression test.)
    let current: i64 = retry_busy(conn, || {
        panic_on_moved_db(conn, path, identity);
        conn.query_row("PRAGMA user_version", [], |row| row.get(0))
    })
    .unwrap_or(0);
    panic_on_moved_db(conn, path, identity);
    if current >= SCHEMA_VERSION {
        return;
    }
    // Take an immediate write lock so two processes don't both start
    // scrubbing. Unlike the read above, this genuinely does contend with a
    // concurrent `BEGIN EXCLUSIVE`/`BEGIN IMMEDIATE` elsewhere (any live
    // `coordinate` caller, which never takes this connection's own init
    // lock) — `retry_busy` retries that, uncapped, instead of relying on
    // rusqlite's default `busy_timeout` (disabled in `connect_locked`) to
    // paper over it with a capped internal wait.
    if let Err(e) = retry_busy(conn, || {
        panic_on_moved_db(conn, path, identity);
        conn.execute_batch("BEGIN IMMEDIATE")
    }) {
        if let Some(msg) = moved_db_message_for(conn, &e, path, identity) {
            panic!("{msg}");
        }
        eprintln!("[skuld] warning: failed to acquire migration lock: {e}");
        return;
    }
    panic_on_moved_db(conn, path, identity);
    // Re-check inside the transaction in case another process beat us to it.
    let inside_tx: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap_or(0);
    if inside_tx >= SCHEMA_VERSION {
        let _ = conn.execute_batch("COMMIT");
        return;
    }
    if current < 1 {
        panic_on_moved_db(conn, path, identity);
        scrub_serial_filters_v1(conn, path, identity);
    }
    panic_on_moved_db(conn, path, identity);
    if let Err(e) = conn.execute(&format!("PRAGMA user_version = {SCHEMA_VERSION}"), []) {
        eprintln!("[skuld] warning: failed to bump schema version: {e}");
    }
    panic_on_moved_db(conn, path, identity);
    if let Err(e) = conn.execute_batch("COMMIT") {
        eprintln!("[skuld] warning: failed to commit schema migration: {e}");
    }
    panic_on_moved_db(conn, path, identity);
}

/// Migration v0 → v1: rewrite every `serial_filter` to its canonical Display
/// form, collapsing `Const(true)` and `Const(false)` to the `*` and `""`
/// sentinels respectively. Rows that fail to parse are LEFT ALONE if their
/// owning instance is alive (touching them might break a running test's
/// serialization invariants); only the standard `clean_stale_entries` path
/// removes them later.
///
/// `identity` — see [`migrate_schema`]'s doc: checked (via
/// [`panic_on_moved_db`]) before each row's `UPDATE`, since this can loop
/// over an arbitrary number of rows and a move landing partway through must
/// still be caught before the next one trusts a connection that's no
/// longer pointed at `path`.
fn scrub_serial_filters_v1(conn: &rusqlite::Connection, path: &std::path::Path, identity: FileIdentity) {
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
                    panic_on_moved_db(conn, path, identity);
                    if let Err(e) = conn.execute(
                        "UPDATE running SET serial_filter = ?1 WHERE id = ?2",
                        rusqlite::params![canonical, id],
                    ) {
                        if let Some(msg) = moved_db_message_for(conn, &e, path, identity) {
                            panic!("{msg}");
                        }
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
/// [`panic_on_moved_db_full`] before every `DELETE` attempt — inside the
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
            retry_busy(&self.conn, || {
                panic_on_moved_db_full(&self.conn, &self.path, &self.identity);
                self.conn.execute("DELETE FROM running WHERE id = ?1", [self.id])
            })
            .unwrap_or_else(|e| {
                if let Some(msg) = moved_db_message_for_full(&self.conn, &e, &self.path, &self.identity) {
                    panic!("{msg}");
                }
                panic!(
                    "skuld: failed to unregister test from coordination DB at {:?}: {e}",
                    self.path
                )
            });
            #[cfg(test)]
            run_test_after_write_hook(AfterWriteSite::Drop);
            panic_on_moved_db_full(&self.conn, &self.path, &self.identity);
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
        panic_on_moved_db_full(&conn, db_path, &identity);

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
                // A move landing between the check at the top of this loop
                // and the COMMIT above must still end loud: otherwise this
                // registration lands in an orphaned file, unserialized
                // against anything real, and the only place that would
                // ever surface it is this connection's own eventual Drop —
                // as a downgraded warning, not a panic, if the test itself
                // already panicked by then (see `TestRegistration::drop`'s
                // own doc). Catching it here, before any of that, is what
                // makes it a loud failure at the point it happened instead.
                #[cfg(test)]
                run_test_after_write_hook(AfterWriteSite::Coordinate);
                panic_on_moved_db_full(&conn, db_path, &identity);
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
                signal_test_retry_hook();
                skuld_debug_eprintln!(
                    "coordination: {name} is retrying a transient busy error against {:?} — \
                     uncapped, so if this never resolves, something is holding a lock on that \
                     database indefinitely",
                    conn.path().unwrap_or("<unknown>")
                );
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                // See `is_retryable`'s doc: `SQLITE_LOCKED` here can only be
                // a same-connection self-conflict, a bug in this crate's own
                // code, not a condition worth having retried past.
                debug_assert!(
                    e.sqlite_error_code() != Some(rusqlite::ErrorCode::DatabaseLocked),
                    "coordinate: got SQLITE_LOCKED against {:?}: {e}",
                    conn.path().unwrap_or("<unknown>")
                );
                // A missing `-wal`/`-shm` companion (main file identity
                // untouched, so the check above never caught it) tends to
                // surface as exactly this shape of opaque I/O error —
                // re-checked here at this exact moment (including the
                // companions) rather than assumed from the error's shape
                // alone, so a genuine I/O error (e.g. `ENOSPC`) still
                // reports as what it actually is.
                if let Some(msg) = moved_db_message_for_full(&conn, &e, db_path, &identity) {
                    panic!("{msg}");
                }
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
