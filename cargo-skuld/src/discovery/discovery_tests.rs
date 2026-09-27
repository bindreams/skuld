use super::*;
use crate::test_support::lock_fixture_workspace;

#[test]
fn discovers_binaries_across_a_multi_crate_workspace() {
    // Was pointed at this repo's own root: `cargo nextest list` there
    // races `skuld`'s own `tests/*_cli.rs`, which spawn `CARGO_BIN_EXE_*`
    // paths under the *same* target dir with no lock of their own — on
    // macOS `cargo` deletes and re-creates every flat `target/debug/<bin>`
    // path on every invocation, even this one, which changes nothing.
    // The fixture workspace exercises the same multi-crate discovery
    // under the lock this crate's other fixture-touching tests already
    // hold, without that cross-crate exposure.
    let _guard = lock_fixture_workspace();
    let found = discover_binaries(_guard.root()).expect("cargo nextest list must succeed");
    for name in ["fixture-crate-a", "fixture-crate-b", "fixture-crate-c-plain"] {
        assert!(
            found.iter().any(|b| b.binary_id.contains(name)),
            "expected a binary id containing {name:?}, got {found:?}"
        );
    }
    for b in &found {
        assert!(b.binary_path.exists(), "{:?} does not exist on disk", b.binary_path);
    }
}
