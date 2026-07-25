use super::*;

#[test]
fn discovers_this_workspaces_own_binaries() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let found = discover_binaries(root).expect("cargo nextest list must succeed");
    assert!(
        found.iter().any(|b| b.binary_id.contains("skuld")),
        "expected at least one skuld-related binary id, got {found:?}"
    );
    for b in &found {
        assert!(b.binary_path.exists(), "{:?} does not exist on disk", b.binary_path);
    }
}
