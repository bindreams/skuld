#[skuld::label]
pub const SHARED: skuld::Label;
#[skuld::label]
pub const WEIRDRES: skuld::Label;

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
/// lifetime, not the test body — see crate-a's copy of this function for why.
fn record_process_window(dir: &std::path::Path, name: &str) -> impl FnOnce() {
    append_order_log(dir, &format!("start {name}\n"));
    let dir = dir.to_path_buf();
    let name = name.to_string();
    move || {
        append_order_log(&dir, &format!("end {name}\n"));
    }
}

#[skuld::test(serial = SHARED)]
fn b_locks_shared_resource() {
    // Widen this test's own process lifetime so an accidental overlap would
    // be observable — see crate-a's a_uses_shared_resource for why the
    // deterministic handshake lives in main() instead of here.
    std::thread::sleep(std::time::Duration::from_millis(200));
}

#[skuld::test]
fn b_independent() {}

fn main() {
    let timing_dir = std::env::var("SKULD_NEXTEST_FIXTURE_TIMING_DIR").ok().map(std::path::PathBuf::from);
    let handshake = std::env::var_os("SKULD_NEXTEST_FIXTURE_HANDSHAKE").is_some();
    let args: Vec<String> = std::env::args().collect();

    let end_marker = timing_dir.as_deref().and_then(|dir| {
        args.iter()
            .any(|a| a == "b_locks_shared_resource")
            .then(|| record_process_window(dir, "b_locks_shared_resource"))
    });

    if handshake {
        // See crate-a's main() for why this brackets run_tests() rather
        // than living inside b_locks_shared_resource's body.
        use std::io::Write;
        let mut stdout = std::io::stdout();
        stdout.write_all(b"R").expect("signal ready");
        stdout.flush().expect("flush ready signal");
    }

    let mut runner = skuld::TestRunner::new();
    let timing_dir_for_weird = timing_dir.clone();
    runner.add("weird & |!~ [b] test", &[WEIRDRES], false, move || {
        if let Some(dir) = &timing_dir_for_weird {
            std::fs::write(dir.join("weird-b-ran"), b"").expect("write weird-b marker");
        }
    });
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
