//! Subject of subprocess invocations in `tests/thread_per_trial_cli.rs`. Not a
//! real product binary.
//!
//! Records thread identity (id + name) for the main thread and for each of
//! two dynamic trials, so the driver can assert trials run on distinct,
//! correctly-named threads that are not the main thread.

use std::{fs, path::PathBuf};

fn marker_dir() -> PathBuf {
    PathBuf::from(std::env::var("THREAD_PER_TRIAL_MARKERS").expect("driver must set THREAD_PER_TRIAL_MARKERS"))
}

fn record(name: &str) {
    let id = format!("{:?}", std::thread::current().id());
    let thread_name = std::thread::current().name().unwrap_or("").to_string();
    let contents = format!("{id}\n{thread_name}\n");
    let path = marker_dir().join(name);
    fs::write(&path, contents).unwrap_or_else(|e| panic!("marker {path:?}: {e}"));
}

fn main() {
    record("main");

    let mut runner = skuld::TestRunner::new();
    runner.add("trial_one", &[], false, || record("trial_one"));
    runner.add("trial_two", &[], false, || record("trial_two"));
    runner.run();
}
