//! Test runner: collects and executes tests via libtest-mimic.
//!
//! Tests come from two sources:
//! - `#[skuld::test]` attribute (inventory-registered [`TestDef`](crate::TestDef))
//! - [`TestRunner::add`] (runtime-generated tests)

use std::io::Write;
use std::panic::resume_unwind;
use std::sync::OnceLock;
use std::time::Instant;

use clap::Parser;
use libtest_mimic::{Arguments, Trial};

use crate::capture::FdCapture;
use crate::fixture::{
    cleanup_process_fixtures, collect_fixture_requires, collect_fixture_serial, enter_test_scope, merge_serial_filters,
};
use crate::label::{
    read_label_filter, resolve_labels, validate_labels, validate_serial_filters, Label, LabelFilter, ModuleLabels,
};
use crate::{Ignore, TestDef};

// Debug env var =====

/// Returns `true` if `SKULD_DEBUG` is set to a non-empty, non-falsy
/// value. Cached on first call.
///
/// Truthy: any value other than `""`, `"0"`, `"false"`, `"no"`, `"off"`
/// (case-insensitive). This avoids the surprise of `SKULD_DEBUG=0`
/// enabling debug output.
pub(crate) fn skuld_debug() -> bool {
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

// Nextest metadata dump ================================================================================

/// Per-test metadata exposed to external nextest-integration tooling via
/// [`SKULD_NEXTEST_METADATA_PATH_ENV`]. `pub` and `Deserialize` so a
/// consumer in the same workspace can deserialize the dump directly into
/// this type instead of maintaining its own mirror.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NextestTestMetadata {
    pub name: String,
    pub labels: Vec<String>,
    pub serial_filter: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct NextestMetadataDump {
    tests: Vec<NextestTestMetadata>,
}

const SKULD_NEXTEST_METADATA_PATH_ENV: &str = "SKULD_NEXTEST_METADATA_PATH";

/// Write `tests` as JSON to `path`. Pure — no env access — so it's directly
/// unit-testable without mutating process-global state.
pub(crate) fn write_nextest_metadata(path: &std::path::Path, tests: Vec<NextestTestMetadata>) {
    let dump = NextestMetadataDump { tests };
    let json =
        serde_json::to_string(&dump).unwrap_or_else(|e| panic!("skuld: failed to serialize nextest metadata: {e}"));
    std::fs::write(path, json).unwrap_or_else(|e| panic!("skuld: failed to write nextest metadata to {path:?}: {e}"));
}

/// Write `tests` to the path named by [`SKULD_NEXTEST_METADATA_PATH_ENV`],
/// if that env var is set and valid Unicode. Distinguishes "unset" (silent
/// no-op — the common case) from "set but not valid UTF-8" (a genuine
/// misconfiguration — warned, not silently dropped).
fn dump_nextest_metadata_if_requested(tests: Vec<NextestTestMetadata>) {
    match std::env::var(SKULD_NEXTEST_METADATA_PATH_ENV) {
        Ok(path) => write_nextest_metadata(std::path::Path::new(&path), tests),
        Err(std::env::VarError::NotPresent) => {}
        Err(std::env::VarError::NotUnicode(raw)) => {
            eprintln!(
                "[skuld] warning: {SKULD_NEXTEST_METADATA_PATH_ENV} is set but not valid UTF-8 ({raw:?}); skipping nextest metadata dump"
            );
        }
    }
}

// Per-test observability ==============================================================================

/// Run one test body with per-test observability and serial coordination.
///
/// Every test (serial or not) registers in the coordination database before
/// running and unregisters on completion. Serial tests block until their
/// constraints are satisfied.
///
/// Emits `[skuld] <name>: starting` and `[skuld] <name>: pass|fail (NN ms)`
/// around the body. When `capture` is true, wraps the body in an
/// [`FdCapture`] that redirects stdout/stderr to an in-process pipe and
/// dumps the captured bytes to stderr on failure.
///
/// Runs on a fresh, trial-named thread rather than libtest-mimic's
/// dispatching thread, so `thread_local!` state left dirty by one trial can
/// never leak into the next. `JoinHandle::join()` catches a body panic
/// automatically; the payload is re-thrown via `resume_unwind` below so
/// libtest-mimic still reports it as a normal trial failure.
fn run_with_observability(
    name: &str,
    capture: bool,
    serial_filter: &str,
    labels: &[Label],
    body: impl FnOnce() + Send + 'static,
) {
    use crate::coordination;

    // Check the name before anything else, including the "starting" line and
    // `FdCapture::begin`: a panic here must never land inside the capture
    // window, where `FdCapture`'s `Drop` (not `end`) would run and silently
    // discard it instead of dumping it (see the comment on `FdCapture`'s
    // `Drop` impl in `capture.rs`).
    ensure_valid_thread_name(name);

    let db_path = coordination::db_path();

    // Runner-level "starting" line. Printed BEFORE FdCapture::begin so it
    // lands on the real terminal stderr, not in the capture buffer.
    eprintln!("[skuld] {name}: starting");
    skuld_debug_eprintln!("{name}: entering test scope");
    let started = Instant::now();

    // Set up capture if requested.
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

    // Run on a fresh, trial-named thread rather than libtest-mimic's
    // dispatching thread. `thread::Builder::spawn` requires 'static captures,
    // so everything borrowed from the caller is cloned into the closure. The
    // name was already checked above, before the capture window opened;
    // `spawn` would otherwise panic on an interior NUL byte with a generic
    // std message that doesn't say which trial's name was the problem.
    let thread_name = name.to_string();
    let coordinate_name = thread_name.clone();
    let serial_filter_owned = serial_filter.to_string();
    let labels_owned = labels.to_vec();
    let handle = match std::thread::Builder::new().name(thread_name).spawn(move || {
        // Coordinate: register in DB, block if serial constraints aren't met.
        // The registration guard unregisters on drop (including panic unwind).
        let _reg = coordination::coordinate(&db_path, &coordinate_name, &labels_owned, &serial_filter_owned);
        body();
    }) {
        Ok(h) => h,
        Err(e) => {
            // This panic itself happens inside the capture window (when
            // `capture` is true): restore stdio first so the message reaches
            // the real terminal instead of being silently discarded by
            // `FdCapture`'s `Drop` (not `end`) — see the comment on
            // `FdCapture`'s `Drop` impl in `capture.rs`, and the identical
            // concern for `ensure_valid_thread_name` above (which sidesteps
            // it by running before the window opens; a spawn failure can't
            // be checked that early).
            if let Some(c) = capture_guard.take() {
                let _ = c.end();
            }
            panic!("skuld: failed to spawn trial thread for {name:?}: {e}");
        }
    };
    // JoinHandle::join() already catches a body panic and returns it as
    // Err — no manual catch_unwind needed. The payload propagates below via
    // resume_unwind so libtest-mimic still reports it as a normal failure.
    let result = handle.join();

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

/// Reject a trial name `thread::Builder::spawn` can't use as a thread name.
/// The only such name is one with an interior NUL byte (`CString::new`
/// rejects it); std's own panic for that case doesn't say which trial it
/// was, so check first and panic with the trial name attached.
pub(crate) fn ensure_valid_thread_name(name: &str) {
    if name.contains('\0') {
        panic!("skuld: trial name {name:?} contains a NUL byte and can't be used as a thread name");
    }
}

/// Build a libtest-mimic [`Trial`] for an inventory-registered test.
///
/// When `ignored` is true, libtest-mimic skips the trial by default but
/// runs the real body under `--ignored` / `--include-ignored`. The real
/// body is always passed in — the ignored flag gates execution, not
/// construction. Mirrors the dynamic-tests path.
fn build_inventory_trial(
    trial_name: String,
    labels: Vec<Label>,
    effective_serial: String,
    body: fn(),
    capture: bool,
    ignored: bool,
) -> Trial {
    let observed_name = trial_name.clone();
    let trial = Trial::test(trial_name, move || {
        run_with_observability(&observed_name, capture, &effective_serial, &labels, body);
        Ok(())
    });
    if ignored {
        trial.with_ignored_flag(true)
    } else {
        trial
    }
}

// Trial names =========================================================================================

/// Compute a test's final trial name.
///
/// `display_name` always wins. Otherwise: bare `name` by default, or —
/// with `libtest_names` on — `module_path!()` with its first segment (the
/// crate name) stripped, joined to `name` with `::`. A crate-root test
/// (module has no `::`) keeps its bare name either way.
pub(crate) fn effective_trial_name(
    module: &str,
    name: &str,
    display_name: Option<&str>,
    libtest_names: bool,
) -> String {
    if let Some(d) = display_name {
        return d.to_string();
    }
    if !libtest_names {
        return name.to_string();
    }
    match module.split_once("::") {
        Some((_crate_name, rest)) => format!("{rest}::{name}"),
        None => name.to_string(),
    }
}

/// Gather `(trial_name, origin)` pairs for every inventory-registered and
/// dynamic test, for the startup duplicate-name check. `origin` is a
/// human-readable location used only in panic messages.
pub(crate) fn collect_trial_name_entries(libtest_names: bool, dynamic: &[DynTest]) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    for def in inventory::iter::<TestDef> {
        let trial_name = effective_trial_name(def.module, def.name, def.display_name, libtest_names);
        let origin = format!("{}::{}", def.module, def.name);
        entries.push((trial_name, origin));
    }
    for dyn_test in dynamic {
        let origin = format!("dynamically-added: {}", dyn_test.name);
        entries.push((dyn_test.name.clone(), origin));
    }
    entries
}

/// Inner validation that returns the error message instead of panicking, so
/// unit tests can assert on specific failures. Modeled on
/// [`check_label_registry`](crate::label::check_label_registry).
///
/// Buckets all entries by trial name and returns an error for every bucket
/// with more than one entry, including every origin so a single run
/// surfaces all duplicates.
pub(crate) fn check_duplicate_trial_names(entries: &[(String, String)]) -> Result<(), String> {
    use std::collections::HashMap;

    let mut by_name: HashMap<&str, Vec<&str>> = HashMap::new();
    for (name, origin) in entries {
        by_name.entry(name.as_str()).or_default().push(origin.as_str());
    }

    let mut errors: Vec<String> = Vec::new();
    let mut sorted_names: Vec<&&str> = by_name.keys().collect();
    sorted_names.sort();
    for name in sorted_names {
        let origins = &by_name[name];
        if origins.len() > 1 {
            let locations: Vec<String> = origins.iter().map(|o| format!("  {o}")).collect();
            errors.push(format!(
                "trial name {name:?} declared multiple times:\n{}",
                locations.join("\n")
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("skuld: trial name validation failed:\n{}", errors.join("\n")))
    }
}

/// Validate that the final set of trial names (inventory and dynamic,
/// after `display_name` and `libtest_names()`) has no duplicates. Called at
/// the start of [`TestRunner::run_tests()`], before `self` is consumed by
/// [`TestRunner::collect_dynamic_tests`]. Runs whether or not
/// `libtest_names()` is on.
pub(crate) fn validate_trial_names(libtest_names: bool, dynamic: &[DynTest]) {
    let entries = collect_trial_name_entries(libtest_names, dynamic);
    if let Err(msg) = check_duplicate_trial_names(&entries) {
        panic!("{msg}");
    }
}

// Test runner =====================================================================================

/// A dynamically-added test (registered at runtime, not via proc macro).
pub(crate) struct DynTest {
    name: String,
    ignored: bool,
    serial: String,
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
    /// Opt-in libtest-style trial names. See [`effective_trial_name`].
    libtest_names: bool,
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

    /// Opt into libtest-style trial names: `module_path!()` with its first
    /// segment (the crate name) stripped, joined to the fn name with `::`.
    /// A crate-root test keeps its bare name. An explicit `display_name`
    /// (from `#[skuld::test(name = "...")]`) always wins over this.
    ///
    /// Regardless of this setting, the final set of trial names (inventory
    /// and dynamic together) must be free of duplicates — checked at
    /// startup, in [`TestRunner::run_tests`].
    pub fn libtest_names(&mut self) -> &mut Self {
        self.libtest_names = true;
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
            serial: String::new(),
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
            serial: "*".to_string(),
            labels: labels.to_vec(),
            body: Box::new(body),
        });
    }

    /// Add a test with filtered serial execution.
    pub fn add_serial_with(
        &mut self,
        name: impl Into<String>,
        labels: &[Label],
        ignored: bool,
        filter: LabelFilter,
        body: impl FnOnce() + Send + 'static,
    ) {
        self.dynamic.push(DynTest {
            name: name.into(),
            ignored,
            serial: filter.to_string(),
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
        validate_serial_filters();
        validate_trial_names(self.libtest_names, &self.dynamic);
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
        let mut nextest_metadata: Vec<NextestTestMetadata> = Vec::new();

        // Collect module-level default labels.
        let module_defaults: Vec<&ModuleLabels> = inventory::iter::<ModuleLabels>.into_iter().collect();

        self.collect_inventory_tests(
            label_filter.as_ref(),
            &module_defaults,
            capture,
            &mut trials,
            &mut unavailable,
            &mut nextest_metadata,
        );
        self.collect_dynamic_tests(label_filter.as_ref(), capture, &mut trials, &mut nextest_metadata);

        if args.list {
            dump_nextest_metadata_if_requested(nextest_metadata);
        }

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

    pub(crate) fn collect_inventory_tests(
        &self,
        label_filter: Option<&LabelFilter>,
        module_defaults: &[&ModuleLabels],
        capture: bool,
        trials: &mut Vec<Trial>,
        unavailable: &mut Vec<(String, String)>,
        metadata: &mut Vec<NextestTestMetadata>,
    ) {
        for def in inventory::iter::<TestDef> {
            let resolved = resolve_labels(def, module_defaults);

            // Label filtering — skip entirely (not ignored, just absent).
            if let Some(filter) = label_filter {
                if !filter.matches(&resolved) {
                    continue;
                }
            }

            let trial_name = effective_trial_name(def.module, def.name, def.display_name, self.libtest_names);
            let fixture_serial = collect_fixture_serial(def.fixture_names);
            let effective_serial = merge_serial_filters(def.serial, &fixture_serial);

            // Determine the ignored flag and the optional Unavailable reason.
            // The ignored flag gates execution via libtest-mimic's --ignored /
            // --include-ignored; when set, the real body still runs if those
            // flags are passed.
            let (ignored_flag, unavailable_reason) = if !matches!(def.ignore, Ignore::No) {
                // Statically ignored: don't evaluate preconditions and don't
                // add to the Unavailable report.
                (true, None)
            } else {
                let fixture_requires = collect_fixture_requires(def.fixture_names);
                let reasons: Vec<String> = def
                    .requires
                    .iter()
                    .chain(fixture_requires)
                    .filter_map(|req| req.eval().err())
                    .collect();
                if reasons.is_empty() {
                    (false, None)
                } else {
                    (true, Some(reasons.join("; ")))
                }
            };

            // Ignored/unavailable tests never call coordinate() under a
            // normal run — including them here would over-serialize real
            // tests that only "conflict" through this never-executed one.
            if !ignored_flag {
                // Canonicalize via the same to_storage() the coordination DB uses, so
                // the JSON dump's serial_filter matches the DB's canonical-form
                // invariant instead of leaking a raw, possibly-mixed-case declaration
                // (e.g. `serial = FAST` from a user-declared label identifier).
                metadata.push(NextestTestMetadata {
                    name: trial_name.clone(),
                    labels: resolved.iter().map(|l| l.name().to_string()).collect(),
                    serial_filter: crate::coordination::to_storage(&effective_serial),
                });
            }

            trials.push(build_inventory_trial(
                trial_name.clone(),
                resolved.clone(),
                effective_serial,
                def.body,
                capture,
                ignored_flag,
            ));

            if let Some(reason) = unavailable_reason {
                unavailable.push((trial_name, reason));
            }
        }
    }

    pub(crate) fn collect_dynamic_tests(
        self,
        label_filter: Option<&LabelFilter>,
        capture: bool,
        trials: &mut Vec<Trial>,
        metadata: &mut Vec<NextestTestMetadata>,
    ) {
        for dyn_test in self.dynamic {
            if let Some(filter) = label_filter {
                if !filter.matches(&dyn_test.labels) {
                    continue;
                }
            }

            let body = dyn_test.body;
            let serial = dyn_test.serial;
            let labels = dyn_test.labels;
            // Intentional leak: dynamic test names need 'static lifetime for enter_test_scope.
            // Acceptable because the harness runs once per process.
            let name_static: &'static str = Box::leak(dyn_test.name.into_boxed_str());

            if !dyn_test.ignored {
                // Canonicalize via the same to_storage() the coordination DB uses, so
                // the JSON dump's serial_filter matches the DB's canonical-form
                // invariant instead of leaking a raw declaration string.
                metadata.push(NextestTestMetadata {
                    name: name_static.to_string(),
                    labels: labels.iter().map(|l| l.name().to_string()).collect(),
                    serial_filter: crate::coordination::to_storage(&serial),
                });
            }

            trials.push(
                Trial::test(name_static, move || {
                    run_with_observability(name_static, capture, &serial, &labels, move || {
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
