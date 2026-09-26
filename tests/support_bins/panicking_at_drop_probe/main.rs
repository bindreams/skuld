//! Subject of subprocess invocations in `tests/panicking_at_drop_cli.rs`.
//! Not a real product binary.
//!
//! Pins two things about fixture teardown timing for a satisfied
//! `should_panic` test, for both the bare (`Yes`) and message-checked
//! (`WithMessage`) arms:
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
//! `panics_with_tracked_fixture` and `panics_with_tracked_fixture_msg`
//! (`Yes` and `WithMessage`) pin these for a *satisfied* should_panic test.
//! `body_completes_but_fixture_drop_panics` and its `_msg` twin pin the
//! opposite case: the test body never panics, but a fixture's `Drop`
//! (`PanicsOnDrop` below) does. Both arms must fail such a test — a
//! teardown panic must never be mistaken for the expected one, the same
//! way the plain (non-should_panic) arm would fail this shape.
//!
//! Reads `SKULD_PANICKING_AT_DROP_PROBE_OUT` (required for the two
//! drop-order tests: a file path). Each of `Tracked`'s and `Dependent`'s
//! `Drop` appends one line there, tagged with its kind and what
//! `std::thread::panicking()` read at the moment it ran — appended rather
//! than overwritten so the driver can check both the values and the order
//! they were written in. Never panics itself (a panic here, while the
//! test's own panic is already unwinding through this same `Drop`, would be
//! a double panic — `SIGABRT` — which would only obscure the read this
//! probe exists to take). `PanicsOnDrop::drop` is the one exception: it
//! panics unconditionally, since being the teardown panic under test is its
//! entire purpose.
//!
//! The driver runs each test individually (`--exact <name>`), since the
//! two `body_completes_but_fixture_drop_panics*` tests are expected to
//! fail the process and must not affect the other tests' exit status.

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

// Same scenario as above, but for the message-checked (`WithMessage`) arm:
// the drop-order/panicking() guarantees must hold there too, not just for
// the bare arm.
#[skuld::test(should_panic = "expected panic to pin fixture drop timing and order")]
fn panics_with_tracked_fixture_msg(#[fixture(dependent)] _v: &Dependent) {
    panic!("expected panic to pin fixture drop timing and order");
}

struct PanicsOnDrop;

impl Drop for PanicsOnDrop {
    fn drop(&mut self) {
        panic!("PanicsOnDrop::drop panicked");
    }
}

#[skuld::fixture]
fn panics_on_drop() -> Result<PanicsOnDrop, String> {
    Ok(PanicsOnDrop)
}

// Body never panics; `panics_on_drop`'s teardown does. Must FAIL: a
// teardown panic isn't the body panic should_panic contracts for.
#[skuld::test(should_panic)]
fn body_completes_but_fixture_drop_panics(#[fixture(panics_on_drop)] _p: &PanicsOnDrop) {}

// Same, but for `WithMessage`, with the expected substring set to match
// the teardown panic's own message — the exact shape that would let the
// pre-fix code mistake it for a satisfied expectation.
#[skuld::test(should_panic = "PanicsOnDrop::drop panicked")]
fn body_completes_but_fixture_drop_panics_msg(#[fixture(panics_on_drop)] _p: &PanicsOnDrop) {}

fn main() {
    skuld::run_all();
}
