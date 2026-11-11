//! Tests for the file-lock registry.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering::SeqCst};

use super::lock::{lock_exclusive, open_lock_target, try_lock_exclusive};
use super::registry::{
    mutex_base, register, register_with, running_dir, try_register, IdGenerator, Registration, PROCESS_IDS,
};
use super::rendezvous::rendezvous;
use super::test_hooks::{retry_rendezvous, set_test_retry_hook, set_test_seam_hook, Seam};
use super::{SERIAL_ALL, SERIAL_NONE};
use crate::label::Label;

/// A fresh coordination directory, `<temp>/.skuld`, which does not exist yet.
pub(super) fn temp_coord() -> (crate::TempDir, PathBuf) {
    let dir = crate::TempDir::new().unwrap();
    let coord = dir.path().join(".skuld");
    (dir, coord)
}

/// One registration attempt: `Some` means the test could start.
fn attempt(coord: &Path, name: &str, labels: &[Label], filter: &str) -> Option<Registration> {
    try_register(&PROCESS_IDS, coord, name, labels, filter)
}

/// Whether a test with `labels` and `filter` could start now, leaving no registration behind.
fn can_start_now(coord: &Path, labels: &[Label], filter: &str) -> bool {
    match attempt(coord, "probe", labels, filter) {
        Some(mut reg) => {
            reg.unregister();
            true
        }
        None => false,
    }
}

/// A live registration of a test that is not the one under test.
fn running(coord: &Path, name: &str, labels: &[Label], filter: &str) -> Registration {
    attempt(coord, name, labels, filter).unwrap_or_else(|| panic!("setup: {name} must be able to start"))
}

/// The names in `running/`, sorted.
fn running_files(coord: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(running_dir(coord))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// Make `running/` and write a file of `body` in it.
fn hand_made(coord: &Path, file: &str, body: &str) -> PathBuf {
    std::fs::create_dir_all(running_dir(coord)).unwrap();
    let path = running_dir(coord).join(file);
    std::fs::write(&path, body).unwrap();
    path
}

/// An entry in the documented format.
fn entry_json(filter: &str) -> String {
    format!(r#"{{"name":"hand_made","labels":[],"filter":"{filter}"}}"#)
}

/// Another handle holding `path`'s lock, as a registered test in another process would.
fn hold(path: &Path) -> File {
    let file = std::fs::OpenOptions::new().read(true).write(true).open(path).unwrap();
    lock_exclusive(&file).unwrap();
    file
}

// can_start rules =====

#[test]
fn non_serial_can_start_when_empty() {
    let (_dir, coord) = temp_coord();
    assert!(can_start_now(&coord, &[], SERIAL_NONE));
}

#[test]
fn non_serial_blocked_by_global_serial() {
    let (_dir, coord) = temp_coord();
    let _blocker = running(&coord, "blocker", &[], SERIAL_ALL);
    assert!(!can_start_now(&coord, &[], SERIAL_NONE));
}

#[test]
fn non_serial_blocked_by_matching_filter() {
    let (_dir, coord) = temp_coord();
    let docker = Label::__new("docker");
    let _serial = running(&coord, "serial_docker", &[], "docker");
    assert!(!can_start_now(&coord, &[docker], SERIAL_NONE));
    assert!(can_start_now(&coord, &[], SERIAL_NONE));
}

#[test]
fn global_serial_blocked_when_anything_running() {
    let (_dir, coord) = temp_coord();
    let _some = running(&coord, "some_test", &[], SERIAL_NONE);
    assert!(!can_start_now(&coord, &[], SERIAL_ALL));
}

#[test]
fn global_serial_can_start_when_empty() {
    let (_dir, coord) = temp_coord();
    assert!(can_start_now(&coord, &[], SERIAL_ALL));
}

#[test]
fn filtered_serial_blocked_by_matching_running_test() {
    let (_dir, coord) = temp_coord();
    let docker = Label::__new("docker");
    let _docker_test = running(&coord, "docker_test", &[docker], SERIAL_NONE);
    assert!(!can_start_now(&coord, &[], "docker"));
}

#[test]
fn filtered_serial_not_blocked_by_non_matching() {
    let (_dir, coord) = temp_coord();
    let network = Label::__new("network");
    let _network_test = running(&coord, "network_test", &[network], SERIAL_NONE);
    assert!(can_start_now(&coord, &[], "docker"));
}

#[test]
fn filtered_serial_and_semantics() {
    let (_dir, coord) = temp_coord();
    let a = Label::__new("a");
    let b = Label::__new("b");

    let _test_a = running(&coord, "test_a", &[a], SERIAL_NONE);
    assert!(can_start_now(&coord, &[], "a & b"), "[a] alone does not match `a & b`");

    let _test_ab = running(&coord, "test_ab", &[a, b], SERIAL_NONE);
    assert!(!can_start_now(&coord, &[], "a & b"));
}

#[test]
fn filtered_serial_not_semantics() {
    let (_dir, coord) = temp_coord();
    let a = Label::__new("a");
    let b = Label::__new("b");

    let _test_a = running(&coord, "test_a", &[a], SERIAL_NONE);
    assert!(can_start_now(&coord, &[], "!a"), "[a] does not match `!a`");

    let _test_b = running(&coord, "test_b", &[b], SERIAL_NONE);
    assert!(!can_start_now(&coord, &[], "!a"));
}

#[test]
fn an_unparseable_filter_in_an_entry_is_skipped() {
    let (_dir, coord) = temp_coord();
    let json = hand_made(&coord, "legacy.json", &entry_json("((("));
    let _owner = hold(&hand_made(&coord, "legacy.lock", ""));
    assert!(can_start_now(&coord, &[Label::__new("docker")], SERIAL_NONE));
    assert!(json.exists(), "a live entry stays");
}

// Entry format and unregistration =====

#[test]
fn registration_publishes_the_documented_entry() {
    let (_dir, coord) = temp_coord();
    let docker = Label::__new("docker");
    let reg = running(&coord, "my_test", &[docker], "docker");

    let json = std::fs::read_to_string(running_dir(&coord).join(format!("{}.json", reg.id()))).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"name": "my_test", "labels": ["docker"], "filter": "docker"})
    );
    assert_eq!(
        running_files(&coord),
        vec![format!("{}.json", reg.id()), format!("{}.lock", reg.id())]
    );
}

