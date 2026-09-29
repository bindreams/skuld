//! Tests that `migrate_schema` and the scrub turn every failure into a panic
//! naming the database, and never swallow one.
//!
//! Failures are injected with SQLite's authorizer, which denies a chosen
//! statement at prepare time: deterministic, no timing, no filesystem tricks.

use super::coordination_tests::temp_db;
use super::moved_db::DbIdentity;
use super::{migrate_schema, open_db, register, SCHEMA_VERSION};
use std::ffi::{c_char, c_int, c_void, CStr};

/// What the authorizer is asked about one statement.
struct Action<'a> {
    code: c_int,
    arg1: Option<&'a str>,
    arg2: Option<&'a str>,
}

type Deny<'a> = Box<dyn Fn(&Action) -> bool + 'a>;

/// What the authorizer callback needs: the closure, and a slot for its panic.
struct AuthorizerState<'c> {
    deny: Deny<'c>,
    panic: std::cell::RefCell<Option<Box<dyn std::any::Any + Send>>>,
}

/// Denies every statement `deny` matches on `conn` until dropped. A panic in
/// `deny` is caught (unwinding out of the `extern "C"` callback would abort the
/// whole test binary), denies the statement, and is re-raised when the
/// `Authorizer` drops.
struct Authorizer<'c> {
    conn: &'c rusqlite::Connection,
    // Boxed so the pointer handed to SQLite stays put.
    state: Box<AuthorizerState<'c>>,
}

unsafe extern "C" fn authorize(
    user: *mut c_void,
    code: c_int,
    arg1: *const c_char,
    arg2: *const c_char,
    _db: *const c_char,
    _trigger: *const c_char,
) -> c_int {
    // Safety: `user` is the `Box<AuthorizerState>` owned by the live `Authorizer`.
    let state = unsafe { &*(user as *const AuthorizerState) };
    let denied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let text = |p: *const c_char| {
            // Safety: SQLite passes NUL-terminated strings or NULL.
            (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_str().unwrap())
        };
        let action = Action {
            code,
            arg1: text(arg1),
            arg2: text(arg2),
        };
        (state.deny)(&action)
    }));
    match denied {
        Ok(false) => rusqlite::ffi::SQLITE_OK,
        Ok(true) => rusqlite::ffi::SQLITE_DENY,
        Err(payload) => {
            // Keep the first panic; a later one is a consequence of it.
            state.panic.borrow_mut().get_or_insert(payload);
            rusqlite::ffi::SQLITE_DENY
        }
    }
}

impl<'c> Authorizer<'c> {
    fn install(conn: &'c rusqlite::Connection, deny: impl Fn(&Action) -> bool + 'c) -> Self {
        let state = Box::new(AuthorizerState {
            deny: Box::new(deny),
            panic: std::cell::RefCell::new(None),
        });
        // Safety: `conn.handle()` is valid while `conn` is borrowed; the boxed
        // state outlives the registration because `Drop` removes it first.
        let rc = unsafe {
            rusqlite::ffi::sqlite3_set_authorizer(
                conn.handle(),
                Some(authorize),
                (&*state as *const AuthorizerState).cast_mut().cast(),
            )
        };
        assert_eq!(rc, rusqlite::ffi::SQLITE_OK);
        Self { conn, state }
    }
}

impl Drop for Authorizer<'_> {
    fn drop(&mut self) {
        // Safety: as in `install`.
        unsafe { rusqlite::ffi::sqlite3_set_authorizer(self.conn.handle(), None, std::ptr::null_mut()) };
        if !std::thread::panicking() {
            if let Some(payload) = self.state.panic.borrow_mut().take() {
                std::panic::resume_unwind(payload);
            }
        }
    }
}

const SQLITE_PRAGMA: c_int = 19;
const SQLITE_TRANSACTION: c_int = 22;
const SQLITE_UPDATE: c_int = 23;
const SQLITE_READ: c_int = 20;

fn reads_user_version(a: &Action) -> bool {
    a.code == SQLITE_PRAGMA && a.arg1 == Some("user_version") && a.arg2.is_none()
}

fn sets_user_version(a: &Action) -> bool {
    a.code == SQLITE_PRAGMA && a.arg1 == Some("user_version") && a.arg2.is_some()
}

fn transaction(a: &Action, verb: &str) -> bool {
    a.code == SQLITE_TRANSACTION && a.arg1 == Some(verb)
}

/// A database at schema version 0 with one legacy row the scrub rewrites.
fn v0_db() -> (tempfile::TempDir, std::path::PathBuf, rusqlite::Connection, DbIdentity) {
    let (dir, path) = temp_db();
    let (conn, identity) = open_db(&path);
    conn.execute("PRAGMA user_version = 0", []).unwrap();
    register(&conn, "legacy", &[], "(a) | (a)").unwrap();
    (dir, path, conn, identity)
}

