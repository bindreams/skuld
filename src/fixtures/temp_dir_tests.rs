use super::temp_dir::{create_dir, TempDir};
use std::io;
use std::path::Path;

/// Whether `err` names `path`, as written or as `{:?}` formats it.
fn names_path(err: &str, path: &Path) -> bool {
    let debug = format!("{path:?}");
    err.contains(path.to_str().unwrap()) || err.contains(debug.trim_matches('"'))
}

/// Create the directory `path` the way `TempDir` does.
fn create(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return create_dir(path);
    #[cfg(windows)]
    {
        let parent = path.parent().unwrap();
        create_dir(&crate::win_nt::open_dir(parent).unwrap(), parent, path)
    }
}

#[test]
fn a_name_held_by_an_existing_entry_is_already_exists() {
    let parent = TempDir::new().unwrap();
    std::fs::write(parent.join("file"), b"keep").unwrap();
    std::fs::create_dir(parent.join("dir")).unwrap();

    for taken in ["file", "dir"] {
        let err = create(&parent.join(taken)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{taken}: {err}");
    }
    assert_eq!(std::fs::read(parent.join("file")).unwrap(), b"keep");
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
    assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 1);
}

#[test]
fn close_removes_the_directory() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.join("f"), b"").unwrap();
    let path = dir.path().to_path_buf();
    dir.close().unwrap();
    assert!(!path.exists());
}

#[test]
fn debug_names_the_path() {
    let dir = TempDir::new().unwrap();
    assert!(names_path(&format!("{dir:?}"), dir.path()), "{dir:?}");
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn assert_not_root() {
        // SAFETY: geteuid has no preconditions.
        assert_ne!(
            unsafe { libc::geteuid() },
            0,
            "permission tests require a non-root user"
        );
    }

    #[test]
    fn directories_are_private_to_their_owner() {
        let dir = TempDir::new().unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{mode:o}");
    }

    #[test]
    fn close_returns_the_removal_error_naming_the_path() {
        assert_not_root();
        let dir = TempDir::new().unwrap();
        let locked = dir.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("f"), b"").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
        let path = dir.path().to_path_buf();

        let err = dir.close().unwrap_err().to_string();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(&path).unwrap();
        assert!(names_path(&err, &path), "{err}");
    }

    #[test]
    fn a_failed_removal_on_drop_warns_naming_the_path() {
        assert_not_root();
        let warnings = CapturedWarnings::start();
        let dir = TempDir::new().unwrap();
        let locked = dir.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("f"), b"").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
        let path = dir.path().to_path_buf();

        drop(dir);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(&path).unwrap();
        let text = warnings.text();
        assert!(text.contains("could not remove") && names_path(&text, &path), "{text}");
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::win_nt::test_support::mark_for_deletion;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn a_name_held_by_a_delete_pending_directory_is_already_exists() {
        let parent = TempDir::new().unwrap();
        let held = parent.join("held");
        std::fs::create_dir(&held).unwrap();
        let _pending = mark_for_deletion(&held, false);

        let err = create(&held).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
    }

    fn assert_says_being_deleted(err: &str, parent: &Path) {
        assert!(
            err.contains("being deleted") && !err.contains("Access is denied"),
            "the error must name the deletion, not an access denial: {err}"
        );
        assert!(names_path(err, parent), "{err}");
    }

    #[test]
    fn a_parent_being_deleted_says_so() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.join("d");
        std::fs::create_dir(&parent).unwrap();
        let _pending = mark_for_deletion(&parent, false);

        let Err(err) = TempDir::new_in(&parent) else {
            panic!("{parent:?} must be rejected")
        };
        assert_says_being_deleted(&err.to_string(), &parent);
    }

    /// Returning AlreadyExists here would have tempfile retry a name that can never succeed.
    #[test]
    fn a_parent_deleted_after_it_was_opened_says_so() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.join("d");
        std::fs::create_dir(&parent).unwrap();
        let handle = crate::win_nt::open_dir(&parent).unwrap();
        let _pending = mark_for_deletion(&parent, false);

        let err = create_dir(&handle, &parent, &parent.join("p")).unwrap_err();
        assert_ne!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
        assert_says_being_deleted(&err.to_string(), &parent);
    }

    #[test]
    fn close_returns_the_removal_error_naming_the_path() {
        let dir = TempDir::new().unwrap();
        let held = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(dir.join("f"))
            .unwrap();
        let path = dir.path().to_path_buf();

        let err = dir.close().unwrap_err().to_string();
        drop(held);
        std::fs::remove_dir_all(&path).unwrap();
        assert!(names_path(&err, &path), "{err}");
    }

    #[test]
    fn a_failed_removal_on_drop_warns_naming_the_path() {
        let warnings = CapturedWarnings::start();
        let dir = TempDir::new().unwrap();
        let held = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(dir.join("f"))
            .unwrap();
        let path = dir.path().to_path_buf();

        drop(dir);
        drop(held);
        std::fs::remove_dir_all(&path).unwrap();
        let text = warnings.text();
        assert!(text.contains("could not remove") && names_path(&text, &path), "{text}");
    }

    /// `D:rel` is relative to drive D's working directory; tempfile alone would keep it relative.
    #[test]
    fn a_drive_relative_parent_hands_out_an_absolute_path() {
        let holder = TempDir::new_in(".").unwrap();
        let cwd = std::env::current_dir().unwrap();
        let drive = &cwd.to_str().unwrap()[..2];
        assert!(drive.ends_with(':'), "precondition: {cwd:?} has a drive letter");
        let parent = format!("{drive}{}", holder.file_name().unwrap().to_str().unwrap());
        assert!(!Path::new(&parent).is_absolute(), "precondition: {parent}");

        let dir = TempDir::new_in(&parent).unwrap();
        assert!(dir.path().is_absolute(), "{:?}", dir.path());
        assert_eq!(dir.parent(), Some(holder.path()));
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

// Sanitiser -----

use super::temp_dir::{file_name_safe, NAME_MAX, RANDOM_LEN};

#[test]
fn every_forbidden_character_becomes_an_underscore() {
    for c in ['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\u{1}', '\u{7f}'] {
        assert_eq!(file_name_safe(&format!("a{c}b")), "a_b", "{c:?}");
    }
}

/// Whether Win32 reads `name` as a DOS device: the part before the first `.`, trailing spaces
/// trimmed, is a reserved name in any case. An oracle independent of the code under test.
fn is_device_name(name: &str) -> bool {
    let base = name.split('.').next().unwrap().trim_end_matches(' ').to_uppercase();
    let numbered = ["COM", "LPT"].iter().any(|stem| {
        base.strip_prefix(stem)
            .is_some_and(|d| d.chars().count() == 1 && "0123456789¹²³".contains(d))
    });
    numbered || ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&base.as_str())
}