#[test]
fn unregister_removes_both_files_and_is_idempotent() {
    let (_dir, coord) = temp_coord();
    let mut reg = running(&coord, "my_test", &[], SERIAL_NONE);
    reg.unregister();
    assert_eq!(running_files(&coord), Vec::<String>::new());
    reg.unregister();
}

// Scan =====

#[test]
fn json_without_lock_is_removed() {
    let (_dir, coord) = temp_coord();
    let json = hand_made(&coord, "orphan.json", &entry_json("*"));
    assert!(
        can_start_now(&coord, &[], SERIAL_NONE),
        "a global-serial entry with no owner must not block"
    );
    assert!(!json.exists());
}

#[test]
fn orphan_unheld_lock_is_removed() {
    let (_dir, coord) = temp_coord();
    let lock = hand_made(&coord, "orphan.lock", "");
    assert!(can_start_now(&coord, &[], SERIAL_NONE));
    assert!(!lock.exists());
}

#[test]
fn held_lock_without_json_is_kept_and_not_an_entry() {
    let (_dir, coord) = temp_coord();
    let lock = hand_made(&coord, "held.lock", "");
    let _owner = hold(&lock);
    assert!(can_start_now(&coord, &[], SERIAL_ALL), "no entry, so nothing runs");
    assert!(lock.exists());
}

#[test]
fn tmp_files_are_removed() {
    let (_dir, coord) = temp_coord();
    let json_tmp = hand_made(&coord, "a.json.tmp", &entry_json("*"));
    let lock_tmp = hand_made(&coord, "b.lock.tmp", "");
    assert!(can_start_now(&coord, &[], SERIAL_ALL));
    assert!(!json_tmp.exists() && !lock_tmp.exists());
}

#[cfg(unix)]
#[test]
fn foreign_0644_lock_is_try_locked_read_only() {
    use std::os::unix::fs::PermissionsExt;

    let (_dir, coord) = temp_coord();
    hand_made(&coord, "foreign.json", &entry_json("*"));
    let lock = hand_made(&coord, "foreign.lock", "");
    // Read-only, like another uid's 0644 file: the scan must not open it for writing.
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o444)).unwrap();
    let owner = File::open(&lock).unwrap();
    lock_exclusive(&owner).unwrap();

    assert!(
        !can_start_now(&coord, &[], SERIAL_NONE),
        "the held lock marks the entry live"
    );

    drop(owner);
    assert!(
        can_start_now(&coord, &[], SERIAL_NONE),
        "released, the entry is dead and removed"
    );
}

