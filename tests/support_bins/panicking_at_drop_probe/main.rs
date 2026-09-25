//! Subject of subprocess invocations in `tests/panicking_at_drop_cli.rs`.
//! Not a real product binary.
//!
//! Pins two things about fixture teardown timing for a satisfied
//! `should_panic` test:
//!
//! 1. A Test-scoped fixture's `Drop` impl (`Tracked` below) must see
//!    `std::thread::panicking() == true`: `__scope` (which reclaims
//!    Test-scoped fixtures, dropping this one) is dropped from inside the
//!    `catch_unwind` closure that caught the test's own panic — while that
//!    panic is still unwinding through it — not after `catch_unwind` has
//!    already stopped the unwind.
//! 2. A Variable-scoped fixture that depends on a Test-scoped one
//!    (`Dependent` below, via `#[fixture(tracked)]`) must both see
//!    `panicking() == true` too, and drop *before* the Test-scoped fixture
//!    it depends on — same order the plain (non-should_panic) arm gets for
//!    free, since it never splits fixture teardown across two scopes.
//!    Dropping it after would mean `TestScope::drop` already reclaimed
//!    (`Box::from_raw`'d) `tracked`'s storage while `dependent`, which
//!    borrowed from it, was still alive — a dangling reference, not just a
//!    timing difference.
//!
//! Reads `SKULD_PANICKING_AT_DROP_PROBE_OUT` (required: a file path).
//! Each fixture's `Drop` appends one line there, tagged with its kind and
//! what `std::thread::panicking()` read at the moment it ran — appended
//! rather than overwritten so the driver can check both the values and the
//! order they were written in. Never panics itself (a panic here, while the
//! test's own panic is already unwinding through this same `Drop`, would be
//! a double panic — `SIGABRT` — which would only obscure the read this
//! probe exists to take).

fn log_drop(line: &str) {
    if let Ok(path) = std::env::var("SKULD_PANICKING_AT_DROP_PROBE_OUT") {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            use std::io::Write;
            let _ = writeln!(f, "{line}");
        }
    }
}

struct Tracked;

impl Drop for Tracked {
    fn drop(&mut self) {
        log_drop(&format!("test-scoped panicking={}", std::thread::panicking()));
    }
}

// Test-scoped: `__scope` (TestScope) is what reclaims Test-scoped fixtures,
// which is the drop path this probe exists to pin.
#[skuld::fixture(scope = test)]
fn tracked() -> Result<Tracked, String> {
    Ok(Tracked)
}

struct Dependent;

impl Drop for Dependent {
    fn drop(&mut self) {
        log_drop(&format!("variable-scoped panicking={}", std::thread::panicking()));
    }
}

// Variable-scoped (the default, no `scope = ...`): depends on `tracked`
// (Test-scoped) via `#[fixture(tracked)]`. Its `FixtureHandle` must move
// into the same catch_unwind closure as `__scope`, and drop before it, or
// this borrows-from-a-reclaimed-fixture hazard goes unnoticed.
#[skuld::fixture]
fn dependent(#[fixture(tracked)] _t: &Tracked) -> Result<Dependent, String> {
    Ok(Dependent)
}

#[skuld::test(should_panic)]
fn panics_with_tracked_fixture(#[fixture(dependent)] _v: &Dependent) {
    panic!("expected panic to pin fixture drop timing and order");
}

fn main() {
    skuld::run_all();
}
