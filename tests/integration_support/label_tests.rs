//! Tests for label inheritance via `default_labels!` and explicit opt-out.

use std::sync::atomic::{AtomicU32, Ordering};

#[skuld::label]
pub const SMOKE: skuld::Label;

/// Doc-commented label. Exercises doc-attr passthrough in the proc macro.
#[skuld::label]
pub const UNIT: skuld::Label;

#[skuld::label]
pub const CUSTOM: skuld::Label;

/// `pub(crate)` visibility passthrough check.
#[skuld::label]
pub(crate) const INTERNAL: skuld::Label;

/// `#[cfg(...)]` passthrough: when cfg sits *after* `#[skuld::label]` it is
/// seen by the macro (inner attrs flow through `attrs`) and forwarded to
/// BOTH the const and the inventory submission, so they compile-in and
/// compile-out together. `#[cfg(test)]` is trivially true in this
/// integration-test binary, but exercises the dual-forward path.
#[skuld::label]
#[cfg(test)]
pub const CFG_PASSTHROUGH: skuld::Label;

// Set default labels for this module.
skuld::default_labels!(SMOKE, UNIT);

static INHERITED_RAN: AtomicU32 = AtomicU32::new(0);
static EXPLICIT_RAN: AtomicU32 = AtomicU32::new(0);
static OPTOUT_RAN: AtomicU32 = AtomicU32::new(0);
static INTERNAL_RAN: AtomicU32 = AtomicU32::new(0);
static SERIAL_MIXED_RAN: AtomicU32 = AtomicU32::new(0);

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

/// Uses the `pub(crate)` label. Proves visibility passthrough.
#[skuld::test(labels = [INTERNAL])]
fn label_pub_crate_visibility() {
    INTERNAL_RAN.fetch_add(1, Ordering::Relaxed);
}

/// `serial = SMOKE` embeds the ident `"SMOKE"` on `TestDef.serial`. Runtime
/// re-parses it case-insensitively into `"smoke"`, matching the canonical
/// label name. Validates the macro-ident ↔ runtime-filter coupling.
#[skuld::test(serial = SMOKE, labels = [CUSTOM])]
fn label_serial_mixed_case() {
    SERIAL_MIXED_RAN.fetch_add(1, Ordering::Relaxed);
}

/// Exercises the `cfg_attr` passthrough path: the label registers in `test`
/// profile but the const also exists regardless. Silent no-op coverage for
/// the attribute forwarding loop.
#[skuld::test(labels = [CFG_PASSTHROUGH])]
fn label_cfg_passthrough() {}

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
    assert_eq!(
        INTERNAL_RAN.load(Ordering::Relaxed),
        1,
        "label_pub_crate_visibility should have run"
    );
    assert_eq!(
        SERIAL_MIXED_RAN.load(Ordering::Relaxed),
        1,
        "label_serial_mixed_case should have run"
    );
}