/// The name a prefix gets: `<safe prefix>-<pid>-<random>`.
fn full_name(safe_prefix: &str) -> String {
    format!("{safe_prefix}-{}-{}", std::process::id(), "a".repeat(RANDOM_LEN))
}

#[test]
fn a_prefix_is_escaped_exactly_when_its_full_name_would_be_a_dos_device() {
    for prefix in [
        "nul.json",
        "NUL.x",
        "con .y",
        "COM1.z",
        "LPT9.txt",
        "COM¹.w",
        "lpt³.a",
        "CONIN$.q",
        "conout$.b",
        "prn.a.b",
        "aux",
        "com0",
        "nul",
        "plain.json",
        "console.txt",
        "nul_x.y",
        "com10.z",
        "lpt.w",
        ".nul",
    ] {
        let safe = file_name_safe(prefix);
        assert!(!is_device_name(&full_name(&safe)), "{prefix}: {safe}");
        let escaped = safe != prefix;
        assert_eq!(escaped, is_device_name(&full_name(prefix)), "{prefix}: {safe}");
    }
}

/// The escaped directory must be usable through its path, which Win32 would otherwise resolve to
/// the device.
#[test]
fn a_device_like_prefix_gives_a_usable_directory() {
    let parent = TempDir::new().unwrap();
    let dir = TempDir::with_prefix_in("nul.json", &parent).unwrap();
    assert!(dir.is_dir());
    std::fs::write(dir.join("f"), b"data").unwrap();
    assert_eq!(std::fs::read(dir.join("f")).unwrap(), b"data");
    dir.close().unwrap();
}

