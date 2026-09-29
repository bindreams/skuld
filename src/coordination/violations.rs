//! Process-wide record of cleanup failures that could not be raised as panics.
//!
//! `TestRegistration::drop` fails loudly, but a panic in `Drop` while the
//! thread is already unwinding aborts the process, so during an unwind it
//! downgrades to a warning. A warning does not fail the run: a `#[should_panic]`
//! test, or a test whose own failure is reported first, would hide a coordination
//! database that was moved or broken. So the downgraded failure is recorded
//! here, and the runner, which owns the process's exit, fails the run with it.

use std::sync::{Mutex, PoisonError};

static VIOLATIONS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn lock() -> std::sync::MutexGuard<'static, Vec<String>> {
    // The list is only ever pushed to or drained, so a panic while it was held
    // cannot leave it inconsistent.
    VIOLATIONS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Record a downgraded cleanup failure.
pub(super) fn record(message: String) {
    lock().push(message);
}

/// Drain everything recorded so far.
pub(crate) fn take() -> Vec<String> {
    std::mem::take(&mut *lock())
}

/// A copy of everything recorded so far, leaving it in place: for tests, which
/// share the process with every other test and must not drain their entries.
#[cfg(test)]
pub(in crate::coordination) fn recorded() -> Vec<String> {
    lock().clone()
}
