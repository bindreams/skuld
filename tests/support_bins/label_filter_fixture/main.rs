//! Subject of subprocess invocations in `tests/label_filter_cli.rs`. Not a
//! real product binary.
//!
//! Contains a diverse zoo of labeled tests so the driver can set
//! `SKULD_LABELS` to any of a wide range of boolean expressions and
//! verify which tests are collected, ignored, or absent. Each test body
//! writes an empty file `$LABEL_FILTER_FIXTURE_MARKERS/<name>` on entry
//! so the driver can detect which actually ran.

use std::{fs, path::PathBuf};

// Label zoo ==========================================================================================
//
// Every pair of inventory tests is distinguishable by at least one filter
// expression in the matrix — no false positives. FAST_SLOW guards against
// prefix-matching bugs (`fast` must not match `fast_slow`).

#[skuld::label]
pub const FAST: skuld::Label;
#[skuld::label]
pub const SLOW: skuld::Label;
#[skuld::label]
pub const DOCKER: skuld::Label;
#[skuld::label]
pub const INTEGRATION: skuld::Label;
#[skuld::label]
pub const NET: skuld::Label;
#[skuld::label]
pub const DB: skuld::Label;
#[skuld::label]
pub const FAST_SLOW: skuld::Label;

// Precondition helpers (mirroring tests/integration_support/harness_tests.rs).
fn always_ok() -> Result<(), String> {
    Ok(())
}
fn always_fail() -> Result<(), String> {
    Err("unavailable".into())
}

// Marker infrastructure ==============================================================================

fn marker_dir() -> PathBuf {
    PathBuf::from(std::env::var("LABEL_FILTER_FIXTURE_MARKERS").expect("driver must set LABEL_FILTER_FIXTURE_MARKERS"))
}

fn mark(name: &str) {
    let path = marker_dir().join(name);
    fs::write(&path, b"").unwrap_or_else(|e| panic!("marker {path:?}: {e}"));
}

// Top-level tests (no default_labels! in scope) ======================================================

#[skuld::test]
fn t_none() {
    mark("t_none");
}

#[skuld::test(labels = [FAST])]
fn t_fast() {
    mark("t_fast");
}

#[skuld::test(labels = [SLOW])]
fn t_slow() {
    mark("t_slow");
}

#[skuld::test(labels = [FAST, DOCKER])]
fn t_fast_docker() {
    mark("t_fast_docker");
}

#[skuld::test(labels = [SLOW, DOCKER])]
fn t_slow_docker() {
    mark("t_slow_docker");
}

#[skuld::test(labels = [FAST_SLOW])]
fn t_fast_slow_compound() {
    mark("t_fast_slow_compound");
}

#[skuld::test(labels = [FAST, FAST])]
fn t_dup_fast() {
    mark("t_dup_fast");
}

#[skuld::test(labels = [FAST])]
#[ignore]
fn t_outer_ignored_fast() {
    mark("t_outer_ignored_fast");
}

#[skuld::test(labels = [FAST], ignore)]
fn t_native_ignored_fast() {
    mark("t_native_ignored_fast");
}

#[skuld::test(labels = [FAST], requires = [always_ok])]
fn t_req_fast() {
    mark("t_req_fast");
}

#[skuld::test(labels = [FAST], requires = [always_fail])]
fn t_req_unmet_fast() {
    mark("t_req_unmet_fast");
}

#[skuld::test(serial, labels = [FAST])]
fn t_serial_fast() {
    mark("t_serial_fast");
}

#[skuld::test(serial = FAST, labels = [FAST])]
fn t_serial_filter_fast() {
    mark("t_serial_filter_fast");
}

#[skuld::test(labels = [FAST], should_panic)]
fn t_should_panic_fast() {
    mark("t_should_panic_fast");
    panic!("expected");
}

#[skuld::test(labels = [SLOW], should_panic)]
fn t_should_panic_slow() {
    mark("t_should_panic_slow");
    panic!("expected");
}

// Module with default_labels!(INTEGRATION, NET) =====================================================
//
// Marker names are bare fn names (globally unique in this fixture) to match
// libtest-mimic's trial-name convention (`def.display_name.unwrap_or(def.name)`
// at src/runner.rs uses the bare fn ident).

mod inherited {
    use super::{mark, DB, INTEGRATION, NET};

    skuld::default_labels!(INTEGRATION, NET);

    #[skuld::test]
    fn t_default() {
        mark("t_default");
    }

    #[skuld::test(labels = [DB])]
    fn t_explicit_db() {
        mark("t_explicit_db");
    }

    #[skuld::test(labels = [])]
    fn t_optout() {
        mark("t_optout");
    }

    pub mod inherited_nested {
        use super::super::mark;

        #[skuld::test]
        fn t_nested_default() {
            mark("t_nested_default");
        }
    }
}

// Nested module with its own default_labels!(DB) — longest-prefix wins. ============================

mod override_defaults {
    use super::{mark, DB};

    skuld::default_labels!(DB);

    #[skuld::test]
    fn t_override() {
        mark("t_override");
    }
}

// Dynamic tests ======================================================================================

fn main() {
    let mut runner = skuld::TestRunner::new();
    runner.add("dyn_fast", &[FAST], false, || mark("dyn_fast"));
    runner.add("dyn_slow_ignored", &[SLOW], true, || mark("dyn_slow_ignored"));
    runner.add_serial("dyn_serial_fast", &[FAST], false, || mark("dyn_serial_fast"));
    runner.add_serial_with(
        "dyn_serial_filter_fast",
        &[FAST],
        false,
        skuld::LabelFilter::parse("fast").unwrap(),
        || mark("dyn_serial_filter_fast"),
    );
    runner.run();
}
