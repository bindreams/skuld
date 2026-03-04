//! Tests for the `cwd` fixture (CwdGuard).

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use skuld::CwdGuard;

static CWD_SET_RAN: AtomicU32 = AtomicU32::new(0);
static CWD_BACK_RAN: AtomicU32 = AtomicU32::new(0);

#[skuld::test]
fn cwd_set_changes_directory(#[fixture] cwd: &CwdGuard, #[fixture(temp_dir)] dir: &Path) {
    cwd.set(dir);
    let current = std::env::current_dir().unwrap();
    assert_eq!(current, dir, "cwd.set should change the working directory");
    CWD_SET_RAN.fetch_add(1, Ordering::Relaxed);
}

#[skuld::test]
fn cwd_back_returns_to_previous(
    #[fixture] cwd: &CwdGuard,
    #[fixture(temp_dir)] d1: &Path,
    #[fixture(temp_dir)] d2: &Path,
) {
    let original = std::env::current_dir().unwrap();
    cwd.set(d1);
    cwd.set(d2);
    assert_eq!(std::env::current_dir().unwrap(), d2);
    cwd.back();
    assert_eq!(std::env::current_dir().unwrap(), d1, "back() should return to d1");
    cwd.back();
    assert_eq!(
        std::env::current_dir().unwrap(),
        original,
        "second back() should return to original"
    );
    CWD_BACK_RAN.fetch_add(1, Ordering::Relaxed);
}

pub fn assert_all_ran_and_reverted(original_cwd: &Path) {
    assert_eq!(
        CWD_SET_RAN.load(Ordering::Relaxed),
        1,
        "cwd_set_changes_directory should have run"
    );
    assert_eq!(
        CWD_BACK_RAN.load(Ordering::Relaxed),
        1,
        "cwd_back_returns_to_previous should have run"
    );
    let current = std::env::current_dir().unwrap();
    assert_eq!(
        current, original_cwd,
        "CwdGuard should have reverted the working directory"
    );
}
