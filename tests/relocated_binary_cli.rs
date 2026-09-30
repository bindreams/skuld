//! A skuld test binary locates its coordination database at run time, from its
//! own path, so a moved or archived binary works. Uses the standard libtest
//! harness because skuld is the subject. Spawns copies of `capture_fixture`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const DB: &str = ".skuld.db";

/// A cargo-style test binary name, `name-<16 hex>`, which marks a binary as cargo's.
fn hashed(name: &str) -> String {
    format!("{name}-0123456789abcdef{}", std::env::consts::EXE_SUFFIX)
}

fn plain(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// Copy the fixture to `dir/name` and return the copy.
fn copy_fixture_to(dir: &Path, name: &str) -> PathBuf {
    copy_bin_to(env!("CARGO_BIN_EXE_capture_fixture"), dir, name)
}

fn copy_bin_to(src: &str, dir: &Path, name: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let dest = dir.join(name);
    std::fs::copy(src, &dest).unwrap();
    dest
}

fn run(exe: &Path, db_dir: Option<&Path>) -> Output {
    let mut cmd = Command::new(exe);
    cmd.args(["passing_with_noise", "--exact"]).env_remove("SKULD_DB_DIR");
    if let Some(d) = db_dir {
        cmd.env("SKULD_DB_DIR", d);
    }
    cmd.output().unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn binary_moved_into_a_deps_dir_elsewhere_uses_that_profile_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("extracted").join("debug");
    let exe = copy_fixture_to(&profile.join("deps"), &hashed("capture_fixture"));
    let out = run(&exe, None);
    assert!(out.status.success(), "{out:?}\n{}", stderr(&out));
    assert!(profile.join(DB).is_file(), "expected {:?} to exist", profile.join(DB));
}

#[test]
fn binary_moved_next_to_no_deps_dir_uses_its_own_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("bin");
    let exe = copy_fixture_to(&profile, &plain("capture_fixture"));
    let out = run(&exe, None);
    assert!(out.status.success(), "{out:?}\n{}", stderr(&out));
    assert!(profile.join(DB).is_file());
}

/// Without a cargo marker, a directory named `deps` is just a directory.
#[test]
fn unhashed_binary_in_a_deps_dir_uses_that_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let deps = tmp.path().join("opt/app/deps");
    let exe = copy_fixture_to(&deps, &plain("capture_fixture"));
    let out = run(&exe, None);
    assert!(out.status.success(), "{out:?}\n{}", stderr(&out));
    assert!(deps.join(DB).is_file());
    assert!(!tmp.path().join("opt/app").join(DB).exists());
}

#[test]
fn override_env_var_picks_the_directory_and_creates_it() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = copy_fixture_to(&tmp.path().join("debug").join("deps"), &hashed("capture_fixture"));
    let db_dir = tmp.path().join("elsewhere").join("nested");
    let out = run(&exe, Some(&db_dir));
    assert!(out.status.success(), "{out:?}\n{}", stderr(&out));
    assert!(db_dir.join(DB).is_file());
    assert!(
        !tmp.path().join("debug").join(DB).exists(),
        "override must win over the exe location"
    );
}

#[test]
fn relative_override_fails_loudly() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = copy_fixture_to(&tmp.path().join("debug").join("deps"), &hashed("capture_fixture"));
    let out = run(&exe, Some(Path::new("relative/dir")));
    assert!(!out.status.success(), "{out:?}");
    let err = stderr(&out);
    assert!(err.contains("SKULD_DB_DIR") && err.contains("relative/dir"), "{err}");
}

/// Test binaries in cargo's new build-dir layout share the profile dir's DB.

#[test]
fn new_build_dir_layout_resolves_to_the_profile_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("debug");
    let out = profile.join("build/toy2/3d3289c4a1b2c3d4/out");
    let exe = copy_fixture_to(&out, &hashed("a"));
    let res = run(&exe, None);
    assert!(res.status.success(), "{res:?}\n{}", stderr(&res));
    assert!(profile.join(DB).is_file(), "DB should be in {profile:?}");
    assert!(!out.join(DB).exists());
}

