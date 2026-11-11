//! Tests for [`coordinate`] and [`TestRegistration`]. The registry's own rules are tested in
//! `registry_tests.rs`.

use std::path::{Path, PathBuf};

use super::registry::running_dir;
use super::{coordinate, SERIAL_ALL, SERIAL_NONE};

/// A fresh coordination directory, `<temp>/.skuld`, which does not exist yet.
fn temp_coord() -> (crate::TempDir, PathBuf) {
    let dir = crate::TempDir::new().unwrap();
    let coord = dir.path().join(".skuld");
    (dir, coord)
}

/// The `.json` entries in `running/`, parsed.
fn entries(coord: &Path) -> Vec<serde_json::Value> {
    std::fs::read_dir(running_dir(coord))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
        .collect()
}

/// Delete every entry in `running/`, so the registration made there no longer matches what its
/// owner recorded.
fn delete_entries(coord: &Path) {
    for entry in std::fs::read_dir(running_dir(coord)).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "json") {
            std::fs::remove_file(path).unwrap();
        }
    }
}

// TestRegistration guard =====

#[test]
fn registration_guard_cleans_up_on_drop() {
    let (_dir, coord) = temp_coord();
    {
        let _reg = coordinate(&coord, "guarded_test", &[], SERIAL_NONE);
        assert_eq!(entries(&coord).len(), 1);
    }
    assert!(entries(&coord).is_empty(), "the entry must be gone after drop");
    assert_eq!(std::fs::read_dir(running_dir(&coord)).unwrap().count(), 0);
}

#[test]
fn registration_guard_cleans_up_on_panic() {
    let (_dir, coord) = temp_coord();
    let _ = std::panic::catch_unwind(|| {
        let _reg = coordinate(&coord, "panicking_test", &[], SERIAL_NONE);
        panic!("intentional panic");
    });
    assert_eq!(std::fs::read_dir(running_dir(&coord)).unwrap().count(), 0);
}

// Canonicalization at the storage boundary =====
//
// `coordinate()` collapses redundant and tautological serial filters before publishing them, so
// the registry invariant holds: every stored filter is either `""`, `"*"`, or a canonical
// Display string.

/// The `filter` of the one registered entry in `coord`.
fn stored_filter(coord: &Path) -> String {
    let entries = entries(coord);
    assert_eq!(entries.len(), 1, "expected exactly one registered test");
    entries[0]["filter"].as_str().expect("filter is a string").to_owned()
}

#[test]
fn coordinate_stores_canonical_form_for_redundant_filter() {
    let (_dir, coord) = temp_coord();
    let _reg = coordinate(&coord, "redundant", &[], "(a) | (a)");
    assert_eq!(stored_filter(&coord), "a");
}

#[test]
fn coordinate_collapses_tautology_to_global_serial_sentinel() {
    let (_dir, coord) = temp_coord();
    let _reg = coordinate(&coord, "taut", &[], "a | !a");
    assert_eq!(stored_filter(&coord), SERIAL_ALL);
}

#[test]
fn coordinate_collapses_contradiction_to_non_serial_sentinel() {
    let (_dir, coord) = temp_coord();
    let _reg = coordinate(&coord, "contra", &[], "a & !a");
    assert_eq!(stored_filter(&coord), SERIAL_NONE);
}

#[test]
fn coordinate_preserves_serial_none_sentinel() {
    let (_dir, coord) = temp_coord();
    let _reg = coordinate(&coord, "none", &[], SERIAL_NONE);
    assert_eq!(stored_filter(&coord), SERIAL_NONE);
}

#[test]
fn coordinate_preserves_serial_all_sentinel() {
    let (_dir, coord) = temp_coord();
    let _reg = coordinate(&coord, "all", &[], SERIAL_ALL);
    assert_eq!(stored_filter(&coord), SERIAL_ALL);
}

// Drop-panic-during-unwind downgrade path =====

/// Regression guard for the `&payload` vs `payload.as_ref()` footgun
/// documented on `downgraded_warning_message` in `coordination.rs`. These
/// cases exercise `panic_payload_message`'s extraction logic in isolation —
/// the `.as_ref()` deref already happened before the payload reaches it.
/// `downgraded_warning_message_carries_the_real_panic_message_through_the_box`
/// below, and `drop_panic_during_unwind_cli.rs`'s subprocess test, guard the
/// `&payload` vs `.as_ref()` choice itself.
#[test]
fn panic_payload_message_extracts_a_runtime_formatted_string_payload() {
    let payload = std::panic::catch_unwind(|| {
        let uid = 502;
        panic!("dynamic message uid {uid}");
    })
    .unwrap_err();
    assert_eq!(
        crate::coordination::panic_payload_message(payload.as_ref()),
        "dynamic message uid 502"
    );
}

