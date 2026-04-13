//! Test runner: collects and executes tests via libtest-mimic.
//!
//! Tests come from two sources:
//! - `#[skuld::test]` attribute (inventory-registered [`TestDef`](crate::TestDef))
//! - [`TestRunner::add`] (runtime-generated tests)

use std::io::Write;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::OnceLock;
use std::time::Instant;

use clap::Parser;
use libtest_mimic::{Arguments, Trial};

use crate::capture::FdCapture;
use crate::fixture::{cleanup_process_fixtures, collect_fixture_requires, collect_fixture_serial, enter_test_scope};
use crate::label::{read_label_filter, resolve_labels, validate_labels, Label, LabelExpr, ModuleLabels};
use crate::{Ignore, TestDef};

// Debug env var =====

/// Returns `true` if `SKULD_DEBUG` is set to a non-empty, non-falsy
/// value. Cached on first call.
///
/// Truthy: any value other than `""`, `"0"`, `"false"`, `"no"`, `"off"`
/// (case-insensitive). This avoids the surprise of `SKULD_DEBUG=0`
/// enabling debug output.
fn skuld_debug() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| match std::env::var("SKULD_DEBUG") {
        Ok(v) => {
            let t = v.trim().to_ascii_lowercase();
            !t.is_empty() && t != "0" && t != "false" && t != "no" && t != "off"
        }
        Err(_) => false,
    })
}

/// Emit a `[skuld-debug]` line when `SKULD_DEBUG=1` is set. Always writes
/// to `io::stderr()` — which is the real stderr as long as the call
/// happens outside an [`FdCapture`] window. Callers must arrange for that.
macro_rules! skuld_debug_eprintln {
    ($($arg:tt)*) => {
        if skuld_debug() {
            eprintln!("[skuld-debug] {}", format_args!($($arg)*));
        }
    };
}

// Serial lock =====

/// Acquire an exclusive, blocking, cross-process file lock.
///
/// Uses `flock(LOCK_EX)` on Unix and `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK)`
/// on Windows. The OS automatically releases the lock when the process exits
/// (including crashes), so no manual cleanup is needed.
fn lock_exclusive(file: &std::fs::File) {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        assert!(ret == 0, "skuld: flock failed: {}", std::io::Error::last_os_error());
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{LockFileEx, LOCKFILE_EXCLUSIVE_LOCK};
        use windows::Win32::System::IO::OVERLAPPED;

        let handle = HANDLE(file.as_raw_handle() as _);
        let mut overlapped = OVERLAPPED::default();
        unsafe {
            LockFileEx(handle, LOCKFILE_EXCLUSIVE_LOCK, None, !0, !0, &mut overlapped)
                .expect("skuld: LockFileEx failed");
        }
    }
}

/// Release a file lock previously acquired by [`lock_exclusive`].
fn unlock(file: &std::fs::File) {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        assert!(
            ret == 0,
            "skuld: flock(LOCK_UN) failed: {}",
            std::io::Error::last_os_error()
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::UnlockFile;

        let handle = HANDLE(file.as_raw_handle() as _);
        unsafe {
            UnlockFile(handle, 0, 0, !0, !0).expect("skuld: UnlockFile failed");
        }
    }
}

/// RAII guard that holds both the in-process [`Mutex`] and the cross-process
/// file lock. Dropping it releases the file lock first, then the mutex.
struct SerialGuard {
    // Option so we can take the MutexGuard in drop without moving out of self.
    inner: Option<std::sync::MutexGuard<'static, std::fs::File>>,
}

impl Drop for SerialGuard {
    fn drop(&mut self) {
        if let Some(guard) = self.inner.take() {
            skuld_debug_eprintln!("serial: releasing file lock");
            unlock(&guard);
            // MutexGuard drops here, releasing the in-process lock.
        }
    }
}