#[test]
fn entry_with_unknown_fields_is_read() {
    let (_dir, coord) = temp_coord();
    hand_made(
        &coord,
        "future.json",
        r#"{"name":"future","labels":[],"filter":"*","added_later":{"x":1}}"#,
    );
    let _owner = hold(&hand_made(&coord, "future.lock", ""));
    assert!(!can_start_now(&coord, &[], SERIAL_NONE));
}

#[test]
#[should_panic(expected = "broken.json")]
fn unparseable_entry_panics_naming_the_file() {
    let (_dir, coord) = temp_coord();
    hand_made(&coord, "broken.json", "not json");
    let _owner = hold(&hand_made(&coord, "broken.lock", ""));
    let _ = attempt(&coord, "t", &[], SERIAL_NONE);
}

#[test]
fn colliding_id_retries_with_a_new_id() {
    let (_dir, coord) = temp_coord();
    let ids = IdGenerator::new();
    let colliding = ids.id_at(0);
    let lock = hand_made(&coord, &format!("{colliding}.lock"), "");
    let _owner = hold(&lock);

    let mut reg = try_register(&ids, &coord, "t", &[], SERIAL_NONE).expect("nothing blocks it");
    assert_eq!(reg.id(), ids.id_at(1));
    assert!(lock.exists(), "the live lock of the colliding id is not replaced");
    reg.unregister();
}

#[test]
fn register_with_uses_ids_from_its_generator() {
    let (_dir, coord) = temp_coord();
    let ids = IdGenerator::new();
    let mut reg = register_with(&ids, &coord, "t", &[], SERIAL_NONE);
    assert_eq!(reg.id(), ids.id_at(0));
    reg.unregister();
}

// Mutex =====

#[test]
fn lock_file_is_created_under_the_mutex() {
    let (_dir, coord) = temp_coord();
    let _hook = set_test_seam_hook(Seam::Created, {
        let coord = coord.clone();
        move || {
            let fresh = open_lock_target(&mutex_base(&coord));
            assert!(
                matches!(try_lock_exclusive(&fresh), Err(std::fs::TryLockError::WouldBlock)),
                "the mutex must be held while the lock file is created"
            );
        }
    });
    let mut reg = running(&coord, "t", &[], SERIAL_NONE);
    reg.unregister();
}

// Dead owners =====

/// What [`registered_child`] prints once registered.
const CHILD_READY: &str = "child-registered";
const CHILD_DIR_ENV: &str = "SKULD_REGISTRY_TEST_CHILD_DIR";
#[cfg(unix)]
const CHILD_UMASK_ENV: &str = "SKULD_REGISTRY_TEST_CHILD_UMASK";

/// Name of [`registered_child`], as libtest's `--exact` wants it.
fn child_test_name() -> String {
    format!("{}::registered_child", module_tail())
}

/// This module's path without the crate name, as libtest's `--exact` wants it.
fn module_tail() -> &'static str {
    module_path!().split_once("::").expect("module path has a crate").1
}

/// Not a test of its own: the body of the process [`spawn_registered_child`] starts. Without
/// `CHILD_DIR_ENV` it has nothing to do and passes. With it, it registers `SERIAL_ALL` in that
/// directory, reports, and holds the registration until killed.
#[test]
fn registered_child() {
    use std::io::{BufRead, Write};

    let Some(dir) = std::env::var_os(CHILD_DIR_ENV) else {
        return;
    };
    #[cfg(unix)]
    if let Ok(mask) = std::env::var(CHILD_UMASK_ENV) {
        // Safety: `umask` only sets this process's creation mask.
        unsafe { libc::umask(u32::from_str_radix(&mask, 8).unwrap() as libc::mode_t) };
    }
    let _reg = register(Path::new(&dir), "child", &[], SERIAL_ALL);
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{CHILD_READY}").unwrap();
    stdout.flush().unwrap();
    // Never written to: ends only when the parent kills this process.
    let _ = std::io::stdin().lock().read_line(&mut String::new());
}

