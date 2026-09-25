//! Unified test harness with runtime preconditions and unavailability reporting.
//!
//! Provides `#[skuld::test]` for annotating test functions. Tests can declare
//! runtime preconditions (e.g. "valgrind must be installed"), fixture injection,
//! custom display names, and labels for filtering. Tests whose preconditions are
//! not met show as `ignored` with an unavailability summary after all tests run.
//!
//! For dynamic test generation (e.g. from data files), use [`TestRunner::add`]
//! to register tests at runtime alongside attribute-registered ones.
//!
//! See the [README](../README.md) for usage instructions.

extern crate self as skuld;

mod capture;
pub(crate) mod coordination;
pub mod fixture;
pub mod fixtures;
pub mod label;
pub mod metadata;
pub mod runner;
#[cfg(test)]
mod runner_tests;

pub use coordination::{SERIAL_ALL, SERIAL_NONE};
pub use fixture::{
    cleanup_process_fixtures, collect_fixture_requires, collect_fixture_serial, enter_test_scope, fixture, fixture_get,
    fixture_registry, merge_serial_filters, warm_up, FixtureDef, FixtureHandle, FixtureRef, FixtureScope, TestScope,
};
pub use fixtures::cwd::{cwd, CwdGuard};
pub use fixtures::env::{env, EnvGuard};
pub use fixtures::temp_dir::{temp_dir, TempDir};
pub use fixtures::test_name::{test_name, TestName};
pub use label::{Label, LabelEntry, LabelFilter, ModuleLabels};
pub use metadata::{FixtureMetadata, RequirementInfo, TestMetadata};
pub use runner::{run_all, NextestTestMetadata, TestRunner};

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::OnceLock;

// Re-export proc macros for consumers.
pub use skuld_macros::fixture;
pub use skuld_macros::label;
pub use skuld_macros::test;

// Re-export inventory so that macro-generated `inventory::submit!` calls resolve.
pub use inventory;

/// A named precondition check. Carries both a human-readable name and the
/// check function itself so that metadata can be serialized without losing
/// identity.
pub struct Requirement {
    pub name: &'static str,
    pub check: fn() -> Result<(), String>,
}

impl Requirement {
    /// Evaluate the requirement, returning `Ok(())` or `Err(reason)`.
    pub fn eval(&self) -> Result<(), String> {
        (self.check)()
    }
}

/// Whether a test expects a panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShouldPanic {
    No,
    Yes,
    WithMessage(&'static str),
}

// Test context ========================================================================================

/// Metadata about the currently executing test, set by [`enter_test_scope`].
#[derive(Clone, Copy)]
pub struct CurrentTest {
    pub name: &'static str,
    pub module_path: &'static str,
}

thread_local! {
    pub(crate) static CURRENT_TEST: Cell<Option<CurrentTest>> = const { Cell::new(None) };
}

/// Get the current test context. Panics if called outside a test body.
pub fn current_test() -> CurrentTest {
    CURRENT_TEST.get().expect("called outside of a test body")
}

/// Lazily-built index from `(name, module)` to [`TestDef`]. O(1) lookup for
/// metadata construction.
pub fn test_registry() -> &'static HashMap<(&'static str, &'static str), &'static TestDef> {
    static REGISTRY: OnceLock<HashMap<(&str, &str), &TestDef>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        inventory::iter::<TestDef>()
            .map(|def| ((def.name, def.module), def))
            .collect()
    })
}

// Ignore ==============================================================================================

/// Whether a test is statically ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ignore {
    No,
    Yes,
    WithReason(&'static str),
}

// Test definition =====================================================================================

/// A test registered by `#[skuld::test(...)]` via inventory.
pub struct TestDef {
    pub name: &'static str,
    /// Module path (from `module_path!()`) for matching against `default_labels!`.
    pub module: &'static str,
    /// Display name (custom name). `None` → use `name`.
    pub display_name: Option<&'static str>,
    pub requires: &'static [Requirement],
    /// Names of fixtures used by this test (from `#[fixture]` params).
    /// Used for transitive requirement collection via [`collect_fixture_requires`].
    pub fixture_names: &'static [&'static str],
    pub ignore: Ignore,
    /// Labels for filtering.
    pub labels: &'static [Label],
    /// Whether `labels = [...]` was explicitly written (even if empty).
    /// When false, module-level defaults from `default_labels!` apply.
    pub labels_explicit: bool,
    /// Serial filter expression for this test.
    /// Empty string means non-serial; `"*"` means serial with everything;
    /// a label expression means serial only with tests matching that filter.
    /// Propagated transitively from fixtures via [`collect_fixture_serial`].
    pub serial: &'static str,
    pub should_panic: ShouldPanic,
    pub body: fn(),
}

inventory::collect!(TestDef);

// Private helpers for macro-generated code ============================================================

#[doc(hidden)]
pub mod __private {
    /// Build a single-threaded tokio runtime for async test execution.
    ///
    /// This is a separate function (rather than combined with `block_on`) so that
    /// `should_panic` tests can construct the runtime *outside* their `catch_unwind`
    /// boundary. A runtime build failure is an infrastructure error, not a test panic.
    #[cfg(feature = "tokio")]
    pub fn build_async_runtime() -> ::tokio::runtime::Runtime {
        ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime for async test")
    }