#[test]
fn panic_payload_message_extracts_a_static_str_payload() {
    let payload = std::panic::catch_unwind(|| {
        panic!("static message");
    })
    .unwrap_err();
    assert_eq!(
        crate::coordination::panic_payload_message(payload.as_ref()),
        "static message"
    );
}

#[test]
fn panic_payload_message_falls_back_on_a_non_string_payload() {
    let payload = std::panic::catch_unwind(|| {
        std::panic::panic_any(42_i32);
    })
    .unwrap_err();
    assert_eq!(
        crate::coordination::panic_payload_message(payload.as_ref()),
        "<non-string panic payload>"
    );
}

/// Exercises the exact call convention `Drop::drop` uses —
/// `downgraded_warning_message(&payload)` — which is what actually
/// regression-guards the footgun documented on `downgraded_warning_message`
/// in `coordination.rs`: passing the un-deref'd `&payload` here would wrongly
/// fall back to `"<non-string panic payload>"` for a real, runtime-formatted
/// string payload.
#[test]
fn downgraded_warning_message_carries_the_real_panic_message_through_the_box() {
    let payload = std::panic::catch_unwind(|| {
        let uid = 502;
        panic!("dynamic message uid {uid}");
    })
    .unwrap_err();

    let msg = crate::coordination::downgraded_warning_message(&payload);

    assert!(
        msg.contains("dynamic message uid 502"),
        "must surface the real panic message through the Box, not the \
         non-string-payload fallback: {msg:?}"
    );
    assert!(
        !msg.contains("<non-string panic payload>"),
        "regression: `&payload` was passed instead of `payload.as_ref()`, so the \
         downcast silently missed and fell back to the placeholder: {msg:?}"
    );
}

/// The downgrade in `TestRegistration::drop` is conditioned on `std::thread::panicking()`: it
/// must fire *only* while the thread is already unwinding from another panic. A normal drop
/// (nothing else unwinding) of a registration whose entry is gone must still panic loudly;
/// downgrading unconditionally would silently swallow every such failure.
#[test]
fn drop_panics_loudly_when_nothing_else_is_unwinding() {
    let (_dir, coord) = temp_coord();
    let reg = coordinate(&coord, "normal_drop_broken_entry", &[], SERIAL_NONE);
    delete_entries(&coord);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(reg)));
    assert!(
        result.is_err(),
        "drop must panic when the registration is broken and no other unwind is already in flight"
    );
}

// Failures recorded during an unwind =====

/// A registration dropped while its thread unwinds cannot panic (that aborts), so its refusal to
/// unregister a broken registration is recorded process-wide for the runner to fail the run with.
#[test]
fn drop_during_an_unwind_records_the_downgraded_failure() {
    let (_dir, coord) = temp_coord();
    let a = coordinate(&coord, "a", &[], SERIAL_NONE);
    delete_entries(&coord);

    let result = std::thread::spawn(move || {
        let _held = a;
        panic!("the test's own failure");
    })
    .join();

    assert!(result.is_err());
    let recorded = crate::coordination::violations::recorded();
    assert!(
        recorded
            .iter()
            .any(|m| m.contains(&format!("{coord:?}")) && m.contains("moved")),
        "the downgraded failure must be recorded with its path and cause: {recorded:?}"
    );
}

// Unusable directory =====

/// A read-only `running/` fails registration at its first write; the message must still point at
/// `SKULD_DB_DIR`.
#[cfg(unix)]
#[test]
fn a_read_only_running_directory_names_the_override() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid has no preconditions.
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "requires a non-root user: root ignores directory modes"
    );
    let (_dir, coord) = temp_coord();
    drop(coordinate(&coord, "creates the directories", &[], SERIAL_NONE));
    let running = running_dir(&coord);
    std::fs::set_permissions(&running, std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = std::panic::catch_unwind(|| coordinate(&coord, "t", &[], SERIAL_NONE));
    std::fs::set_permissions(&running, std::fs::Permissions::from_mode(0o755)).unwrap();
    let payload = result.err().expect("expected a panic");
    let msg = payload.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(msg.contains("SKULD_DB_DIR"), "{msg}");
}
