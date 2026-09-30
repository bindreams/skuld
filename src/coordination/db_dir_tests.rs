#[cfg(unix)]
use super::db_dir::create_dir_all_open_with;
use super::db_dir::{has_cargo_marker, layout_dir, resolve};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

fn abs(p: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("C:{p}"))
    } else {
        PathBuf::from(p)
    }
}

// Layout rules (pure) -----

/// A cargo-style hash suffix, present on every test binary in `deps/` and so a positive cargo marker.
const H: &str = "0123456789abcdef";

fn no_marker(_: &Path) -> bool {
    false
}

fn marker(_: &Path) -> bool {
    true
}

fn layout(exe: &str, has_marker: fn(&Path) -> bool) -> Option<PathBuf> {
    layout_dir(&abs(exe), has_marker)
}

#[test]
fn hashed_test_binary_in_deps_resolves_to_the_profile_dir() {
    assert_eq!(
        layout(&format!("/work/target/debug/deps/foo-{H}"), no_marker),
        Some(abs("/work/target/debug"))
    );
    assert_eq!(
        layout(&format!("/work/target/debug/deps/foo-{H}.exe"), no_marker),
        Some(abs("/work/target/debug"))
    );
}

#[test]
fn hashed_name_with_linuxs_deleted_suffix_still_counts_as_hashed() {
    assert_eq!(
        layout(&format!("/p/debug/deps/t-{H} (deleted)"), no_marker),
        Some(abs("/p/debug"))
    );
}

#[test]
fn hashed_example_resolves_to_the_profile_dir() {
    assert_eq!(
        layout(&format!("/w/target/release/examples/ex-{H}"), no_marker),
        Some(abs("/w/target/release"))
    );
}

#[test]
fn deps_without_a_cargo_marker_is_a_plain_directory() {
    assert_eq!(layout("/opt/app/deps/t", no_marker), Some(abs("/opt/app/deps")));
    assert_eq!(layout("/opt/app/examples/t", no_marker), Some(abs("/opt/app/examples")));
}

#[test]
fn deps_with_a_cargo_marker_on_the_profile_dir_steps_up() {
    let seen = std::cell::RefCell::new(Vec::new());
    let got = layout_dir(&abs("/opt/app/deps/t"), |p| {
        seen.borrow_mut().push(p.to_path_buf());
        true
    });
    assert_eq!(got, Some(abs("/opt/app")));
    assert_eq!(
        *seen.borrow(),
        vec![abs("/opt/app")],
        "the marker is asked about the candidate profile dir"
    );
}

#[test]
fn plain_binary_resolves_to_its_own_directory() {
    assert_eq!(
        layout("/work/target/debug/tool", marker),
        Some(abs("/work/target/debug"))
    );
}

#[test]
fn new_build_dir_layout_resolves_to_the_profile_dir() {
    // Shape of cargo's -Zbuild-dir-new-layout, measured once; the hashes here are made up.
    let a = format!("/t/target/debug/build/toy2/3d3289c4a1b2c3d4e5f6/out/a-{H}");
    let b = format!("/t/target/debug/build/toy2/644b91ce0a1b2c3d4e5f/out/b-{H}");
    assert_eq!(layout(&a, no_marker), Some(abs("/t/target/debug")));
    assert_eq!(layout(&b, no_marker), Some(abs("/t/target/debug")));
}

#[test]
fn an_out_chain_without_a_cargo_marker_is_a_plain_directory() {
    assert_eq!(
        layout("/p/build/a/b/out/tool", no_marker),
        Some(abs("/p/build/a/b/out"))
    );
    assert_eq!(layout("/p/build/a/b/out/tool", marker), Some(abs("/p")));
}

#[test]
fn a_bare_out_dir_not_under_build_is_a_plain_directory() {
    assert_eq!(layout(&format!("/x/y/out/t-{H}"), marker), Some(abs("/x/y/out")));
    assert_eq!(
        layout(&format!("/x/notbuild/pkg/hash/out/t-{H}"), marker),
        Some(abs("/x/notbuild/pkg/hash/out"))
    );
}

