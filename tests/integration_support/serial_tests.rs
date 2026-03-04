//! Tests for the `serial` flag on tests and fixtures.

use std::sync::atomic::{AtomicU32, Ordering};

static EXPLICIT_SERIAL_RAN: AtomicU32 = AtomicU32::new(0);

#[skuld::test(serial)]
fn explicit_serial_test() {
    EXPLICIT_SERIAL_RAN.fetch_add(1, Ordering::Relaxed);
}

pub fn assert_all_ran() {
    assert_eq!(
        EXPLICIT_SERIAL_RAN.load(Ordering::Relaxed),
        1,
        "explicit_serial_test should have run"
    );
}
