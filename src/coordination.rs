//! Cross-process test coordination for serial execution.
//!
//! Every test (serial or not) registers itself in the [`registry`] before running, which lives
//! in `.skuld/` on OS file locks. Serial tests use it to block until their serialization
//! constraints are satisfied.

#[cfg(test)]
mod coordination_tests;
mod db_dir;
#[cfg(test)]
mod db_dir_tests;
mod file_identity;
#[cfg(test)]
mod file_identity_tests;
mod lock;
#[cfg(test)]
mod lock_tests;
#[cfg(unix)]
mod publish;
#[cfg(all(test, unix))]
mod publish_tests;
mod registry;
#[cfg(test)]
mod registry_tests;
#[cfg(test)]
mod rendezvous;
#[cfg(test)]
mod rendezvous_tests;
#[cfg(test)]
mod test_hooks;
#[cfg(test)]
mod test_hooks_tests;
mod violations;

use crate::label::{Label, LabelFilter};

pub(crate) use violations::take as take_violations;

use std::path::PathBuf;

// Sentinel values for the `serial` field on TestDef / FixtureDef =====

/// Test is not serial — runs concurrently with everything.
pub const SERIAL_NONE: &str = "";

/// Test is serial with everything — no other test may run concurrently.
pub const SERIAL_ALL: &str = "*";

// Coordination directory =====

/// Environment variable that overrides the directory that holds the coordination directory
/// `.skuld/`. Must be an absolute path; created if missing.
pub(crate) const DB_DIR_ENV: &str = "SKULD_DB_DIR";

/// Appended to failures caused by an unusable coordination directory.
pub(crate) const DB_DIR_HINT: &str = "if the directory is unusable, set SKULD_DB_DIR to a writable absolute directory";

/// The coordination directory, `<resolved>/.skuld`, resolved at run time, once per process.
///
/// Once, so that an environment change made by a test body (`SKULD_DB_DIR` via the `env`
/// fixture, say) cannot split later trials of this process across two directories.
///
/// Panics, naming the cause, when the directory cannot be determined, created or used.
/// There is deliberately no fallback: a per-process directory would silently disable
/// cross-process locking.
pub(crate) fn coord_dir() -> PathBuf {
    use std::sync::OnceLock;
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let over = std::env::var_os(DB_DIR_ENV);
        match db_dir::resolve(over.as_deref(), std::env::current_exe) {
            Ok(dir) => dir.join(".skuld"),
            Err(msg) => panic!("skuld: {msg}"),
        }
    })
    .clone()
}

// Test probe hooks =====

/// Probe hook for Skuld's own test suite (`tests/lock_contention_regression.rs`,
/// via the `lock_hold_probe` support binary): hold `coord_dir`'s real global mutex — the one
/// registration takes — for exactly the duration of `while_held`, so a driver process can
/// deterministically prove a second process's `try_lock` on the same lock target reports
/// `WouldBlock` while this one is running, then succeeds once it returns.
pub(crate) fn probe_hold_global_lock(coord_dir: &std::path::Path, while_held: impl FnOnce()) {
    lock::with_global_lock(&registry::mutex_base(coord_dir), |_token| while_held())
}

/// Probe hook for Skuld's own test suite (`tests/lock_contention_regression.rs`,
/// via the `lock_try_probe` support binary): open a *fresh* handle on `coord_dir`'s lock
/// target — never the one [`probe_hold_global_lock`] or a registration holds — and attempt a
/// non-blocking `try_lock` on it, returning the raw result. A fresh handle matters because
/// `flock`/`LockFileEx` locks are scoped to the open file description/handle, not the process:
/// only a genuinely separate handle (here, in a separate process) can observe contention.
pub(crate) fn probe_try_global_lock(coord_dir: &std::path::Path) -> Result<(), std::fs::TryLockError> {
    lock::try_lock_exclusive(&lock::open_lock_target(&registry::mutex_base(coord_dir)))
}

/// Canonicalize a raw `serial_filter` string for storage in a registry entry.
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

// RAII guard =====

/// RAII guard that unregisters the test from the coordination directory on drop, including while
/// unwinding from a panic.
///
/// [`registry::Registration::unregister`] panics when the coordination directory moved
/// mid-run. That panic cannot propagate while the thread is already unwinding (a second panic
/// aborts the process): it is downgraded to a warning and recorded in `violations`, which fails
/// the run.
pub(crate) struct TestRegistration {
    registration: registry::Registration,
}

impl Drop for TestRegistration {
    fn drop(&mut self) {
        // `unregister` panics on a moved directory, which should propagate. But if this drop
        // runs because the thread is already unwinding, a second panic aborts the whole
        // process, so catch it to tell the two cases apart.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.registration.unregister()));
        match result {
            Ok(()) => {}
            Err(payload) => {
                if std::thread::panicking() {
                    // Downgraded to a warning, and recorded so the runner fails the run with it
                    // (see `violations`).
                    let warning = downgraded_warning_message(&payload);
                    eprintln!("{warning}");
                    violations::record(warning);
                } else {
                    std::panic::resume_unwind(payload);
                }
            }
        }
    }
}

/// Build the downgraded-warning line for a panic payload caught from the
/// `unregister` call above, for the case where the drop is already running
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
        "[skuld] warning: coordination cleanup panicked while already unwinding from another \
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

/// Coordinate test execution: block until the test can start, register it, and return a guard
/// that unregisters it on drop.
///
/// Waits by the registry's retry loop (10 ms → 200 ms backoff, uncapped) and emits a debug
/// warning after 60 s.
///
/// This is the main entry point called by the test runner for every test.
pub(crate) fn coordinate(
    coord_dir: &std::path::Path,
    name: &str,
    labels: &[Label],
    serial_filter: &str,
) -> TestRegistration {
    // Canonicalize once up front so every comparison in the registry operates on the storage
    // form (e.g. "a | !a" → "*").
    let registration = registry::register(coord_dir, name, labels, &to_storage(serial_filter));
    TestRegistration { registration }
}

macro_rules! skuld_debug_eprintln {
    ($($arg:tt)*) => {
        if crate::runner::skuld_debug() {
            eprintln!("[skuld-debug] {}", format_args!($($arg)*));
        }
    };
}
use skuld_debug_eprintln;
