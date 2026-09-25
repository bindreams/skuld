//! Subject of subprocess invocations in `tests/panicking_at_drop_cli.rs`.
//! Not a real product binary.
//!
//! Pins the timing of `__scope`'s `Drop` for a satisfied `should_panic`
//! test: a Test-scoped fixture whose value's `Drop` impl checks
//! `std::thread::panicking()` must see `true`, because `__scope` (which
//! reclaims Test-scoped fixtures, dropping this one) is dropped from inside
//! the `catch_unwind` closure that caught the test's own panic — while that
//! panic is still unwinding through it — not after `catch_unwind` has
//! already stopped the unwind. Before that fix, `__scope` was a local of
//! the *outer* closure and dropped only after `catch_unwind` returned, by
//! which point the thread was no longer panicking.
//!
//! Reads `SKULD_PANICKING_AT_DROP_PROBE_OUT` (required: a file path).
//! `Tracked::drop` writes `"true"` or `"false"` there, reflecting what
//! `std::thread::panicking()` read at the moment it ran. Never panics
//! itself (a panic here, while the test's own panic is already unwinding
//! through this same `Drop`, would be a double panic — `SIGABRT` — which
//! would only obscure the read this probe exists to take).

struct Tracked;

impl Drop for Tracked {
    fn drop(&mut self) {
        if let Ok(path) = std::env::var("SKULD_PANICKING_AT_DROP_PROBE_OUT") {
            let _ = std::fs::write(path, std::thread::panicking().to_string());
        }
    }
}

// Test-scoped: `__scope` (TestScope) is what reclaims Test-scoped fixtures,
// which is the drop path this probe exists to pin. A Variable-scoped fixture
// (the default with no `scope = ...`) is instead owned by its FixtureHandle, a
// local of the outer closure outside `catch_unwind`, so it would drop after
// `catch_unwind` already stopped the unwind regardless of this fix.
#[skuld::fixture(scope = test)]
fn tracked() -> Result<Tracked, String> {
    Ok(Tracked)
}

#[skuld::test(should_panic)]
fn panics_with_tracked_fixture(#[fixture(tracked)] _v: &Tracked) {
    panic!("expected panic to pin __scope's drop timing");
}

fn main() {
    skuld::run_all();
}
