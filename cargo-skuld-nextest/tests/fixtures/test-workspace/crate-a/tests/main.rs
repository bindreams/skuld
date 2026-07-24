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
    // Widens this test's own process lifetime so an accidental overlap is
    // reliably observable — not used to wait for or synchronize with the
    // other process.
    std::thread::sleep(std::time::Duration::from_millis(200));
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