/// A trial that changes `SKULD_DB_DIR` mid-run must not move later trials to another DB.
#[test]
fn db_path_is_resolved_once_per_process() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("debug");
    let exe = copy_bin_to(
        env!("CARGO_BIN_EXE_db_dir_env_probe"),
        &profile.join("deps"),
        &hashed("probe"),
    );
    let other = tmp.path().join("other");
    let out = Command::new(&exe)
        .env_remove("SKULD_DB_DIR")
        .env("PROBE_OTHER_DB_DIR", &other)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}\n{}", stderr(&out));
    assert!(profile.join(DB).is_file());
    assert!(
        !other.join(DB).exists(),
        "second trial registered in the mid-run SKULD_DB_DIR"
    );
}

/// A directory skuld creates for `SKULD_DB_DIR` must be usable by any uid, whatever the umask.
#[cfg(unix)]
#[test]
fn created_override_dirs_are_world_writable_despite_umask() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    let tmp = tempfile::tempdir().unwrap();
    let exe = copy_fixture_to(&tmp.path().join("debug/deps"), &hashed("capture_fixture"));
    let db_dir = tmp.path().join("made/nested");
    let parent_before = std::fs::metadata(tmp.path()).unwrap().permissions().mode() & 0o777;
    let mut cmd = Command::new(&exe);
    cmd.args(["passing_with_noise", "--exact"]).env("SKULD_DB_DIR", &db_dir);
    // SAFETY: umask is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            libc::umask(0o077);
            Ok(())
        })
    };
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{out:?}\n{}", stderr(&out));
    for d in [tmp.path().join("made"), db_dir] {
        let mode = std::fs::metadata(&d).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o777, "{d:?}");
    }
    let parent_after = std::fs::metadata(tmp.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        parent_before, parent_after,
        "the pre-existing parent must be left alone"
    );
}

/// The DB must not depend on how the binary was reached: direct, via a file symlink, or via a
/// directory symlink (macOS and Windows `current_exe` do not resolve symlinks).
#[cfg(unix)]
#[test]
fn symlinked_binary_uses_the_real_profile_dir() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let real_profile = tmp.path().join("real/debug");
    let real_deps = real_profile.join("deps");
    let exe = copy_fixture_to(&real_deps, &hashed("fx"));

    let file_link_dir = tmp.path().join("b");
    std::fs::create_dir_all(&file_link_dir).unwrap();
    let file_link = file_link_dir.join("link");
    symlink(&exe, &file_link).unwrap();

    let dir_link = tmp.path().join("dlink");
    symlink(&real_deps, &dir_link).unwrap();

    for via in [exe.clone(), file_link, dir_link.join(hashed("fx"))] {
        let out = run(&via, None);
        assert!(out.status.success(), "{via:?}: {out:?}\n{}", stderr(&out));
    }
    assert!(real_profile.join(DB).is_file());
    assert!(
        !real_deps.join(DB).exists(),
        "dir symlink must not create a DB in deps/"
    );
    assert!(
        !file_link_dir.join(DB).exists(),
        "file symlink must not create a DB beside the link"
    );
}

/// Directory permission bits do not bind root, so this needs a non-root user.
#[cfg(unix)]
mod read_only {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Sets `mode` on a directory and restores 0755 on drop so the temp dir can be deleted.
    struct Mode(PathBuf);
    impl Mode {
        fn set(p: &Path, mode: u32) -> Self {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
            Self(p.to_path_buf())
        }
    }
    impl Drop for Mode {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    fn assert_not_root() {
        // SAFETY: geteuid has no preconditions.
        assert_ne!(
            unsafe { libc::geteuid() },
            0,
            "read-only directory tests require a non-root user"
        );
    }

