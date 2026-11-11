//! PASS/FAIL/INFO lines and the FAIL tally behind the exit code.

use std::sync::atomic::{AtomicUsize, Ordering};

static FAILS: AtomicUsize = AtomicUsize::new(0);

pub fn check(scenario: &str, ok: bool, what: impl std::fmt::Display) {
    if !ok {
        FAILS.fetch_add(1, Ordering::Relaxed);
    }
    println!("[{}] {scenario}: {what}", if ok { "PASS" } else { "FAIL" });
}

pub fn info(scenario: &str, what: impl std::fmt::Display) {
    println!("[INFO] {scenario}: {what}");
}

pub fn fails() -> usize {
    FAILS.load(Ordering::Relaxed)
}
