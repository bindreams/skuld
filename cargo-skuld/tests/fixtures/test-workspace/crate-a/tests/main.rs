#[skuld::label]
pub const SHARED: skuld::Label;

fn now_nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
}

/// Writes the process's start time now; returns a closure the caller runs
/// just before exiting to write the end time. Spans the WHOLE PROCESS
/// lifetime, not the test body: a body-scoped window can't distinguish
/// "nextest never launched the second process concurrently" from "nextest
/// launched both and the second blocked inside coordinate()", because
/// skuld's own runtime coordination already masks that difference at the
/// test-body level.
fn record_process_window(dir: &std::path::Path, name: &str) -> impl FnOnce() {
    std::fs::write(dir.join(format!("{name}-proc-start")), now_nanos().to_string()).expect("write start marker");
    let dir = dir.to_path_buf();
    let name = name.to_string();
    move || {
        std::fs::write(dir.join(format!("{name}-proc-end")), now_nanos().to_string()).expect("write end marker");
    }
}

#[skuld::test(labels = [SHARED])]
fn a_uses_shared_resource() {
    if std::env::var_os("SKULD_NEXTEST_FIXTURE_HANDSHAKE").is_some() {
        // Real synchronization, not a guessed duration: used only by
        // negative_control_two_directly_spawned_processes_overlap (see its
        // doc), which spawns this binary directly and needs a genuine
        // guarantee that this process and its counterpart in crate-b were
        // alive at the same instant, not a hope that a fixed sleep was
        // long enough for two independently-scheduled spawns to line up.
        use std::io::{Read, Write};
        let mut stdout = std::io::stdout();
        stdout.write_all(b"R").expect("signal ready");
        stdout.flush().expect("flush ready signal");
        let mut release = [0u8; 1];
        std::io::stdin().read_exact(&mut release).expect("wait for release signal");
    } else {
        // Normal path (a real `nextest run`, or standalone testing, with
        // no driver on the other end of stdin/stdout to shake hands
        // with): just widen this test's own process lifetime so an
        // accidental overlap would be observable.
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[skuld::test]
fn a_independent() {}

fn main() {
    let timing_dir = std::env::var("SKULD_NEXTEST_FIXTURE_TIMING_DIR").ok().map(std::path::PathBuf::from);
    let args: Vec<String> = std::env::args().collect();

    let end_marker = timing_dir.as_deref().and_then(|dir| {
        args.iter()
            .any(|a| a == "a_uses_shared_resource")
            .then(|| record_process_window(dir, "a_uses_shared_resource"))
    });

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

    if let Some(end) = end_marker {
        end();
    }
    conclusion.exit();
}
