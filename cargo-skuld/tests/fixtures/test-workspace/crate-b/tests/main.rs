#[skuld::label]
pub const SHARED: skuld::Label;
#[skuld::label]
pub const WEIRDRES: skuld::Label;

fn now_nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
}

fn record_process_window(dir: &std::path::Path, name: &str) -> impl FnOnce() {
    std::fs::write(dir.join(format!("{name}-proc-start")), now_nanos().to_string()).expect("write start marker");
    let dir = dir.to_path_buf();
    let name = name.to_string();
    move || {
        std::fs::write(dir.join(format!("{name}-proc-end")), now_nanos().to_string()).expect("write end marker");
    }
}

#[skuld::test(serial = SHARED)]
fn b_locks_shared_resource() {
    if std::env::var_os("SKULD_NEXTEST_FIXTURE_HANDSHAKE").is_some() {
        // Real synchronization, not a guessed duration — see crate-a's
        // a_uses_shared_resource for why.
        use std::io::{Read, Write};
        let mut stdout = std::io::stdout();
        stdout.write_all(b"R").expect("signal ready");
        stdout.flush().expect("flush ready signal");
        let mut release = [0u8; 1];
        std::io::stdin().read_exact(&mut release).expect("wait for release signal");
    } else {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[skuld::test]
fn b_independent() {}

fn main() {
    let timing_dir = std::env::var("SKULD_NEXTEST_FIXTURE_TIMING_DIR").ok().map(std::path::PathBuf::from);
    let args: Vec<String> = std::env::args().collect();

    let end_marker = timing_dir.as_deref().and_then(|dir| {
        args.iter()
            .any(|a| a == "b_locks_shared_resource")
            .then(|| record_process_window(dir, "b_locks_shared_resource"))
    });

    let mut runner = skuld::TestRunner::new();
    let timing_dir_for_weird = timing_dir.clone();
    runner.add("weird & |!~ [b] test", &[WEIRDRES], false, move || {
        if let Some(dir) = &timing_dir_for_weird {
            std::fs::write(dir.join("weird-b-ran"), b"").expect("write weird-b marker");
        }
    });
    let conclusion = runner.run_tests();

    if let Some(end) = end_marker {
        end();
    }
    conclusion.exit();
}
