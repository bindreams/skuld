//! End-to-end test verifying every trial runs on its own, correctly-named
//! thread that is not the main thread. Out-of-harness because it observes
//! real OS thread identity across a subprocess run.

use std::{fs, process::Command};

#[test]
fn each_trial_runs_on_its_own_thread() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thread_per_trial"));
    cmd.arg("--test-threads=1");
    cmd.env("THREAD_PER_TRIAL_MARKERS", dir.path());
    let out = cmd.output().expect("spawn thread_per_trial");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let read = |name: &str| -> (String, String) {
        let path = dir.path().join(name);
        let contents = fs::read_to_string(&path).unwrap_or_else(|e| panic!("marker {path:?}: {e}"));
        let mut lines = contents.lines();
        let id = lines.next().unwrap_or_default().to_string();
        let thread_name = lines.next().unwrap_or_default().to_string();
        (id, thread_name)
    };

    let (main_id, _) = read("main");
    let (id_one, name_one) = read("trial_one");
    let (id_two, name_two) = read("trial_two");

    assert_ne!(id_one, main_id, "trial_one must not run on the main thread");
    assert_ne!(id_two, main_id, "trial_two must not run on the main thread");
    assert_ne!(id_one, id_two, "trial_one and trial_two must run on different threads");
    assert_eq!(
        name_one, "trial_one",
        "trial_one's thread must be named after its trial"
    );
    assert_eq!(
        name_two, "trial_two",
        "trial_two's thread must be named after its trial"
    );
}
