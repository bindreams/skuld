//! Regression tests for skuld's output capture redesign.
//!
//! These tests exercise the FD-level capture path from inside a running
//! skuld harness. For the (a) subprocess-level behaviors that cannot be
//! observed from within a single run — "captured bytes really are
//! suppressed on pass" and "`--nocapture` really disables capture" —
//! see `tests/capture_cli.rs` which spawns the `capture_fixture` binary
//! with specific CLI flags.
//!
//! The most load-bearing test here is [`tracing_subscriber_installed_by_test`]:
//! it reproduces bindreams/hole#196's failure mode (a test installs its
//! own `tracing_subscriber::registry().try_init()` global default, emits
//! a `tracing::info!` and a `log::info!` via `tracing-log::LogTracer`,
//! and asserts both reach its own vec-backed writer). This test FAILS on
//! the pre-fix skuld@5c0b636 because skuld's own thread-local `set_default`
//! subscriber intercepts the events. It PASSES on post-fix skuld because
//! skuld no longer touches the tracing dispatch path.

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

// Execution trackers ===================================================================================

static PASSES_QUIETLY_RAN: AtomicBool = AtomicBool::new(false);
static PASSING_WITH_EPRINTLN_RAN: AtomicBool = AtomicBool::new(false);
static FAILING_WITH_EPRINTLN_RAN: AtomicBool = AtomicBool::new(false);
static FAILING_WITH_PRINTLN_RAN: AtomicBool = AtomicBool::new(false);
static FAILING_WITH_RAW_WRITE_RAN: AtomicBool = AtomicBool::new(false);
static TRACING_REGRESSION_RAN: AtomicBool = AtomicBool::new(false);
static SUBPROCESS_OUTPUT_RAN: AtomicBool = AtomicBool::new(false);
static SHOULD_PANIC_WITH_MARKER_RAN: AtomicBool = AtomicBool::new(false);
static ASYNC_CAPTURE_RAN: AtomicBool = AtomicBool::new(false);
static ASYNC_LARGE_WRITE_RAN: AtomicBool = AtomicBool::new(false);
static ASYNC_LARGE_WRITE_BYTES: AtomicUsize = AtomicUsize::new(0);

// Test 1 — baseline silent pass =======================================================================

#[skuld::test]
fn passes_quietly() {
    PASSES_QUIETLY_RAN.store(true, Ordering::Relaxed);
}

// Test 2 — passing test with eprintln ==================================================================

#[skuld::test]
fn passing_with_eprintln_noise() {
    eprintln!("noise-2f: this should be captured on pass and discarded");
    PASSING_WITH_EPRINTLN_RAN.store(true, Ordering::Relaxed);
}

// Test 3 — failing-but-expected with eprintln ==========================================================
//
// Uses `should_panic` so the panic is eaten by the skuld test body
// wrapper before it reaches the runner. This is enough to exercise the
// capture/restore plumbing; the *dump on failure* behavior is verified
// by the subprocess tests in capture_cli.rs (which spawn a real failing
// test and grep the outer captured stderr for the diagnostic marker).

#[skuld::test(should_panic = "expected-panic-3f")]
fn failing_with_eprintln_and_panic() {
    eprintln!("diagnostic-marker-3f");
    FAILING_WITH_EPRINTLN_RAN.store(true, Ordering::Relaxed);
    panic!("expected-panic-3f");
}

// Test 4 — failing with println =======================================================================

#[skuld::test(should_panic = "expected-panic-4f")]
fn failing_with_println() {
    println!("diagnostic-marker-4f");
    FAILING_WITH_PRINTLN_RAN.store(true, Ordering::Relaxed);
    panic!("expected-panic-4f");
}

// Test 5 — failing with raw io::stderr().write_all ====================================================
//
// Bypasses print!/eprint! macros entirely. On libtest's nightly
// set_output_capture this would be missed; our FD-level capture
// catches it.

#[skuld::test(should_panic = "expected-panic-5f")]
fn failing_with_raw_write() {
    std::io::stderr()
        .write_all(b"raw-bytes-5f\n")
        .expect("raw write to stderr");
    FAILING_WITH_RAW_WRITE_RAN.store(true, Ordering::Relaxed);
    panic!("expected-panic-5f");
}