    /// Trait for converting test return values into `()`.
    ///
    /// `()` passes through; `Result<(), E>` panics on `Err`. The proc macro wraps
    /// every test function call in `IntoTestResult::into_test_result(...)` so that
    /// returning `Err` from a test is a failure, not a silent pass.
    pub trait IntoTestResult {
        fn into_test_result(self);
    }

    impl IntoTestResult for () {
        fn into_test_result(self) {}
    }

    impl<E: std::fmt::Debug> IntoTestResult for Result<(), E> {
        fn into_test_result(self) {
            self.unwrap_or_else(|e| panic!("test returned an error: {e:?}"));
        }
    }

    /// Probe hook for Skuld's own test suite: open a coordination DB
    /// connection at `path`, running the Unix atomic-publish step and the
    /// real schema-init path (`open_db`, not just `connect`),
    /// and return it instead of dropping it. Going through `open_db` — not
    /// `connect` alone — matters here: `PRAGMA journal_mode = WAL` is what
    /// makes SQLite create the `-wal`/`-shm` companions in the first place,
    /// and this hook exists to measure the mode those companions come out
    /// at. `umask` is process-global, so testing "publish creates 0666
    /// files despite a restrictive umask" safely needs a genuine subprocess
    /// rather than mutating umask in-process, where it would corrupt
    /// concurrently running unit tests. Support binaries under
    /// `tests/support_bins/` link against Skuld's public API only, hence
    /// this hook.
    ///
    /// Returning the live connection (rather than dropping it here) matters:
    /// SQLite deletes `-wal`/`-shm` when the last connection to a database
    /// closes, so a caller that dropped the connection before the driver
    /// process got a chance to `stat` the companions would see them
    /// vanish — not because publishing failed, but because nothing was
    /// holding the database open anymore. `publish_probe`'s `main` keeps
    /// this connection alive across a handshake with the driver for exactly
    /// that reason.
    #[cfg(unix)]
    pub fn probe_coordination_connect(path: &std::path::Path) -> rusqlite::Connection {
        crate::coordination::open_db(path)
    }

    /// Probe hook for Skuld's own test suite (`tests/lock_contention_regression.rs`,
    /// via the `lock_hold_probe` support binary): hold `path`'s coordination
    /// DB init lock — the real one `connect`/`open_db` use — for the
    /// duration of `while_held`, so a driver process can deterministically
    /// prove a second process's `try_lock` on the same lock file blocks
    /// while this one runs, and succeeds once it returns. Thin wrapper
    /// around `coordination`'s own `probe_hold_init_lock`, needed because
    /// the `lock` module is private to `coordination` and can't be reached
    /// from `lib.rs` directly.
    pub fn probe_hold_init_lock(path: &std::path::Path, while_held: impl FnOnce()) {
        crate::coordination::probe_hold_init_lock(path, while_held)
    }

    /// Probe hook for Skuld's own test suite (`tests/lock_contention_regression.rs`,
    /// via the `lock_try_probe` support binary): attempt a non-blocking
    /// `try_lock` on `path`'s coordination DB init lock file through a fresh
    /// handle, returning the raw result for the caller to report. See
    /// [`probe_hold_init_lock`].
    pub fn probe_try_init_lock(path: &std::path::Path) -> Result<(), std::fs::TryLockError> {
        crate::coordination::probe_try_init_lock(path)
    }

    /// Probe hook for Skuld's own test suite: register in the coordination
    /// DB at `path`, corrupt it so a later connection attempt fails, then
    /// panic — while the registration guard is still alive, so unwinding
    /// drops it. `TestRegistration::drop`'s own cleanup opens a connection
    /// too, so this reproduces a panic occurring *during* an active unwind
    /// (inside a `Drop` the unwind
    /// itself triggers). Without a `catch_unwind` guard in `Drop`, an
    /// uncaught panic there is a panic during a panic, which Rust turns into
    /// `abort()` (`SIGABRT`) — killing the whole process, not just this one
    /// failing test. Needs a genuine subprocess: aborting the calling
    /// process is the whole point of the probe.
    ///
    /// Corruption method: replace the DB file with a directory of the same
    /// name, rather than `chmod`ing it narrow, so this hook is meaningful on
    /// both platforms it runs on. On Unix, `ensure_published`'s no-replace
    /// rename still attempts to publish over the directory, but it fails
    /// `EEXIST` (a no-replace rename treats anything already at the target
    /// regardless of type as taken) and silently no-ops, so the failure
    /// surfaces from SQLite's own file open, same as on Windows (which
    /// skips the Unix-only publish step entirely): SQLite rejects a
    /// directory as a database (`SQLITE_CANTOPEN`) on both — confirmed on
    /// macOS, and Skuld's CI Windows lane is what confirms the Windows
    /// half. Either way, `chmod` has no Windows analogue and would have
    /// left this hook, and the `Drop` fix it exercises, untested on Windows
    /// CI even though the fix itself is platform-agnostic.
    pub fn probe_drop_panic_during_unwind(path: &std::path::Path) {
        let _registration = crate::coordination::coordinate(path, "probe", &[], "");
        std::fs::remove_file(path).unwrap_or_else(|e| panic!("probe: could not remove {path:?} to corrupt it: {e}"));
        std::fs::create_dir(path).unwrap_or_else(|e| panic!("probe: could not create a directory at {path:?}: {e}"));

        panic!(
            "probe: artificial panic to trigger unwind; TestRegistration::drop's own cleanup \
             must not be allowed to abort the process"
        );
    }
}