#[test]
fn shallow_paths_do_not_panic_or_overstep() {
    assert_eq!(layout(&format!("/deps/t-{H}"), no_marker), Some(abs("/")));
    assert_eq!(layout(&format!("/a/out/t-{H}"), no_marker), Some(abs("/a/out")));
    assert_eq!(
        layout(&format!("/pkg/hash/out/t-{H}"), no_marker),
        Some(abs("/pkg/hash/out"))
    );
    assert_eq!(layout(&format!("/build/pkg/hash/out/t-{H}"), no_marker), Some(abs("/")));
}

#[test]
fn examples_nested_under_out_follows_the_examples_rule() {
    assert_eq!(
        layout(&format!("/p/build/x/h/out/examples/t-{H}"), no_marker),
        Some(abs("/p/build/x/h/out"))
    );
}

#[test]
fn resolution_follows_the_executable_not_the_build_location() {
    assert_eq!(
        layout(&format!("/one/target/debug/deps/t-{H}"), no_marker),
        Some(abs("/one/target/debug"))
    );
    assert_eq!(
        layout(&format!("/two/extracted/debug/deps/t-{H}"), no_marker),
        Some(abs("/two/extracted/debug"))
    );
}

#[test]
fn executable_without_a_parent_has_no_layout_dir() {
    assert_eq!(layout_dir(Path::new(""), no_marker), None);
}

#[test]
fn cargo_marker_is_a_fingerprint_dir_or_a_cachedir_tag_one_level_up() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("debug");
    std::fs::create_dir(&profile).unwrap();
    assert!(!has_cargo_marker(&profile));
    std::fs::create_dir(profile.join(".fingerprint")).unwrap();
    assert!(has_cargo_marker(&profile));
    std::fs::remove_dir(profile.join(".fingerprint")).unwrap();
    std::fs::write(tmp.path().join("CACHEDIR.TAG"), b"").unwrap();
    assert!(has_cargo_marker(&profile));
}

#[test]
fn an_unmarked_unhashed_deps_directory_resolves_to_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let deps = tmp.path().join("opt/app/deps");
    std::fs::create_dir_all(&deps).unwrap();
    std::fs::write(deps.join("t"), b"").unwrap();
    let got = resolve(None, || Ok(deps.join("t"))).unwrap();
    assert_eq!(got, dunce::canonicalize(&deps).unwrap());
}

// Override and errors -----

fn never() -> io::Result<PathBuf> {
    panic!("current_exe must not be consulted when SKULD_DB_DIR is set")
}

#[test]
fn override_wins_without_consulting_the_executable_and_is_created() {
    let tmp = tempfile::tempdir().unwrap();
    let want = tmp.path().join("a").join("b");
    assert_eq!(resolve(Some(want.as_os_str()), never).unwrap(), want);
    assert!(want.is_dir());
}

#[test]
fn override_works_when_current_exe_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let got = resolve(Some(tmp.path().as_os_str()), || Err(io::Error::other("no /proc"))).unwrap();
    assert_eq!(got, tmp.path());
}

#[test]
fn current_exe_failure_without_override_names_the_variable() {
    let err = resolve(None, || Err(io::Error::other("no /proc"))).unwrap_err();
    assert!(err.contains("SKULD_DB_DIR") && err.contains("no /proc"), "{err}");
}

#[test]
fn relative_override_is_rejected_naming_the_variable_and_value() {
    let err = resolve(Some(OsStr::new("rel/dir")), never).unwrap_err();
    assert!(err.contains("SKULD_DB_DIR") && err.contains("rel/dir"), "{err}");
}

#[test]
fn empty_override_is_rejected() {
    let err = resolve(Some(OsStr::new("")), never).unwrap_err();
    assert!(err.contains("SKULD_DB_DIR"), "{err}");
}

#[test]
fn uncreatable_override_is_an_error_naming_the_path_and_cause() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("file");
    std::fs::write(&file, b"").unwrap();
    let want = file.join("sub");
    let err = resolve(Some(want.as_os_str()), never).unwrap_err();
    assert!(err.contains(&format!("{want:?}")), "{err}");
}

#[test]
fn missing_exe_directory_is_an_error_naming_the_variable() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = tmp.path().join("gone/deps/t");
    let err = resolve(None, || Ok(exe)).unwrap_err();
    assert!(err.contains("SKULD_DB_DIR"), "{err}");
}

// Executable canonicalization -----