/// File-backed serial lock shared across all test processes in this build
/// profile.
///
/// The lock file lives at `target/{profile}/.skuld-serial.lock`, resolved
/// at compile time from `OUT_DIR` so it works correctly with custom target
/// dirs. Under `cargo test` (single process, `--test-threads=1`), the lock
/// is trivially uncontended. Under `cargo nextest run` (process-per-test),
/// the file lock serializes across processes.
fn acquire_serial_lock() -> SerialGuard {
    use std::sync::Mutex;

    struct LockState {
        mtx: Mutex<std::fs::File>,
        path: std::path::PathBuf,
    }

    static STATE: OnceLock<LockState> = OnceLock::new();
    let state = STATE.get_or_init(|| {
        let path = std::path::Path::new(env!("SKULD_TARGET_PROFILE_DIR")).join(".skuld-serial.lock");
        skuld_debug_eprintln!("serial: lock file at {path:?}");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .unwrap_or_else(|e| panic!("skuld: failed to open serial lock at {path:?}: {e}"));
        LockState {
            mtx: Mutex::new(file),
            path,
        }
    });
    let guard = state.mtx.lock().unwrap_or_else(|e| e.into_inner());
    skuld_debug_eprintln!("serial: acquiring file lock at {:?}...", state.path);
    lock_exclusive(&guard);
    skuld_debug_eprintln!("serial: lock acquired at {:?}", state.path);
    SerialGuard { inner: Some(guard) }
}

/// Run `body` under the serial lock if `serial` is true, or directly otherwise.
pub(crate) fn run_maybe_serial(serial: bool, body: impl FnOnce()) {
    if serial {
        let _guard = acquire_serial_lock();
        body();
    } else {
        body();
    }
}

// Per-test observability ==============================================================================

/// Run one test body with per-test observability.
///
/// Emits `[skuld] <name>: starting` and `[skuld] <name>: pass|fail (NN ms)`
/// around the body. When `capture` is true, wraps the body in an
/// [`FdCapture`] that redirects stdout/stderr to an in-process pipe and
/// dumps the captured bytes to stderr on failure. When `capture` is false
/// (user passed `--nocapture`, or running under `cargo nextest run`),
/// the body runs with unmodified stdio and nothing intercepts its output.
fn run_with_observability(name: &str, capture: bool, serial: bool, body: impl FnOnce()) {
    // Runner-level "starting" line. Printed BEFORE FdCapture::begin so it
    // lands on the real terminal stderr, not in the capture buffer.
    eprintln!("[skuld] {name}: starting");
    skuld_debug_eprintln!("{name}: entering test scope");
    let started = Instant::now();

    // Set up capture if requested. Failure to begin a capture is
    // fatal: the process state may be partially corrupted (especially
    // on Windows, though `FdCapture::begin` is transactional so this
    // should never actually leak) and running the test body with
    // unknown stdio is worse than terminating the test run.
    //
    // The "capture enabled" debug print happens BEFORE `begin()`
    // because after `begin()` our eprintln goes into the capture
    // pipe, not the real stderr, and would be discarded on pass.
    let mut capture_guard: Option<FdCapture> = None;
    if capture {
        skuld_debug_eprintln!("{name}: capture enabled (fd redirect)");
        match FdCapture::begin() {
            Ok(c) => {
                capture_guard = Some(c);
            }
            Err(e) => {
                eprintln!("[skuld] {name}: FATAL: capture setup failed: {e}");
                eprintln!("[skuld] {name}: refusing to run test with unknown stdio state; aborting.");
                std::process::abort();
            }
        }
    }

    // NOTE: between here and `capture_guard.take().end()`, writes from
    // this thread to stdout/stderr go into the pipe. Do NOT eprintln!
    // debug output in this window — it would land in the capture buffer.

    let result = catch_unwind(AssertUnwindSafe(|| run_maybe_serial(serial, body)));

    let duration = started.elapsed();

    // Restore stdio before any further diagnostic output so we print to
    // the real terminal, not the capture buffer.
    let captured_bytes: Vec<u8> = match capture_guard.take() {
        Some(c) => match c.end() {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("[skuld] {name}: warning: capture teardown failed: {e}");
                Vec::new()
            }
        },
        None => Vec::new(),
    };

    skuld_debug_eprintln!("{name}: capture disabled");

    let outcome = if result.is_ok() { "pass" } else { "fail" };
    eprintln!("[skuld] {name}: {outcome} ({} ms)", duration.as_millis());

    if result.is_err() && !captured_bytes.is_empty() {
        eprintln!("[skuld] {name}: ---- captured ----");
        // Best-effort raw write; errors here are themselves diagnostic
        // noise and we cannot do anything useful with them.
        let _ = std::io::stderr().write_all(&captured_bytes);
        if !captured_bytes.ends_with(b"\n") {
            let _ = std::io::stderr().write_all(b"\n");
        }
        eprintln!("[skuld] {name}: ---- end capture ----");
    }

    if let Err(payload) = result {
        resume_unwind(payload);
    }
}