/// Whether the created name `name` fits [`NAME_MAX`] as every covered file system counts it:
/// UTF-8 bytes, UTF-16 units, and (HFS+) UTF-16 units after canonical decomposition.
fn assert_fits(name: &str) {
    use unicode_normalization::UnicodeNormalization;

    assert!(name.len() <= NAME_MAX, "{} bytes", name.len());
    assert!(
        name.encode_utf16().count() <= NAME_MAX,
        "{} UTF-16 units",
        name.encode_utf16().count()
    );
    let nfd: String = name.nfd().collect();
    assert!(
        nfd.encode_utf16().count() <= NAME_MAX,
        "{} NFD UTF-16 units",
        nfd.encode_utf16().count()
    );
}

#[test]
fn a_long_prefix_is_cut_so_the_created_name_fits() {
    let parent = TempDir::new().unwrap();
    for prefix in ["x".repeat(1000), "😀".repeat(100), "ΐ".repeat(118)] {
        let dir = TempDir::with_prefix_in(&prefix, &parent).unwrap();
        let name = dir.file_name().unwrap().to_str().unwrap().to_owned();
        let pid_and_random = format!("-{}-", std::process::id());
        let (safe, random) = name.rsplit_once(&pid_and_random).unwrap();
        assert_eq!(random.len(), RANDOM_LEN, "{name}");
        assert!(prefix.starts_with(safe) && !safe.is_empty(), "{name}");
        assert_fits(&name);
    }
}

// Retries -----

#[test]
fn a_taken_name_is_retried_with_a_fresh_name() {
    let parent = TempDir::new().unwrap();
    let mut tried = Vec::new();
    let dir = TempDir::create_with("t", parent.path(), |path| {
        tried.push(path.to_path_buf());
        if tried.len() <= 5 {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "taken"));
        }
        std::fs::create_dir(path)
    })
    .unwrap();
    assert_eq!(tried.len(), 6);
    assert_eq!(dir.path(), tried[5]);
    let mut distinct = tried.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 6, "{tried:?}");
}

#[test]
fn any_other_error_stops_after_one_attempt() {
    let parent = TempDir::new().unwrap();
    let mut attempts = 0;
    let err = TempDir::create_with("t", parent.path(), |_| {
        attempts += 1;
        Err(io::Error::other("the parent is being deleted"))
    })
    .unwrap_err();
    assert_eq!(attempts, 1, "{err}");
}

// Fixture -----

#[test]
fn the_fixture_hands_out_a_canonical_path_for_any_test_name() {
    const NAME: &str = "weird/na:me*?\"<>|.json";
    let _scope = crate::enter_test_scope(NAME, "temp_dir_tests");
    let handle = crate::fixture_get("temp_dir", std::any::TypeId::of::<TempDir>());
    // SAFETY: the temp_dir fixture's value is a TempDir.
    let dir: &TempDir = unsafe { handle.as_ref::<TempDir>() };
    assert!(dir.is_dir());
    assert_eq!(dir.path(), dir.path().canonicalize().unwrap());
    let name = dir.file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with(&format!("{}-", file_name_safe(NAME))), "{name}");
    let path = dir.path().to_path_buf();
    drop(handle);
    assert!(!path.exists(), "{path:?}");
}

// Removal warnings -----

use super::temp_dir::WARNINGS;

/// Captures this thread's removal warnings until dropped.
struct CapturedWarnings;

impl CapturedWarnings {
    fn start() -> Self {
        WARNINGS.set(Some(Vec::new()));
        CapturedWarnings
    }

    fn text(&self) -> String {
        WARNINGS.with_borrow(|w| String::from_utf8_lossy(w.as_deref().unwrap_or_default()).into_owned())
    }
}

impl Drop for CapturedWarnings {
    fn drop(&mut self) {
        WARNINGS.set(None);
    }
}

#[test]
fn close_leaves_nothing_for_drop_to_warn_about() {
    let warnings = CapturedWarnings::start();
    let dir = TempDir::new().unwrap();
    dir.close().unwrap();
    assert_eq!(warnings.text(), "");
}

#[test]
fn a_dropped_directory_is_removed_without_a_warning() {
    let warnings = CapturedWarnings::start();
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    drop(dir);
    assert!(!path.exists());
    assert_eq!(warnings.text(), "");
}