/// A copy of this test binary running [`registered_child`] against `coord`, returned once it has
/// registered.
#[allow(clippy::zombie_processes)] // the caller kills and waits for the child it is handed
fn spawn_registered_child(coord: &Path, umask: Option<&str>) -> std::process::Child {
    use std::io::BufRead;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", &child_test_name(), "--nocapture", "--test-threads=1"])
        .env(CHILD_DIR_ENV, coord)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    #[cfg(unix)]
    if let Some(mask) = umask {
        cmd.env(CHILD_UMASK_ENV, mask);
    }
    #[cfg(not(unix))]
    assert!(umask.is_none(), "umask is Unix-only");
    let mut child = cmd.spawn().unwrap();
    let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        if out.read_line(&mut line).unwrap_or(0) == 0 {
            let status = child.wait().unwrap();
            panic!("the child exited before registering: {status}");
        }
        // libtest has already printed `test <name> ... ` on this line.
        if line.trim_end().ends_with(CHILD_READY) {
            return child;
        }
    }
}

#[test]
fn entry_of_a_dead_owner_is_removed() {
    let (_dir, coord) = temp_coord();
    let mut child = spawn_registered_child(&coord, None);
    let child_files = running_files(&coord);
    assert_eq!(
        child_files.len(),
        2,
        "the child holds a .lock and a .json: {child_files:?}"
    );
    assert!(
        !can_start_now(&coord, &[], SERIAL_NONE),
        "a live SERIAL_ALL child blocks"
    );

    child.kill().unwrap();
    child.wait().unwrap();

    // Not `attempt`: Windows releases a dead process's locks asynchronously, so the first scans may
    // still see the lock held.
    let mut reg = register(&coord, "parent", &[], SERIAL_ALL);
    let ours = vec![format!("{}.json", reg.id()), format!("{}.lock", reg.id())];
    assert_eq!(running_files(&coord), ours, "the child's files must be gone");
    reg.unregister();
}

#[cfg(unix)]
const FD_CHILD_DIR_ENV: &str = "SKULD_REGISTRY_TEST_FD_CHILD_DIR";
#[cfg(unix)]
const FD_CHILD_DONE: &str = "child-unregistered";

/// Not a test of its own: the body of the process [`unregister_needs_no_new_file_descriptor`]
/// starts. Without `FD_CHILD_DIR_ENV` it passes. With it, it registers, spends every free file
/// descriptor, then unregisters.
#[cfg(unix)]
#[test]
fn unregistering_child() {
    let Some(dir) = std::env::var_os(FD_CHILD_DIR_ENV) else {
        return;
    };
    let mut reg = register(Path::new(&dir), "child", &[], SERIAL_NONE);
    assert_eq!(
        running_files(Path::new(&dir)).len(),
        2,
        "registered: a .lock and a .json"
    );

    // The next `open` returns the lowest free descriptor, so a soft limit at or below it makes
    // every later `open` fail EMFILE.
    // Safety: `fcntl(F_GETFD)` only inspects a descriptor and fails on a closed one.
    let lowest_free = (0..)
        .find(|&fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1)
        .unwrap();
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // Safety: `limit` is a valid rlimit for the call to read and write.
    unsafe {
        assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit), 0);
        limit.rlim_cur = lowest_free as libc::rlim_t;
        assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &limit), 0);
    }
    std::fs::File::open(".").expect_err("no descriptor is free");

    reg.unregister();
    println!("{FD_CHILD_DONE}");
}

/// A test body can exhaust its descriptor table (`RLIMIT_NOFILE`); unregistration must then still
/// succeed, so it may open nothing that registration did not already hold.
#[cfg(unix)]
#[test]
fn unregister_needs_no_new_file_descriptor() {
    let (_dir, coord) = temp_coord();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{}::unregistering_child", module_tail()),
            "--nocapture",
        ])
        .env(FD_CHILD_DIR_ENV, &coord)
        .output()
        .unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        output.status.success() && stdout.contains(FD_CHILD_DONE),
        "the child must unregister with no free descriptor: {}
stdout:
{stdout}
stderr:
{stderr}",
        output.status
    );
    assert_eq!(
        running_files(&coord),
        Vec::<String>::new(),
        "unregistration removes both files"
    );
}

#[cfg(unix)]
#[test]
fn files_are_0644_whatever_the_umask() {
    use std::os::unix::fs::PermissionsExt;

    let (_dir, coord) = temp_coord();
    let mut child = spawn_registered_child(&coord, Some("077"));
    for file in running_files(&coord) {
        let mode = std::fs::metadata(running_dir(&coord).join(&file))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o644, "{file}");
    }
    child.kill().unwrap();
    child.wait().unwrap();
}

// Concurrency =====

