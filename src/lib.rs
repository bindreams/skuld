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

pub mod fixture;
pub mod fixtures;
pub mod label;
pub mod probe;
pub mod runner;

pub use fixture::{
    cleanup_process_fixtures, collect_fixture_requires, collect_fixture_serial, enter_test_scope, fixture, fixture_get,
    fixture_registry, warm_up, FixtureDef, FixtureHandle, FixtureRef, FixtureScope, TestScope,
};
pub use fixtures::cwd::{cwd, CwdGuard};
pub use fixtures::env::{env, EnvGuard};
pub use fixtures::temp_dir::{temp_dir, TempDir};
pub use fixtures::test_name::{test_name, TestName};
pub use label::ModuleLabels;
pub use probe::{probe_executable, probe_path};
pub use runner::{run_all, TestRunner};

use std::cell::Cell;

// Re-export proc macros for consumers.
pub use skuld_macros::fixture;
pub use skuld_macros::test;

// Re-export inventory so that macro-generated `inventory::submit!` calls resolve.
pub use inventory;

/// A precondition check function. Returns `Ok(())` if the requirement is met,
/// or `Err(reason)` if not.
pub type RequireFn = fn() -> Result<(), String>;

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
    pub requires: &'static [RequireFn],
    /// Names of fixtures used by this test (from `#[fixture]` params).
    /// Used for transitive requirement collection via [`collect_fixture_requires`].
    pub fixture_names: &'static [&'static str],
    pub ignore: Ignore,
    /// Labels for filtering. Stored in libtest-mimic's `kind` field joined by `:`.
    pub labels: &'static [&'static str],
    /// Whether `labels = [...]` was explicitly written (even if empty).
    /// When false, module-level defaults from `default_labels!` apply.
    pub labels_explicit: bool,
    /// Whether this test must run under the global serial lock.
    /// Propagated transitively from fixtures via [`collect_fixture_serial`].
    pub serial: bool,
    pub body: fn(),
}

inventory::collect!(TestDef);