// Test 6+7 — hole#196 regression (tracing + log via bridge) ===========================================
//
// Installs a global default tracing subscriber via
// `registry().try_init()` — this internally calls `set_global_default`.
// Emits both a `tracing::info!` and a `log::info!` (bridged to tracing
// via `LogTracer::init()`), and asserts both reach the test-installed
// subscriber's vec-backed writer.
//
// Verified to FAIL on skuld@5c0b636 (the pre-fix revision) because that
// skuld installs a thread-local `set_default` with filter `off`, which
// takes precedence over the user's `set_global_default` on the test
// thread and swallows the events. To PASS, skuld must not install any
// subscriber in the test's dispatch path.
//
// `serial` because tracing's global default is process-wide state and
// would interfere with (and be interfered with by) other tests that
// install subscribers. Since we are the only test in this suite that
// installs a global, `serial` is formally unnecessary under
// single-threaded capture mode, but we declare it for intent-clarity.

/// Vec-backed `MakeWriter` that appends every write to a shared buffer.
#[derive(Clone)]
struct VecWriter(Arc<Mutex<Vec<u8>>>);

impl<'a> MakeWriter<'a> for VecWriter {
    type Writer = VecWriterGuard<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        VecWriterGuard(self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

struct VecWriterGuard<'a>(std::sync::MutexGuard<'a, Vec<u8>>);

impl std::io::Write for VecWriterGuard<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Shared buffer for the regression test. Static so the test body can
/// reach it from within the `#[skuld::test]`-wrapped closure and still
/// read the contents through the top-level `assert_all_ran`.
fn regression_buf() -> &'static Arc<Mutex<Vec<u8>>> {
    static BUF: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    BUF.get_or_init(|| Arc::new(Mutex::new(Vec::new())))
}

#[skuld::test(serial)]
fn tracing_subscriber_installed_by_test() {
    let buf = regression_buf().clone();
    let writer = VecWriter(buf.clone());

    // Install the log→tracing bridge first so `log::info!` calls are
    // translated into tracing events and flow through the subscriber
    // we're about to install. `LogTracer::init` can only succeed once
    // per process; the test suite has no other caller, so this should
    // be fine.
    //
    // NOTE: If this call returns Err, it means something else in the
    // test binary already installed a log logger. We treat that as a
    // fatal setup error for this test — the assertion below would be
    // meaningless.
    tracing_log::LogTracer::init().expect("LogTracer::init failed — another log logger already installed");

    // Install the tracing subscriber. `registry().with(layer).try_init()`
    // calls `set_global_default` internally. Before the capture redesign,
    // skuld's own thread-local `set_default` would win over this global
    // on the test thread and swallow all events. After the redesign,
    // skuld installs nothing and this subscriber is the actual default.
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(writer).with_ansi(false))
        .try_init()
        .expect("tracing subscriber try_init failed");

    tracing::info!("tracing-marker-6f");
    log::info!("log-marker-7f");

    // Give the subscriber a moment to flush anything async — in practice
    // `fmt::layer()` is synchronous, so no delay is needed.
    let contents = {
        let guard = buf.lock().expect("buf lock");
        String::from_utf8(guard.clone()).expect("utf-8 captured bytes")
    };

    assert!(
        contents.contains("tracing-marker-6f"),
        "user-installed subscriber did not receive tracing::info! event.\n\
         Captured bytes:\n{contents}"
    );
    assert!(
        contents.contains("log-marker-7f"),
        "user-installed subscriber did not receive log::info! event via LogTracer.\n\
         Captured bytes:\n{contents}"
    );

    TRACING_REGRESSION_RAN.store(true, Ordering::Relaxed);
}

// Test 8 — subprocess output capture ==================================================================
//
// The test body spawns a child process that writes to its own stdout
// and stderr. Under FD-level capture, the child inherits fds 1 and 2
// (which point at our pipe), so its output ends up in skuld's capture
// buffer. We cannot directly observe the buffer from inside the harness
// (it's private and dumped to real stderr only on panic), but we can
// assert:
//   (a) the subprocess ran and returned successfully,
//   (b) our own `eprintln!` inside the test body did not race or
//       deadlock with the child's output.
//
// The subprocess tests in capture_cli.rs provide the end-to-end proof
// that the child's bytes actually land in the outer `---- captured ----`
// section.