#[test]
fn global_serial_prevents_concurrent_execution() {
    const THREADS: usize = 8;
    let (_dir, coord) = temp_coord();

    let running = AtomicU32::new(0);
    let points = rendezvous(THREADS);

    std::thread::scope(|s| {
        for point in points {
            s.spawn(|| {
                point.wait();
                let mut reg = register(&coord, "serial_test", &[], SERIAL_ALL);
                assert_eq!(
                    running.fetch_add(1, SeqCst),
                    0,
                    "global serial allowed concurrent execution"
                );
                std::thread::yield_now();
                assert_eq!(running.load(SeqCst), 1, "global serial allowed concurrent execution");
                running.fetch_sub(1, SeqCst);
                reg.unregister();
            });
        }
    });
}

#[test]
fn non_serial_allows_concurrent_execution() {
    const THREADS: usize = 8;
    const { assert!(THREADS >= 2) };
    let (_dir, coord) = temp_coord();

    // Two rendezvous points: the first races every thread into `register` together; the second
    // holds every thread's registration until all are registered, so the peak is THREADS
    // whatever `register`'s latency. A live-but-stuck participant still hangs the rest; see the
    // non-blocking counterpart below.
    let peak = AtomicU32::new(0);
    let running = AtomicU32::new(0);
    let entry_points = rendezvous(THREADS);
    let observation_points = rendezvous(THREADS);

    std::thread::scope(|s| {
        for (entry, observation) in entry_points.into_iter().zip(observation_points) {
            s.spawn(|| {
                entry.wait();
                let mut reg = register(&coord, "parallel_test", &[], SERIAL_NONE);
                let n = running.fetch_add(1, SeqCst) + 1;
                peak.fetch_max(n, SeqCst);
                observation.wait();
                running.fetch_sub(1, SeqCst);
                reg.unregister();
            });
        }
    });

    assert_eq!(
        peak.load(SeqCst) as usize,
        THREADS,
        "non-serial tests should run concurrently"
    );
}

/// Non-blocking counterpart to `non_serial_allows_concurrent_execution`: asks `try_register`
/// against already-registered tests, so a regression fails instead of hanging. The registrations
/// carry labels, and one serial entry holds a filter the candidate does not match: neither may
/// block a non-serial test, while the same serial entry still blocks a matching one.
#[test]
fn non_serial_registrations_do_not_block_can_start_for_another_non_serial_test() {
    const REGISTRATIONS: usize = 7;
    let (_dir, coord) = temp_coord();
    let docker = Label::__new("docker");
    let network = Label::__new("network");
    let cache = Label::__new("cache");

    let _parallel: Vec<Registration> = (0..REGISTRATIONS)
        .map(|i| running(&coord, &format!("parallel_test_{i}"), &[docker, network], SERIAL_NONE))
        .collect();
    let _serial_cache = running(&coord, "serial_cache", &[], "cache");

    assert!(
        can_start_now(&coord, &[docker, network], SERIAL_NONE),
        "{REGISTRATIONS} labelled non-serial registrations and a serial entry with a non-matching filter must not block another non-serial test from starting"
    );
    assert!(
        !can_start_now(&coord, &[cache], SERIAL_NONE),
        "the serial entry must still block a test its filter matches"
    );
}

#[test]
fn filtered_serial_blocks_only_matching_tests() {
    let (_dir, coord) = temp_coord();
    let docker = Label::__new("docker");
    let network = Label::__new("network");

    let _serial = running(&coord, "serial_docker", &[], "docker");
    let _net = running(&coord, "net_test", &[network], SERIAL_NONE);
    assert!(!can_start_now(&coord, &[docker], SERIAL_NONE));
}

/// Retries a blocked registrant must survive before the blocker leaves: more than any plausible
/// cap, so a retry limit would fail the registration instead.
const NO_CAP_RETRIES: usize = 8;

#[test]
fn blocked_registration_signals_retry_and_holds_no_files() {
    let (_dir, coord) = temp_coord();
    let mut blocker = running(&coord, "blocker", &[], SERIAL_ALL);
    let blocker_files = running_files(&coord);
    let (worker, test) = retry_rendezvous();

    std::thread::scope(|s| {
        let waiter = s.spawn(|| {
            let _hook = set_test_retry_hook(worker);
            register(&coord, "waiter", &[], SERIAL_ALL)
        });

        test.pass_retries(NO_CAP_RETRIES);
        test.wait_for_retry();
        assert_eq!(running_files(&coord), blocker_files, "a waiting test holds no files");

        blocker.unregister();
        test.release();
        let mut reg = waiter.join().unwrap();
        test.expect_no_more_retries();
        reg.unregister();
    });
}

// Moved directories =====