// Test runner =====================================================================================

/// A dynamically-added test (registered at runtime, not via proc macro).
struct DynTest {
    name: String,
    ignored: bool,
    serial: bool,
    labels: Vec<Label>,
    body: Box<dyn FnOnce() + Send + 'static>,
}

/// Collects tests from both `#[skuld::test]` (inventory) and runtime
/// [`add`](TestRunner::add) calls, then runs them via libtest-mimic.
#[derive(Default)]
pub struct TestRunner {
    dynamic: Vec<DynTest>,
    /// Custom args to strip before passing to libtest-mimic/clap.
    strip: Vec<String>,
}

impl TestRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register custom CLI args to strip before passing to libtest-mimic.
    ///
    /// Use for test-binary-specific flags (e.g. `--no-sandbox`) that would
    /// otherwise be rejected by the standard argument parser.
    pub fn strip_args(&mut self, args: &[&str]) -> &mut Self {
        self.strip.extend(args.iter().map(|s| s.to_string()));
        self
    }

    /// Add a test that was generated at runtime (e.g. from a data file).
    ///
    /// The `body` closure should panic on failure (like a normal test).
    pub fn add(
        &mut self,
        name: impl Into<String>,
        labels: &[Label],
        ignored: bool,
        body: impl FnOnce() + Send + 'static,
    ) {
        self.dynamic.push(DynTest {
            name: name.into(),
            ignored,
            serial: false,
            labels: labels.to_vec(),
            body: Box::new(body),
        });
    }

    /// Add a test that was generated at runtime, with serial execution.
    pub fn add_serial(
        &mut self,
        name: impl Into<String>,
        labels: &[Label],
        ignored: bool,
        body: impl FnOnce() + Send + 'static,
    ) {
        self.dynamic.push(DynTest {
            name: name.into(),
            ignored,
            serial: true,
            labels: labels.to_vec(),
            body: Box::new(body),
        });
    }

    /// Run all tests (inventory-registered + dynamic) and exit.
    pub fn run(self) -> ! {
        self.run_tests().exit();
    }

    /// Run all tests and return the conclusion for post-run assertions.
    pub fn run_tests(self) -> libtest_mimic::Conclusion {
        validate_labels();
        let label_filter = read_label_filter();
        let mut remaining_args: Vec<String> = std::env::args().collect();
        remaining_args.retain(|a| !self.strip.contains(a));
        let mut args = Arguments::parse_from(remaining_args);

        // Repurpose libtest-mimic's --nocapture as the on/off switch for
        // skuld's FD-level capture:
        //   * default (flag unset) — capture, force single-threaded
        //   * --nocapture (user flag or nextest) — no capture, respect
        //     the user's test_threads setting
        let capture = !args.nocapture;
        if capture {
            // FD redirect is process-wide; running tests in parallel
            // would interleave their output into one buffer. Force
            // single-threaded for the duration of this run.
            args.test_threads = Some(1);
        }
        skuld_debug_eprintln!("run_tests: capture={} test_threads={:?}", capture, args.test_threads);

        let mut trials = Vec::new();
        let mut unavailable: Vec<(String, String)> = Vec::new();

        // Collect module-level default labels.
        let module_defaults: Vec<&ModuleLabels> = inventory::iter::<ModuleLabels>.into_iter().collect();

        self.collect_inventory_tests(
            label_filter.as_ref(),
            &module_defaults,
            capture,
            &mut trials,
            &mut unavailable,
        );
        self.collect_dynamic_tests(label_filter.as_ref(), capture, &mut trials);

        let conclusion = libtest_mimic::run(&args, trials);

        // Clean up process-scoped fixtures (LIFO order).
        cleanup_process_fixtures();

        if !unavailable.is_empty() {
            eprintln!("\n--- Unavailable ({}) ---", unavailable.len());
            for (name, reason) in &unavailable {
                eprintln!("  {name}: {reason}");
            }
        }

        conclusion
    }

    fn collect_inventory_tests(
        &self,
        label_filter: Option<&LabelExpr>,
        module_defaults: &[&ModuleLabels],
        capture: bool,
        trials: &mut Vec<Trial>,
        unavailable: &mut Vec<(String, String)>,
    ) {
        for def in inventory::iter::<TestDef> {
            let resolved = resolve_labels(def, module_defaults);

            // Label filtering — skip entirely (not ignored, just absent).
            if let Some(filter) = label_filter {
                if !filter.matches(&resolved) {
                    continue;
                }
            }

            let trial_name = def.display_name.unwrap_or(def.name);

            // Static ignore — don't check preconditions, don't report as unavailable.
            if !matches!(def.ignore, Ignore::No) {
                trials.push(Trial::test(trial_name, || Ok(())).with_ignored_flag(true));
                continue;
            }

            // Collect requirements from both explicit requires and fixture dependencies.
            let fixture_requires = collect_fixture_requires(def.fixture_names);
            let reasons: Vec<String> = def
                .requires
                .iter()
                .chain(fixture_requires.into_iter())
                .filter_map(|req| req.eval().err())
                .collect();

            if reasons.is_empty() {
                let body = def.body;
                let is_serial = def.serial || collect_fixture_serial(def.fixture_names);
                let observed_name = trial_name.to_string();
                trials.push(Trial::test(trial_name, move || {
                    run_with_observability(&observed_name, capture, is_serial, body);
                    Ok(())
                }));
            } else {
                let reason = reasons.join("; ");
                unavailable.push((trial_name.to_string(), reason));
                trials.push(Trial::test(trial_name, || Ok(())).with_ignored_flag(true));
            }
        }
    }

    fn collect_dynamic_tests(self, label_filter: Option<&LabelExpr>, capture: bool, trials: &mut Vec<Trial>) {
        for dyn_test in self.dynamic {
            if let Some(filter) = label_filter {
                if !filter.matches(&dyn_test.labels) {
                    continue;
                }
            }

            let body = dyn_test.body;
            let is_serial = dyn_test.serial;
            // Intentional leak: dynamic test names need 'static lifetime for enter_test_scope.
            // Acceptable because the harness runs once per process.
            let name_static: &'static str = Box::leak(dyn_test.name.into_boxed_str());
            trials.push(
                Trial::test(name_static, move || {
                    run_with_observability(name_static, capture, is_serial, move || {
                        // Auto-wrap dynamic tests in a test scope so fixtures are available.
                        let _scope = enter_test_scope(name_static, "");
                        body();
                    });
                    Ok(())
                })
                .with_ignored_flag(dyn_test.ignored),
            );
        }
    }
}

/// Shorthand: run only inventory-registered tests and exit.
pub fn run_all() -> ! {
    TestRunner::new().run();
}
