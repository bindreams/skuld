use super::file_identity::FileIdentity;

#[test]
fn of_file_matches_of_path() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();

    let opened_a = std::fs::File::open(&a).unwrap();
    let opened_b = std::fs::File::open(&b).unwrap();

    let id_a = FileIdentity::of_file(&opened_a).unwrap();
    assert_eq!(Some(id_a), FileIdentity::of(&a));
    assert_ne!(Some(id_a), FileIdentity::of(&b));
    assert_eq!(Some(FileIdentity::of_file(&opened_b).unwrap()), FileIdentity::of(&b));
}