#[cfg(unix)]
#[test]
#[should_panic(expected = "moved")]
fn drop_panics_when_running_dir_was_replaced() {
    let (_dir, coord) = temp_coord();
    let mut reg = running(&coord, "t", &[], SERIAL_NONE);
    let aside = coord.join("running.aside");
    std::fs::rename(running_dir(&coord), &aside).unwrap();
    std::fs::create_dir(running_dir(&coord)).unwrap();
    reg.unregister();
}

#[cfg(windows)]
#[test]
fn running_lock_cannot_be_deleted_while_registered() {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    let (_dir, coord) = temp_coord();
    let mut reg = running(&coord, "t", &[], SERIAL_NONE);
    let err = std::fs::remove_file(running_dir(&coord).join(format!("{}.lock", reg.id())))
        .expect_err("Windows must refuse to delete a lock file its owner holds open");
    assert_eq!(err.raw_os_error(), Some(ERROR_SHARING_VIOLATION), "{err}");
    reg.unregister();
}

/// A recursive delete that carries on past errors would otherwise remove the live `.json` and
/// leave its `.lock`, which the scan ignores: a serial test could then start beside this one.
#[cfg(windows)]
#[test]
fn running_json_cannot_be_deleted_while_registered() {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    let (_dir, coord) = temp_coord();
    let mut reg = running(&coord, "t", &[], SERIAL_ALL);
    let err =
        std::fs::remove_file(reg.json_path()).expect_err("Windows must refuse to delete an entry its owner holds open");
    assert_eq!(err.raw_os_error(), Some(ERROR_SHARING_VIOLATION), "{err}");
    assert!(
        attempt(&coord, "second", &[], SERIAL_NONE).is_none(),
        "the entry must still block a conflicting registration"
    );
    reg.unregister();
    assert_eq!(running_files(&coord), Vec::<String>::new());
}

/// Rename `coord` aside and make a fresh one at its path.
#[cfg(unix)]
fn split(coord: &Path) {
    std::fs::rename(coord, coord.with_extension("aside")).unwrap();
    std::fs::create_dir(coord).unwrap();
}

#[cfg(unix)]
#[test]
#[should_panic(expected = "moved")]
fn register_panics_when_the_mutex_splits_before_publication() {
    let (_dir, coord) = temp_coord();
    let _hook = set_test_seam_hook(Seam::Published, {
        let coord = coord.clone();
        move || split(&coord)
    });
    let _ = register(&coord, "t", &[], SERIAL_NONE);
}

#[cfg(unix)]
#[test]
#[should_panic(expected = "moved")]
fn unregister_panics_when_the_mutex_has_split() {
    let (_dir, coord) = temp_coord();
    let mut reg = running(&coord, "t", &[], SERIAL_NONE);
    let _hook = set_test_seam_hook(Seam::Unregistering, {
        let coord = coord.clone();
        move || split(&coord)
    });
    reg.unregister();
}

/// A symlink ancestor of `coord` retargeted mid-run leaves the registration's files behind the
/// old target, so unregistering must fail rather than report success against whatever the path
/// names now. Windows withholds `FILE_SHARE_DELETE` from the lock file, but that does not stop an
/// ancestor being retargeted.
#[test]
#[should_panic(expected = "moved")]
fn unregister_panics_when_a_parent_symlink_is_retargeted_mid_run() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink as symlink_dir;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_dir;

    let dir = crate::TempDir::new().unwrap();
    let (real1, real2, link) = (
        dir.path().join("real1"),
        dir.path().join("real2"),
        dir.path().join("link"),
    );
    std::fs::create_dir(&real1).unwrap();
    std::fs::create_dir(&real2).unwrap();
    symlink_dir(&real1, &link).expect("creating a directory symlink must succeed in this CI environment");
    let coord = link.join(".skuld");

    let mut a = running(&coord, "a", &[], SERIAL_ALL);

    // Retarget. A symlink directory is removed with `remove_dir` on Windows and `remove_file` on
    // Unix; `symlink_dir` after either makes a fresh one.
    #[cfg(unix)]
    std::fs::remove_file(&link).unwrap();
    #[cfg(windows)]
    std::fs::remove_dir(&link).unwrap();
    symlink_dir(&real2, &link).unwrap();

    // `b` registers through the retargeted link, in a coordination directory `a` knows nothing
    // of, so it must not wait on `a`'s global-serial entry.
    let _b = running(&coord, "b", &[], SERIAL_ALL);

    a.unregister();
}