/// `migrate_schema` panicked with a message naming `path` and containing `what`.
fn assert_panics_naming(
    path: &std::path::Path,
    what: &str,
    conn: &rusqlite::Connection,
    identity: &DbIdentity,
) -> String {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| migrate_schema(conn, path, identity)));
    let payload = result.expect_err("migrate_schema must panic instead of swallowing the failure");
    let msg = crate::coordination::panic_payload_message(payload.as_ref()).to_owned();
    assert!(
        msg.contains(&format!("{path:?}")) && msg.contains(what),
        "the panic must name {path:?} and contain {what:?}: {msg:?}"
    );
    msg
}

#[test]
fn migrate_schema_panics_when_reading_the_schema_version_fails() {
    let (_dir, path, conn, identity) = v0_db();
    let first = std::cell::Cell::new(true);
    let _deny = Authorizer::install(&conn, |a| reads_user_version(a) && first.replace(false));

    assert_panics_naming(&path, "schema version", &conn, &identity);
}

#[test]
fn migrate_schema_panics_when_the_migration_lock_cannot_be_taken() {
    let (_dir, path, conn, identity) = v0_db();
    let _deny = Authorizer::install(&conn, |a| transaction(a, "BEGIN"));

    assert_panics_naming(&path, "migration lock", &conn, &identity);
}

#[test]
fn migrate_schema_panics_when_the_version_recheck_inside_the_transaction_fails() {
    let (_dir, path, conn, identity) = v0_db();
    let reads = std::cell::Cell::new(0);
    let _deny = Authorizer::install(&conn, |a| {
        reads_user_version(a) && {
            reads.set(reads.get() + 1);
            reads.get() == 2
        }
    });

    assert_panics_naming(&path, "schema version", &conn, &identity);
}

/// Another process finished the migration between the first read and this
/// one's `BEGIN`: the early return commits an empty transaction, and a failure
/// of that COMMIT must not be discarded.
#[test]
fn migrate_schema_panics_when_the_early_return_commit_fails() {
    let (_dir, path, conn, identity) = v0_db();
    // A plain connection: `open_db` would run the migration itself.
    let other = rusqlite::Connection::open(&path).unwrap();
    let _deny = Authorizer::install(&conn, |a| {
        if transaction(a, "BEGIN") {
            other
                .execute("PRAGMA user_version = 1", [])
                .expect("the other connection finishes the migration");
        }
        transaction(a, "COMMIT")
    });

    assert_panics_naming(&path, "commit", &conn, &identity);
    assert!(
        conn.is_autocommit(),
        "a failed COMMIT must not leave the connection inside its transaction"
    );
}

#[test]
fn migrate_schema_panics_when_the_scrub_cannot_read_the_running_table() {
    let (_dir, path, conn, identity) = v0_db();
    let _deny = Authorizer::install(&conn, |a| a.code == SQLITE_READ && a.arg1 == Some("running"));

    assert_panics_naming(&path, "scrub", &conn, &identity);
}

#[test]
fn migrate_schema_panics_when_a_scrub_row_cannot_be_read() {
    let (_dir, path, conn, identity) = v0_db();
    // A BLOB where the scrub reads TEXT: reading the row fails.
    conn.execute(
        "INSERT INTO running (instance_id, name, serial_filter) VALUES (?1, 'blob', 'x | y')",
        [rusqlite::types::Value::Blob(vec![0xff, 0x00])],
    )
    .unwrap();

    assert_panics_naming(&path, "scrub", &conn, &identity);
}

#[test]
fn migrate_schema_panics_when_a_scrub_update_fails() {
    let (_dir, path, conn, identity) = v0_db();
    let _deny = Authorizer::install(&conn, |a| a.code == SQLITE_UPDATE && a.arg1 == Some("running"));

    assert_panics_naming(&path, "scrub", &conn, &identity);
}

#[test]
fn migrate_schema_panics_when_the_version_bump_fails() {
    let (_dir, path, conn, identity) = v0_db();
    let _deny = Authorizer::install(&conn, sets_user_version);

    assert_panics_naming(&path, "schema version", &conn, &identity);
}

#[test]
fn migrate_schema_panics_when_the_final_commit_fails() {
    let (_dir, path, conn, identity) = v0_db();
    let _deny = Authorizer::install(&conn, |a| transaction(a, "COMMIT"));

    assert_panics_naming(&path, "commit", &conn, &identity);
    assert!(
        conn.is_autocommit(),
        "a failed COMMIT must not leave the connection inside its transaction"
    );
}

/// Sanity: the harness above does not itself make a healthy migration fail.
#[test]
fn migrate_schema_completes_when_nothing_is_denied() {
    let (_dir, path, conn, identity) = v0_db();
    let _deny = Authorizer::install(&conn, |_| false);

    migrate_schema(&conn, &path, &identity);

    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

/// A deny closure that panics must fail its own test, not abort the process:
/// unwinding out of an `extern "C"` callback aborts. The panic is denied at the
/// SQLite level and re-raised on the test thread.
#[test]
fn a_panicking_deny_closure_is_reraised_on_the_test_thread() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _deny = Authorizer::install(&conn, |_| panic!("deny closure blew up"));
        let _ = conn.execute_batch("SELECT 1");
    }));

    let payload = result.expect_err("the deny closure's panic must reach the test thread");
    assert_eq!(
        crate::coordination::panic_payload_message(payload.as_ref()),
        "deny closure blew up"
    );
}