#[skuld::test]
fn subprocess_output_inherits_capture() {
    let status = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/C", "echo child-stdout-8f & echo child-stderr-8f 1>&2"])
            .status()
    } else {
        std::process::Command::new("sh")
            .args(["-c", "echo child-stdout-8f; echo child-stderr-8f >&2"])
            .status()
    };
    let status = status.expect("spawn subprocess");
    assert!(status.success(), "child process failed: {status:?}");

    // Also emit from the parent. Both streams should end up in skuld's
    // capture buffer if capture is on; they should appear live on the
    // terminal if --nocapture is set.
    eprintln!("parent-stderr-8f");
    println!("parent-stdout-8f");

    SUBPROCESS_OUTPUT_RAN.store(true, Ordering::Relaxed);
}

// Test 9 — #[should_panic] with diagnostic ============================================================

#[skuld::test(should_panic = "expected-9f")]
fn should_panic_with_captured_diagnostic() {
    eprintln!("should-panic-marker-9f");
    SHOULD_PANIC_WITH_MARKER_RAN.store(true, Ordering::Relaxed);
    panic!("expected-9f");
}

// Test 10 — async should_panic with captured output ===================================================

#[skuld::test(should_panic = "expected-10f")]
async fn async_capture_test() {
    tokio::task::yield_now().await;
    eprintln!("async-marker-10f");
    ASYNC_CAPTURE_RAN.store(true, Ordering::Relaxed);
    panic!("expected-10f");
}

// Test 11 — async with 128 KiB write (reader thread deadlock check) ===================================
//
// The FD capture pipe has a fixed kernel buffer (~64 KiB on Linux).
// Without a concurrent reader thread draining the pipe, writes larger
// than the buffer would block on `write` and deadlock the test.
// Writing 128 KiB in a single `write_all` call exercises the
// drain-concurrently guarantee.

#[skuld::test(should_panic = "expected-11f")]
async fn async_large_write() {
    tokio::task::yield_now().await;
    let blob = vec![b'x'; 128 * 1024];
    let n = blob.len();
    std::io::stderr().write_all(&blob).expect("large blob write to stderr");
    ASYNC_LARGE_WRITE_BYTES.store(n, Ordering::Relaxed);
    ASYNC_LARGE_WRITE_RAN.store(true, Ordering::Relaxed);
    panic!("expected-11f");
}

// Post-run assertions ================================================================================

/// Called from `tests/integration.rs` after `TestRunner::run_tests()`
/// completes. Verifies each capture test actually ran (execution was
/// not silently skipped, e.g., if the `#[skuld::test]` macro expansion
/// broke in some way).
pub fn assert_all_ran() {
    assert!(
        PASSES_QUIETLY_RAN.load(Ordering::Relaxed),
        "passes_quietly should have executed"
    );
    assert!(
        PASSING_WITH_EPRINTLN_RAN.load(Ordering::Relaxed),
        "passing_with_eprintln_noise should have executed"
    );
    assert!(
        FAILING_WITH_EPRINTLN_RAN.load(Ordering::Relaxed),
        "failing_with_eprintln_and_panic should have executed"
    );
    assert!(
        FAILING_WITH_PRINTLN_RAN.load(Ordering::Relaxed),
        "failing_with_println should have executed"
    );
    assert!(
        FAILING_WITH_RAW_WRITE_RAN.load(Ordering::Relaxed),
        "failing_with_raw_write should have executed"
    );
    assert!(
        TRACING_REGRESSION_RAN.load(Ordering::Relaxed),
        "tracing_subscriber_installed_by_test (hole#196 regression) should have executed"
    );
    assert!(
        SUBPROCESS_OUTPUT_RAN.load(Ordering::Relaxed),
        "subprocess_output_inherits_capture should have executed"
    );
    assert!(
        SHOULD_PANIC_WITH_MARKER_RAN.load(Ordering::Relaxed),
        "should_panic_with_captured_diagnostic should have executed"
    );
    assert!(
        ASYNC_CAPTURE_RAN.load(Ordering::Relaxed),
        "async_capture_test should have executed"
    );
    assert!(
        ASYNC_LARGE_WRITE_RAN.load(Ordering::Relaxed),
        "async_large_write should have executed"
    );
    assert_eq!(
        ASYNC_LARGE_WRITE_BYTES.load(Ordering::Relaxed),
        128 * 1024,
        "async_large_write should have written 128 KiB"
    );
}
