//! Tests for label inheritance via `default_labels!` and explicit opt-out.

use std::sync::atomic::{AtomicU32, Ordering};

// Set default labels for this module.
skuld::default_labels!(smoke, unit);

static INHERITED_RAN: AtomicU32 = AtomicU32::new(0);
static EXPLICIT_RAN: AtomicU32 = AtomicU32::new(0);
static OPTOUT_RAN: AtomicU32 = AtomicU32::new(0);

/// No explicit labels → inherits [smoke, unit] from `default_labels!`.
#[skuld::test]
fn label_inherited() {
    INHERITED_RAN.fetch_add(1, Ordering::Relaxed);
}

/// Explicit labels → gets [custom], NOT [smoke, unit, custom].
#[skuld::test(labels = [custom])]
fn label_explicit() {
    EXPLICIT_RAN.fetch_add(1, Ordering::Relaxed);
}

/// Explicit empty labels → gets nothing, opts out of defaults.
#[skuld::test(labels = [])]
fn label_optout() {
    OPTOUT_RAN.fetch_add(1, Ordering::Relaxed);
}

pub fn assert_all_ran() {
    assert_eq!(
        INHERITED_RAN.load(Ordering::Relaxed),
        1,
        "label_inherited should have run"
    );
    assert_eq!(
        EXPLICIT_RAN.load(Ordering::Relaxed),
        1,
        "label_explicit should have run"
    );
    assert_eq!(OPTOUT_RAN.load(Ordering::Relaxed), 1, "label_optout should have run");
}