#[cfg(target_os = "linux")]
#[test]
fn deleted_executable_is_resolved_through_its_parent() {
    // Linux reports a replaced binary as "<path> (deleted)".
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("debug");
    std::fs::create_dir_all(profile.join("deps")).unwrap();
    let exe = profile.join("deps").join(format!("t-{H} (deleted)"));
    let got = resolve(None, || Ok(exe)).unwrap();
    assert_eq!(got, dunce::canonicalize(&profile).unwrap());
}

#[cfg(not(target_os = "linux"))]
#[test]
fn deleted_suffix_is_not_special_off_linux() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("debug/deps")).unwrap();
    let exe = tmp.path().join("debug/deps/t (deleted)");
    let err = resolve(None, || Ok(exe)).unwrap_err();
    assert!(err.contains("SKULD_DB_DIR"), "{err}");
}

#[test]
fn a_missing_executable_without_the_deleted_suffix_is_an_error_naming_the_variable() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("debug/deps")).unwrap();
    let err = resolve(None, || Ok(tmp.path().join("debug/deps/gone"))).unwrap_err();
    assert!(err.contains("SKULD_DB_DIR"), "{err}");
}

#[cfg(unix)]
#[test]
fn dangling_symlinked_executable_is_an_error_not_the_symlinks_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real/debug/deps");
    std::fs::create_dir_all(&real).unwrap();
    let target = real.join("t");
    std::fs::write(&target, b"").unwrap();
    let link_dir = tmp.path().join("b");
    std::fs::create_dir(&link_dir).unwrap();
    let link = link_dir.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    std::fs::remove_file(&target).unwrap();
    let err = resolve(None, || Ok(link)).unwrap_err();
    assert!(err.contains("SKULD_DB_DIR"), "{err}");
}

#[cfg(unix)]
#[test]
fn symlinked_executable_resolves_to_the_real_profile_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real/debug/deps");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join(format!("t-{H}")), b"").unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(real.join(format!("t-{H}")), &link).unwrap();
    let got = resolve(None, || Ok(link)).unwrap();
    assert_eq!(got, dunce::canonicalize(tmp.path().join("real/debug")).unwrap());
}

// Directory validation -----

#[test]
fn override_naming_an_existing_file_is_rejected_as_not_a_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("file");
    std::fs::write(&file, b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o777)).unwrap();
    }
    let err = resolve(Some(file.as_os_str()), never).unwrap_err();
    assert!(err.contains("not a directory") && err.contains("SKULD_DB_DIR"), "{err}");
}

// Directory creation -----

#[cfg(unix)]
mod creation {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_level_is_never_visible_at_its_final_path_before_it_is_world_writable() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("made");
        let mut calls = 0;
        create_dir_all_open_with(&target, |temp, published| {
            calls += 1;
            assert_eq!(published, target);
            assert!(!published.exists(), "nothing may be visible at the final path yet");
            assert_eq!(mode(temp), 0o777, "the temp must already be world-writable");
            assert_eq!(temp.parent(), published.parent());
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(mode(&target), 0o777);
    }

    #[test]
    fn losing_the_publish_race_leaves_the_winners_directory_untouched_and_no_temp() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("made");
        create_dir_all_open_with(&target, |_, published| {
            std::fs::create_dir(published).unwrap();
            std::fs::set_permissions(published, std::fs::Permissions::from_mode(0o700)).unwrap();
        })
        .unwrap();
        assert_eq!(mode(&target), 0o700);
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![OsStr::new("made")], "the losing temp must be removed");
    }

    #[test]
    fn a_pre_existing_directory_is_left_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("there");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        resolve(Some(dir.as_os_str()), never).unwrap();
        assert_eq!(mode(&dir), 0o755);
    }

    #[test]
    fn only_the_missing_levels_of_a_partial_chain_are_created_world_writable() {
        let tmp = tempfile::tempdir().unwrap();
        let existing = tmp.path().join("existing");
        std::fs::create_dir(&existing).unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).unwrap();
        let leaf = existing.join("a").join("b");
        resolve(Some(leaf.as_os_str()), never).unwrap();
        assert_eq!(mode(&existing), 0o755);
        assert_eq!(mode(&existing.join("a")), 0o777);
        assert_eq!(mode(&leaf), 0o777);
    }
}
