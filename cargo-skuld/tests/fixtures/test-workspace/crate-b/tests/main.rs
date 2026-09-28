#[skuld::label]
pub const SHARED: skuld::Label;
#[skuld::label]
pub const WEIRDRES: skuld::Label;

/// Appends one line to the timing dir's shared order log — see crate-a's
/// copy of this function for the `O_APPEND`/`FILE_APPEND_DATA`,
/// single-`write()`-call contract this relies on.
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
    // No sleep — see crate-a's a_uses_shared_resource for why.
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
        // Signals readiness only, does not block — see crate-a's main()
        // for why this brackets run_tests() rather than living inside
        // b_locks_shared_resource's body, and what actually makes it safe.
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
        // See crate-a's main() for why coming after run_tests() is what
        // actually makes this safe.
        use std::io::Read;
        let mut release = [0u8; 1];
        std::io::stdin().read_exact(&mut release).expect("wait for release signal");
    }

    if let Some(end) = end_marker {
        end();
    }
    conclusion.exit();
}