    #[test]
    fn read_only_profile_dir_fails_loudly_naming_the_directory() {
        assert_not_root();
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("debug");
        let exe = copy_fixture_to(&profile.join("deps"), &hashed("capture_fixture"));
        let _ro = Mode::set(&profile, 0o555);
        let out = run(&exe, None);
        assert!(!out.status.success(), "must not silently fall back: {out:?}");
        let err = stderr(&out);
        assert!(err.contains(profile.to_str().unwrap()), "{err}");
        assert!(
            err.contains("SKULD_DB_DIR"),
            "error should point at the override: {err}"
        );
    }

    #[test]
    fn read_only_profile_dir_with_an_existing_db_fails_loudly_naming_the_override() {
        assert_not_root();
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("debug");
        let exe = copy_fixture_to(&profile.join("deps"), &hashed("capture_fixture"));
        let first = run(&exe, None);
        assert!(first.status.success(), "{first:?}");
        assert!(profile.join(DB).is_file());
        let _ro = Mode::set(&profile, 0o555);
        let out = run(&exe, None);
        assert!(!out.status.success(), "{out:?}");
        let err = stderr(&out);
        assert!(
            err.contains(profile.to_str().unwrap()) && err.contains("SKULD_DB_DIR"),
            "{err}"
        );
    }

    #[test]
    fn unreadable_override_dir_fails_loudly_naming_the_override() {
        assert_not_root();
        let tmp = tempfile::tempdir().unwrap();
        let exe = copy_fixture_to(&tmp.path().join("debug/deps"), &hashed("capture_fixture"));
        let dir = tmp.path().join("wx");
        std::fs::create_dir(&dir).unwrap();
        let _restore = Mode::set(&dir, 0o300);
        let out = run(&exe, Some(&dir));
        assert!(!out.status.success(), "{out:?}");
        let err = stderr(&out);
        assert!(
            err.contains(dir.to_str().unwrap()) && err.contains("SKULD_DB_DIR"),
            "{err}"
        );
    }

    #[test]
    fn read_only_profile_dir_works_with_a_writable_override() {
        assert_not_root();
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("debug");
        let exe = copy_fixture_to(&profile.join("deps"), &hashed("capture_fixture"));
        let _ro = Mode::set(&profile, 0o555);
        let db_dir = tmp.path().join("rw");
        let out = run(&exe, Some(&db_dir));
        assert!(out.status.success(), "{out:?}\n{}", stderr(&out));
        assert!(db_dir.join(DB).is_file());
        assert!(!profile.join(DB).exists());
    }
}

#[cfg(windows)]
mod read_only_windows {
    use super::*;

    /// Denies file/subdirectory creation to Everyone; the deny ACE is removed on drop.
    struct DenyWrite(PathBuf);
    impl DenyWrite {
        fn new(p: &Path) -> Self {
            let st = Command::new("icacls")
                .arg(p)
                .args(["/deny", "*S-1-1-0:(OI)(CI)(WD,AD)"])
                .output()
                .unwrap();
            assert!(st.status.success(), "{st:?}");
            Self(p.to_path_buf())
        }
    }
    impl Drop for DenyWrite {
        fn drop(&mut self) {
            let _ = Command::new("icacls")
                .arg(&self.0)
                .args(["/remove:d", "*S-1-1-0"])
                .output();
        }
    }

    #[test]
    fn write_denied_directory_fails_loudly_naming_the_override() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = copy_fixture_to(&tmp.path().join("debug").join("deps"), &hashed("capture_fixture"));
        let dir = tmp.path().join("denied");
        std::fs::create_dir(&dir).unwrap();
        let _deny = DenyWrite::new(&dir);
        let out = run(&exe, Some(&dir));
        assert!(!out.status.success(), "{out:?}");
        let err = stderr(&out);
        assert!(err.contains("SKULD_DB_DIR") && err.contains("denied"), "{err}");
    }
}
