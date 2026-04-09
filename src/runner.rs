//! Test runner: collects and executes tests via libtest-mimic.
//!
//! Tests come from two sources:
//! - `#[skuld::test]` attribute (inventory-registered [`TestDef`](crate::TestDef))
//! - [`TestRunner::add`] (runtime-generated tests)

use std::io::Write;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::Mutex;
use std::time::Instant;

use clap::Parser;
use libtest_mimic::{Arguments, Trial};
use tracing_subscriber::EnvFilter;

use crate::capture::CaptureBuffer;
use crate::fixture::{cleanup_process_fixtures, collect_fixture_requires, collect_fixture_serial, enter_test_scope};
use crate::label::{extract_label_filters, label_matches, resolve_labels, LabelSelector, ModuleLabels};
use crate::{Ignore, TestDef};

// Serial lock =========================================================================================

/// Global mutex for tests marked `serial`. Ensures only one serial test runs at a time.
static SERIAL_LOCK: Mutex<()> = Mutex::new(());

/// Run `body` under the serial lock if `serial` is true, or directly otherwise.
fn run_maybe_serial(serial: bool, body: impl FnOnce()) {
    if serial {
        let _guard = SERIAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        body();
    } else {
        body();
    }
}

// Per-test observability ==============================================================================

/// Run one test body with per-test observability:
///
/// 1. **Runner events** — prints `[skuld] <name>: starting` and
///    `[skuld] <name>: pass|fail (NN ms)` to stderr unconditionally. Always
///    visible regardless of pass/fail so per-test durations land in the
///    normal `cargo test` output.
/// 2. **Library tracing capture** — installs a thread-local
///    [`tracing_subscriber::fmt`] subscriber whose writer is an in-memory
///    buffer. On pass, the buffer is discarded. On panic, the buffer is
///    drained to stderr with a clear header/footer before the panic is
///    re-raised so libtest-mimic still sees the failure.
///
/// The subscriber is installed via
/// [`tracing::subscriber::set_default`] which is strictly thread-local —
/// two concurrent tests on different libtest-mimic worker threads cannot
/// see each other's events. See [`crate::capture`] for the capture
/// buffer design and its known limitations.
fn run_with_observability(name: &str, serial: bool, body: impl FnOnce()) {
    // Runner-level "starting" line. Eprintln NOT tracing, so it's visible
    // on every run regardless of whether a subscriber is installed.
    eprintln!("[skuld] {name}: starting");
    let started = Instant::now();

    // Per-test capture buffer + subscriber. The subscriber lives only for
    // the duration of `_guard`; dropping `_guard` restores whatever was
    // default on this thread before (usually nothing).
    //
    // Default filter is `off` so passing runs pay ~zero subscriber cost
    // (tracing filters events before formatting). Set `RUST_LOG=info`
    // (or `hole_bridge=debug`, etc.) to activate capture during an
    // investigation. This matches the industry-standard Rust test log
    // pattern and avoids tipping the #147/#165 cumulative-overhead
    // threshold on marginal CI runners.
    let buffer = CaptureBuffer::new();
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("off"));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buffer.make_writer())
        .with_env_filter(env_filter)
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    // Catch panics so we can drain the buffer before re-raising.
    let result = catch_unwind(AssertUnwindSafe(|| run_maybe_serial(serial, body)));

    let duration = started.elapsed();

    // Detach the subscriber BEFORE printing runner events / capture dumps
    // so our own eprintln output doesn't recurse into the buffer.
    drop(_guard);

    let outcome = if result.is_ok() { "pass" } else { "fail" };
    eprintln!(
        "[skuld] {name}: {outcome} ({} ms)",
        duration.as_millis()
    );

    if result.is_err() {
        let bytes = buffer.snapshot();
        if !bytes.is_empty() {
            eprintln!("[skuld] {name}: ---- captured tracing events ----");
            // Best-effort raw write of the captured bytes. Errors here are
            // themselves diagnostic noise, so we drop them.
            let _ = std::io::stderr().write_all(&bytes);
            // Make sure the dump ends with a newline before the footer.
            if !bytes.ends_with(b"\n") {
                let _ = std::io::stderr().write_all(b"\n");
            }
            eprintln!("[skuld] {name}: ---- end capture ----");
        }
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
    labels: Vec<String>,
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
        labels: &[&str],
        ignored: bool,
        body: impl FnOnce() + Send + 'static,
    ) {
        self.dynamic.push(DynTest {
            name: name.into(),
            ignored,
            serial: false,
            labels: labels.iter().map(|s| s.to_string()).collect(),
            body: Box::new(body),
        });
    }

    /// Add a test that was generated at runtime, with serial execution.
    pub fn add_serial(
        &mut self,
        name: impl Into<String>,
        labels: &[&str],
        ignored: bool,
        body: impl FnOnce() + Send + 'static,
    ) {
        self.dynamic.push(DynTest {
            name: name.into(),
            ignored,
            serial: true,
            labels: labels.iter().map(|s| s.to_string()).collect(),
            body: Box::new(body),
        });
    }

    /// Run all tests (inventory-registered + dynamic) and exit.
    pub fn run(self) -> ! {
        self.run_tests().exit();
    }

    /// Run all tests and return the conclusion for post-run assertions.
    pub fn run_tests(self) -> libtest_mimic::Conclusion {
        let (label_selectors, mut remaining_args) = extract_label_filters();
        remaining_args.retain(|a| !self.strip.contains(a));
        let args = Arguments::parse_from(remaining_args);
        let mut trials = Vec::new();
        let mut unavailable: Vec<(String, String)> = Vec::new();

        // Collect module-level default labels.
        let module_defaults: Vec<&ModuleLabels> = inventory::iter::<ModuleLabels>.into_iter().collect();

        self.collect_inventory_tests(&label_selectors, &module_defaults, &mut trials, &mut unavailable);
        self.collect_dynamic_tests(&label_selectors, &mut trials);

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
        label_selectors: &[LabelSelector],
        module_defaults: &[&ModuleLabels],
        trials: &mut Vec<Trial>,
        unavailable: &mut Vec<(String, String)>,
    ) {
        for def in inventory::iter::<TestDef> {
            let resolved = resolve_labels(def, module_defaults);
            let resolved_refs: Vec<&str> = resolved.iter().map(|s| s.as_str()).collect();

            // Label filtering — skip entirely (not ignored, just absent).
            if !label_selectors.is_empty() && !label_matches(&resolved_refs, label_selectors) {
                continue;
            }

            let trial_name = def.display_name.unwrap_or(def.name);
            let kind = resolved.join(":");

            // Static ignore — don't check preconditions, don't report as unavailable.
            if !matches!(def.ignore, Ignore::No) {
                let trial = Trial::test(trial_name, || Ok(())).with_ignored_flag(true);
                let trial = if kind.is_empty() { trial } else { trial.with_kind(kind) };
                trials.push(trial);
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
                let trial = Trial::test(trial_name, move || {
                    run_with_observability(&observed_name, is_serial, body);
                    Ok(())
                });
                let trial = if kind.is_empty() { trial } else { trial.with_kind(kind) };
                trials.push(trial);
            } else {
                let reason = reasons.join("; ");
                unavailable.push((trial_name.to_string(), reason));
                let trial = Trial::test(trial_name, || Ok(())).with_ignored_flag(true);
                let trial = if kind.is_empty() { trial } else { trial.with_kind(kind) };
                trials.push(trial);
            }
        }
    }

    fn collect_dynamic_tests(self, label_selectors: &[LabelSelector], trials: &mut Vec<Trial>) {
        for dyn_test in self.dynamic {
            let dyn_labels: Vec<&str> = dyn_test.labels.iter().map(|s| s.as_str()).collect();
            if !label_selectors.is_empty() && !label_matches(&dyn_labels, label_selectors) {
                continue;
            }

            let kind = dyn_test.labels.join(":");
            let body = dyn_test.body;
            let is_serial = dyn_test.serial;
            // Intentional leak: dynamic test names need 'static lifetime for enter_test_scope.
            // Acceptable because the harness runs once per process.
            let name_static: &'static str = Box::leak(dyn_test.name.into_boxed_str());
            let trial = Trial::test(name_static, move || {
                run_with_observability(name_static, is_serial, move || {
                    // Auto-wrap dynamic tests in a test scope so fixtures are available.
                    let _scope = enter_test_scope(name_static, "");
                    body();
                });
                Ok(())
            })
            .with_ignored_flag(dyn_test.ignored);
            let trial = if kind.is_empty() { trial } else { trial.with_kind(kind) };
            trials.push(trial);
        }
    }
}

/// Shorthand: run only inventory-registered tests and exit.
pub fn run_all() -> ! {
    TestRunner::new().run();
}
