#[skuld::label]
pub const SHARED: skuld::Label;

/// Appends one line to the timing dir's shared order log. Both crate-a's and
/// crate-b's processes append to the SAME file, so its byte order is a real
/// happens-before relation between the two processes — no synchronized or
/// monotonic clock required. The guarantee this relies on is `O_APPEND`
/// (Unix) / `FILE_APPEND_DATA` (Windows — what `OpenOptions::append(true)`
/// opens the file with): each individual `write()` call gets its
/// end-of-file seek and its write treated as one atomic kernel operation,
/// so concurrent writers' calls land at distinct, correctly-ordered offsets
/// and one call's bytes can never land in the middle of another's.
/// `PIPE_BUF` (the write()-atomicity size limit for *pipes*) does not apply
/// to regular files and is not what's relied on here.
///
/// That guarantee is per `write()` *call*, not per logical line — which is
/// exactly why this uses a single raw `write()` and asserts the full
/// length was written, instead of `write_all` (which would silently retry
/// with more `write()` calls on a short write, and a second process's line
/// could then land in the gap between them). A short write here is a
/// contract violation, not a recoverable condition: panicking is the only
/// way to know, from a test, that the ordering guarantee itself broke down.
fn append_order_log(dir: &std::path::Path, line: &str) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("order.log"))
        .expect("open shared order log");
    let bytes = line.as_bytes();
    let written = file.write(bytes).expect("append order log line");
    assert_eq!(
        written,
        bytes.len(),
        "short write to the shared order log ({written} of {} bytes) breaks the one-write()-per-line \
         atomicity this file's ordering guarantee depends on",
        bytes.len()
    );
}

/// Appends this process's start line now; returns a closure the caller runs
/// just before exiting to append the end line. Spans the WHOLE PROCESS
/// lifetime, not the test body: a body-scoped window can't distinguish
/// "nextest never launched the second process concurrently" from "nextest
/// launched both and the second blocked inside coordinate()", because
/// skuld's own runtime coordination already masks that difference at the
/// test-body level.
fn record_process_window(dir: &std::path::Path, name: &str) -> impl FnOnce() {
    append_order_log(dir, &format!("start {name}\n"));
    let dir = dir.to_path_buf();
    let name = name.to_string();
    move || {
        append_order_log(&dir, &format!("end {name}\n"));
    }
}

#[skuld::test(labels = [SHARED])]
fn a_uses_shared_resource() {
    // No sleep: the negative control's determinism comes entirely from the
    // stdin/stdout handshake in main() (bracketing run_tests(), not this
    // body — a handshake in the body itself would need both processes
    // alive *inside* the body simultaneously, which is exactly what
    // skuld's own SHARED/serial coordination between this test and
    // crate-b's b_locks_shared_resource is designed to prevent). The
    // positive case's correctness no longer depends on this process's
    // lifetime being artificially widened either — see
    // run_serializes_the_cross_binary_conflict_via_generated_tool_config's
    // doc for why.
}

#[skuld::test]
fn a_independent() {}

fn main() {
    let timing_dir = std::env::var("SKULD_NEXTEST_FIXTURE_TIMING_DIR").ok().map(std::path::PathBuf::from);
    let handshake = std::env::var_os("SKULD_NEXTEST_FIXTURE_HANDSHAKE").is_some();
    let args: Vec<String> = std::env::args().collect();

    let end_marker = timing_dir.as_deref().and_then(|dir| {
        args.iter()
            .any(|a| a == "a_uses_shared_resource")
            .then(|| record_process_window(dir, "a_uses_shared_resource"))
    });

    if handshake {
        // Signals readiness only — does not itself block anything.
        // run_tests() below starts immediately after this write, without
        // waiting for the driver to acknowledge it. What actually makes
        // this safe is the *other* half of the handshake, after
        // run_tests() below: see its comment.
        use std::io::Write;
        let mut stdout = std::io::stdout();
        stdout.write_all(b"R").expect("signal ready");
        stdout.flush().expect("flush ready signal");
    }

    let mut runner = skuld::TestRunner::new();
    runner.add_serial_with(
        "weird test (name) [a] &|!~,end",
        &[],
        false,
        skuld::LabelFilter::parse("weirdres").unwrap(),
        {
            let timing_dir = timing_dir.clone();
            move || {
                if let Some(dir) = &timing_dir {
                    std::fs::write(dir.join("weird-a-ran"), b"").expect("write weird-a marker");
                }
            }
        },
    );
    let conclusion = runner.run_tests();

    if handshake {
        // This is what makes the handshake safe, by coming *after*
        // run_tests(): the end marker below can only be recorded once this
        // read unblocks, and it only unblocks once the driver has written
        // the release byte — which the driver only does after observing
        // the ready signal from BOTH this process and crate-b's. Since the
        // start marker is always recorded before the ready signal is even
        // sent (see record_process_window's call site above), both
        // processes' start markers are therefore guaranteed to already be
        // in the shared order log before either process's end marker can
        // be — regardless of how fast or slow run_tests() itself is.
        use std::io::Read;
        let mut release = [0u8; 1];
        std::io::stdin().read_exact(&mut release).expect("wait for release signal");
    }

    if let Some(end) = end_marker {
        end();
    }
    conclusion.exit();
}
