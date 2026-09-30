use super::temp_dir::{create_in, TempDir};
use std::cell::Cell;
use std::ffi::OsString;
use std::path::Path;

/// Hands out `names` in order and counts the calls; asking for more is a test failure.
fn names<'a>(names: &'a [&'a str], asked: &'a Cell<usize>) -> impl FnMut() -> String + 'a {
    move || {
        let i = asked.get();
        asked.set(i + 1);
        let Some(name) = names.get(i) else {
            panic!("asked for name #{} after {names:?}", i + 1)
        };
        name.to_string()
    }
}

fn listing(dir: &Path) -> Vec<OsString> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    names
}

#[test]
fn a_name_held_by_an_existing_entry_is_skipped() {
    let parent = TempDir::new().unwrap();
    std::fs::write(parent.join("taken"), b"keep").unwrap();

    let asked = Cell::new(0);
    let made = create_in(&parent, names(&["taken", "fresh"], &asked)).unwrap();
    assert_eq!(made, parent.join("fresh"));
    assert!(made.is_dir());
    assert_eq!(asked.get(), 2);
    assert_eq!(std::fs::read(parent.join("taken")).unwrap(), b"keep");
}

#[test]
fn new_directories_are_distinct_and_removed_on_drop() {
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    assert_ne!(a.path(), b.path());
    std::fs::write(a.join("f"), b"").unwrap();
    let path = a.path().to_path_buf();
    drop(a);
    assert!(!path.exists());
    assert!(b.is_dir());
}

#[test]
fn names_embed_the_process_id() {
    let dir = TempDir::new().unwrap();
    let name = dir.file_name().unwrap().to_str().unwrap();
    assert!(name.contains(&format!("-{}-", std::process::id())), "{name}");
}

#[test]
fn new_in_creates_in_the_given_parent() {
    let parent = TempDir::new().unwrap();
    let dir = TempDir::new_in(&parent).unwrap();
    assert_eq!(dir.parent(), Some(parent.path()));
    assert_eq!(listing(&parent).len(), 1);
}

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::win_nt::test_support::mark_for_deletion;

    #[test]
    fn a_name_held_by_a_delete_pending_directory_is_skipped() {
        let parent = TempDir::new().unwrap();
        let held = parent.join("held");
        std::fs::create_dir(&held).unwrap();
        let _pending = mark_for_deletion(&held, false);

        let asked = Cell::new(0);
        let made = create_in(&parent, names(&["held", "fresh"], &asked)).unwrap();
        assert_eq!(made, parent.join("fresh"));
        assert_eq!(asked.get(), 2);
    }

    #[test]
    fn a_parent_being_deleted_says_so() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.join("d");
        std::fs::create_dir(&parent).unwrap();
        let _pending = mark_for_deletion(&parent, false);

        let asked = Cell::new(0);
        let err = create_in(&parent, names(&["p"], &asked)).unwrap_err().to_string();
        assert!(
            err.contains("being deleted") && !err.contains("Access is denied"),
            "the error must name the deletion, not an access denial: {err}"
        );
        assert!(names_path(&err, &parent), "{err}");
    }
}

// Parity with tempfile -----

#[test]
fn a_relative_parent_hands_out_an_absolute_path() {
    let dir = TempDir::new_in(".").unwrap();
    assert!(dir.path().is_absolute(), "{:?}", dir.path());
    let path = dir.path().to_path_buf();
    drop(dir);
    assert!(!path.exists(), "{path:?}");
}

/// Another local user must not be able to plant every next name in a shared parent, so each name
/// ends in tempfile's random suffix rather than a counter.
#[test]
fn names_end_in_a_random_suffix_after_the_pid() {
    let dir = TempDir::new().unwrap();
    let name = dir.file_name().unwrap().to_str().unwrap();
    let marker = format!("-{}-", std::process::id());
    let (_, suffix) = name.rsplit_once(&marker).unwrap_or_else(|| panic!("{name}"));
    assert!(
        suffix.len() == 6 && suffix.chars().all(|c| c.is_ascii_alphanumeric()),
        "{name}"
    );
}

/// Test names are arbitrary strings, and the fixture uses them as the prefix.
#[test]
fn any_prefix_gives_a_directory_directly_in_the_parent() {
    let parent = TempDir::new().unwrap();
    let long = "x".repeat(40_000);
    for prefix in ["a/b", "a\\b", "a:b", "a\u{1}b", long.as_str()] {
        let dir = TempDir::with_prefix_in(prefix, &parent).unwrap_or_else(|e| panic!("{prefix:.20}: {e}"));
        assert_eq!(dir.parent(), Some(parent.path()), "{prefix:.20}");
        assert!(dir.is_dir(), "{prefix:.20}");
        let name = dir.file_name().unwrap().to_str().unwrap();
        assert!(name.encode_utf16().count() <= 255, "{prefix:.20}: {} units", name.len());
    }
}

/// Whether `err` names `path`, as written or as `{:?}` formats it.
fn names_path(err: &str, path: &Path) -> bool {
    let debug = format!("{path:?}");
    err.contains(path.to_str().unwrap()) || err.contains(debug.trim_matches('"'))
}

#[test]
fn a_missing_parent_is_an_error_naming_it() {
    let tmp = TempDir::new().unwrap();
    let parent = tmp.join("nope").join("deeper");
    let Err(err) = TempDir::new_in(&parent) else {
        panic!("{parent:?} must be rejected")
    };
    let err = err.to_string();
    assert!(names_path(&err, &parent), "{err}");
}

#[test]
fn a_parent_that_is_a_file_is_an_error_naming_it() {
    let tmp = TempDir::new().unwrap();
    let parent = tmp.join("file");
    std::fs::write(&parent, b"").unwrap();
    let Err(err) = TempDir::new_in(&parent) else {
        panic!("{parent:?} must be rejected")
    };
    let err = err.to_string();
    assert!(names_path(&err, &parent), "{err}");
}
