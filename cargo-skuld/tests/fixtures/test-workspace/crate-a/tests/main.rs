#[skuld::label]
pub const SHARED: skuld::Label;

/// Appends one line to the timing dir's shared order log. Both crate-a's and
/// crate-b's processes append to the SAME file, so its byte order is a real
/// happens-before relation between the two processes — no synchronized or
/// monotonic clock required. This relies on a single `write_all` per line
/// under `O_APPEND`: POSIX guarantees a write at or under `PIPE_BUF` is
/// atomic, so two processes' lines can never interleave into a corrupt line,
/// and each process's own lines always keep their relative order.
fn append_order_log(dir: &std::path::Path, line: &str) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("order.log"))
        .expect("open shared order log");
    file.write_all(line.as_bytes()).expect("append order log line");
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
    // Widen this test's own process lifetime so an accidental overlap would
    // be observable. The deterministic path used by
    // negative_control_two_directly_spawned_processes_overlap is a
    // stdin/stdout handshake in main(), bracketing run_tests() — not here:
    // a handshake in the test body itself would need both processes alive
    // *inside* the body simultaneously, which is exactly what skuld's own
    // SHARED/serial coordination between this test and crate-b's
    // b_locks_shared_resource is designed to prevent.
    std::thread::sleep(std::time::Duration::from_millis(200));
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
        // Real synchronization, not a guessed duration: used only by
        // negative_control_two_directly_spawned_processes_overlap (see its
        // doc), which spawns this binary directly and needs a genuine
        // guarantee that this process and its counterpart in crate-b were
        // alive at the same instant. Bracketing run_tests() (rather than
        // putting this inside a_uses_shared_resource's body) means the
        // driver's confirmation that both processes are ready happens
        // before either process starts running actual tests at all, so it
        // can never race skuld's own cross-process test coordination.
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
        use std::io::Read;
        let mut release = [0u8; 1];
        std::io::stdin().read_exact(&mut release).expect("wait for release signal");
    }

    if let Some(end) = end_marker {
        end();
    }
    conclusion.exit();
}
