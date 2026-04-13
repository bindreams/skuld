//! Tests for label inheritance via `default_labels!` and explicit opt-out.

use std::sync::atomic::{AtomicU32, Ordering};

skuld::new_label!(SMOKE, "smoke");
skuld::new_label!(UNIT, "unit");
skuld::new_label!(CUSTOM, "custom");

// Exercise get_label! — references "smoke" defined above.
skuld::get_label!(SMOKE_ALIAS, "smoke");

// Set default labels for this module.
skuld::default_labels!(SMOKE, UNIT);

static INHERITED_RAN: AtomicU32 = AtomicU32::new(0);
static EXPLICIT_RAN: AtomicU32 = AtomicU32::new(0);
static OPTOUT_RAN: AtomicU32 = AtomicU32::new(0);

/// No explicit labels → inherits [smoke, unit] from `default_labels!`.
#[skuld::test]
fn label_inherited() {
    INHERITED_RAN.fetch_add(1, Ordering::Relaxed);
}

/// Explicit labels → gets [custom], NOT [smoke, unit, custom].
#[skuld::test(labels = [CUSTOM])]
fn label_explicit() {
    EXPLICIT_RAN.fetch_add(1, Ordering::Relaxed);
}

/// Explicit empty labels → gets nothing, opts out of defaults.
#[skuld::test(labels = [])]
fn label_optout() {
    OPTOUT_RAN.fetch_add(1, Ordering::Relaxed);
}

/// Verify that get_label! produces a Label equal to the new_label! original.
#[skuld::test]
fn get_label_equals_new_label() {
    assert_eq!(SMOKE_ALIAS, SMOKE);
    assert_eq!(SMOKE_ALIAS.name(), "smoke");
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
