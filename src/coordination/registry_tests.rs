//! Tests for the file-lock registry.

use std::fs::File;
#[cfg(windows)]
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering::SeqCst};

#[cfg(unix)]
use super::lock::lock_exclusive;
use super::lock::{open_lock_target, try_lock_exclusive};
use super::registry::{register, register_with, running_dir, try_register, IdGenerator, Registration, PROCESS_IDS};
use super::rendezvous::rendezvous;
use super::test_hooks::{retry_rendezvous, set_test_retry_hook, set_test_seam_hook, Seam};
use super::{SERIAL_ALL, SERIAL_NONE};
use crate::label::Label;

#[path = "../../tests/support/link.rs"]
mod link;

/// A fresh coordination directory, `<temp>/.skuld`, which does not exist yet.
pub(super) fn temp_coord() -> (crate::TempDir, PathBuf) {
    let dir = crate::TempDir::new().unwrap();
    let coord = dir.path().join(".skuld");
    (dir, coord)
}

/// A coordination directory `<temp>/link/.skuld` behind a directory link (a symlink on Unix, a
/// junction on Windows, which needs no privilege), and a function that retargets the link at
/// another directory. A registration made before the retarget is then behind
/// the old target, so unregistering it must fail. This is the one way to break a registration on
/// every platform: Windows refuses to delete a running test's entry, but not to retarget an
/// ancestor.
pub(super) fn linked_coord() -> (crate::TempDir, PathBuf, impl FnOnce()) {
    use link::{link_dir, retarget_dir};

    let dir = crate::TempDir::new().unwrap();
    let (real1, real2, link) = (
        dir.path().join("real1"),
        dir.path().join("real2"),
        dir.path().join("link"),
    );
    std::fs::create_dir(&real1).unwrap();
    std::fs::create_dir(&real2).unwrap();
    link_dir(&real1, &link);
    let coord = link.join(".skuld");
    let retarget = move || retarget_dir(&real2, &link);
    (dir, coord, retarget)
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

/// Another handle holding the entry `path`, as a registered test in another process would: a lock
/// on Unix, a write handle on Windows (a write handle is what makes an entry live there).
fn hold(path: &Path) -> File {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(super::lock::WINDOWS_SHARE_MODE);
    }
    let file = options.open(path).unwrap();
    #[cfg(unix)]
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
    let _owner = hold(&json);
    assert!(can_start_now(&coord, &[Label::__new("docker")], SERIAL_NONE));
    assert!(json.exists(), "a live entry stays");
}

const WARN_CHILD_DIR_ENV: &str = "SKULD_REGISTRY_TEST_WARN_CHILD_DIR";
const UNPARSEABLE_WARNING: &str = "skipping unparseable serial_filter";

/// The body of the process [`an_unparseable_filter_warns_once_per_process`] starts: scans three
/// times past one live entry with an unparseable filter.
#[test]
#[ignore = "a child body: run by an_unparseable_filter_warns_once_per_process"]
fn unparseable_filter_child() {
    let Some(coord) = std::env::var_os(WARN_CHILD_DIR_ENV).map(PathBuf::from) else {
        return;
    };
    let json = hand_made(&coord, "legacy.json", &entry_json("((("));
    let _owner = hold(&json);
    for _ in 0..3 {
        assert!(can_start_now(&coord, &[Label::__new("docker")], SERIAL_NONE));
    }
}

#[test]
fn an_unparseable_filter_warns_once_per_process() {
    let (_dir, coord) = temp_coord();
    let output = child_command("unparseable_filter_child")
        .env(WARN_CHILD_DIR_ENV, &coord)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{}: {stderr}", output.status);
    assert_eq!(stderr.matches(UNPARSEABLE_WARNING).count(), 1, "{stderr}");
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
    assert_eq!(running_files(&coord), vec![format!("{}.json", reg.id())]);
}

#[test]
fn unregister_removes_the_entry_and_is_idempotent() {
    let (_dir, coord) = temp_coord();
    let mut reg = running(&coord, "my_test", &[], SERIAL_NONE);
    reg.unregister();
    assert_eq!(running_files(&coord), Vec::<String>::new());
    reg.unregister();
}

// Scan =====

#[test]
fn an_entry_nobody_holds_is_removed() {
    let (_dir, coord) = temp_coord();
    let json = hand_made(&coord, "orphan.json", &entry_json("*"));
    assert!(
        can_start_now(&coord, &[], SERIAL_NONE),
        "a global-serial entry with no owner must not block"
    );
    assert!(!json.exists());
}

#[test]
fn a_live_entry_blocks_per_its_filter() {
    let (_dir, coord) = temp_coord();
    let json = hand_made(&coord, "live.json", &entry_json("docker"));
    let _owner = hold(&json);
    assert!(!can_start_now(&coord, &[Label::__new("docker")], SERIAL_NONE));
    assert!(can_start_now(&coord, &[], SERIAL_NONE));
    assert!(json.exists(), "a live entry stays");
}

#[test]
fn tmp_files_are_removed() {
    let (_dir, coord) = temp_coord();
    let tmp = hand_made(&coord, "a.json.tmp", &entry_json("*"));
    assert!(can_start_now(&coord, &[], SERIAL_ALL));
    assert!(!tmp.exists());
}

#[cfg(unix)]
#[test]
fn foreign_0644_entry_is_try_locked_read_only() {
    use std::os::unix::fs::PermissionsExt;

    let (_dir, coord) = temp_coord();
    let json = hand_made(&coord, "foreign.json", &entry_json("*"));
    // Read-only, like another uid's 0644 file: the scan must not open it for writing.
    std::fs::set_permissions(&json, std::fs::Permissions::from_mode(0o444)).unwrap();
    let owner = File::open(&json).unwrap();
    lock_exclusive(&owner).unwrap();

    assert!(!can_start_now(&coord, &[], SERIAL_NONE), "the held entry is live");

    drop(owner);
    assert!(
        can_start_now(&coord, &[], SERIAL_NONE),
        "released, the entry is dead and removed"
    );
}

/// An entry whose owner has deleted it while another process still has it open is gone, not live
/// and not an error: its name only lingers until that process closes it.
#[cfg(windows)]
#[test]
fn a_delete_pending_entry_is_skipped() {
    let (_dir, coord) = temp_coord();
    let json = hand_made(&coord, "pending.json", &entry_json("*"));
    let marked = crate::win_nt::test_support::mark_for_deletion(&json, false);
    assert!(
        can_start_now(&coord, &[], SERIAL_NONE),
        "a delete-pending entry must not block"
    );
    drop(marked);
    assert!(!json.exists());
}

#[test]
fn entry_with_unknown_fields_is_read() {
    let (_dir, coord) = temp_coord();
    hand_made(
        &coord,
        "future.json",
        r#"{"name":"future","labels":[],"filter":"*","added_later":{"x":1}}"#,
    );
    let _owner = hold(&running_dir(&coord).join("future.json"));
    assert!(!can_start_now(&coord, &[], SERIAL_NONE));
}

#[test]
#[should_panic(expected = "broken.json")]
fn unparseable_entry_panics_naming_the_file() {
    let (_dir, coord) = temp_coord();
    let json = hand_made(&coord, "broken.json", "not json");
    let _owner = hold(&json);
    let _ = attempt(&coord, "t", &[], SERIAL_NONE);
}

#[test]
fn colliding_id_retries_with_a_new_id() {
    let (_dir, coord) = temp_coord();
    let ids = IdGenerator::new();
    let colliding = ids.id_at(0);
    let json = hand_made(&coord, &format!("{colliding}.json"), &entry_json(""));
    let _owner = hold(&json);

    let mut reg = try_register(&ids, &coord, "t", &[], SERIAL_NONE).expect("nothing blocks it");
    assert_eq!(reg.id(), ids.id_at(1));
    assert!(json.exists(), "the live entry of the colliding id is not replaced");
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

/// How the Windows scan reads the result of its two filesystem attempts on an entry: the probe, an
/// open for reading with `FILE_SHARE_READ` only, which a write handle refuses (`Ok` means no
/// owner, a sharing violation an owner), and the delete of an entry that has no owner (`Ok` and a
/// sharing violation, another program holding it open, both mean gone). `ERROR_ACCESS_DENIED` is
/// told apart from delete-pending by a check.
#[cfg(windows)]
#[test]
fn filesystem_results_are_classified() {
    use super::registry::{classify_probe, classify_removal, Verdict};

    let os = io::Error::from_raw_os_error;
    let not_found = || io::Error::from(io::ErrorKind::NotFound);
    let unreached = || -> io::Result<bool> { panic!("the delete-pending check is only for a denied attempt") };
    type Attempt = dyn Fn(io::Result<()>, &dyn Fn() -> io::Result<bool>) -> Verdict;
    let probe: &Attempt = &|result, pending| classify_probe(result, pending);
    let removal: &Attempt = &|result, pending| classify_removal(result, pending);

    assert!(matches!(probe(Ok(()), &unreached), Verdict::Dead));
    assert!(matches!(removal(Ok(()), &unreached), Verdict::Gone));
    assert!(matches!(probe(Err(os(32)), &unreached), Verdict::Live));
    assert!(matches!(removal(Err(os(32)), &unreached), Verdict::Gone));
    for attempt in [probe, removal] {
        assert!(matches!(attempt(Err(not_found()), &unreached), Verdict::Gone));
        assert!(matches!(attempt(Err(os(5)), &|| Ok(true)), Verdict::Gone));
        // The last handle closed since the denial.
        assert!(matches!(attempt(Err(os(5)), &|| Err(not_found())), Verdict::Gone));
        // Denied and not delete-pending: this account may not touch it. Loud, never a silent wait.
        let Verdict::Fail(e) = attempt(Err(os(5)), &|| Ok(false)) else {
            panic!("an entry this account may not touch must fail");
        };
        assert!(e.to_string().contains("os error 5"), "{e}");
        let Verdict::Fail(e) = attempt(Err(os(5)), &|| Err(os(1117))) else {
            panic!("a failed check must fail");
        };
        let msg = e.to_string();
        assert!(
            msg.contains("os error 5") && msg.contains("os error 1117"),
            "both errors must be named: {msg}"
        );
        let Verdict::Fail(e) = attempt(Err(os(123)), &unreached) else {
            panic!("any other error fails");
        };
        assert!(e.to_string().contains("os error 123"), "{e}");
    }
}

/// Another program that locks the whole of a running test's entry (`LockFileEx`, which is mandatory
/// on Windows) makes the scan's read fail with a lock violation. As with a sharing violation, the
/// entry's constraints are unknown, so it blocks every registration for now rather than panicking.
#[cfg(windows)]
#[test]
fn a_locked_running_entry_blocks_every_registration() {
    let (_dir, coord) = temp_coord();
    let mut reg = running(&coord, "t", &[], SERIAL_NONE);
    let entry = running_dir(&coord).join(format!("{}.json", reg.id()));
    let other_program = std::fs::OpenOptions::new().read(true).open(&entry).unwrap();
    other_program.lock().unwrap();

    assert!(
        attempt(&coord, "u", &[], SERIAL_NONE).is_none(),
        "an entry that cannot be read blocks even a non-serial test"
    );

    other_program.unlock().unwrap();
    drop(other_program);
    assert!(
        can_start_now(&coord, &[], SERIAL_NONE),
        "readable again, it blocks nothing"
    );
    reg.unregister();
}

/// The warning for an entry that cannot be read names the file and what to do about it.
#[cfg(windows)]
#[test]
fn the_unreadable_entry_warning_names_the_file() {
    let warning = super::registry::unreadable_warning(Path::new(r"C:\x\a.json"));
    assert!(
        warning.starts_with("[skuld] warning:")
            && warning.contains(r"C:\x\a.json")
            && warning.contains("every test waits"),
        "{warning}"
    );
}

#[test]
fn a_warning_text_is_printed_once() {
    use super::registry::warning_once;

    let mut warned = std::collections::HashSet::new();
    assert_eq!(warning_once(&mut warned, "a".to_owned()).as_deref(), Some("a"));
    assert_eq!(warning_once(&mut warned, "a".to_owned()), None, "not twice");
    assert!(
        warning_once(&mut warned, "b".to_owned()).is_some(),
        "another text warns"
    );
}

/// Another program holding an entry open with share mode 0 (no sharing at all) refuses the scan's
/// probe like a live owner does, and refuses its read too. The scan cannot tell the entry's
/// constraints, so it treats it as a live global-serial entry until that program closes it, rather
/// than panicking.
#[cfg(windows)]
#[test]
fn an_entry_another_program_holds_exclusively_blocks_every_registration() {
    use std::os::windows::fs::OpenOptionsExt;

    let (_dir, coord) = temp_coord();
    let json = hand_made(&coord, "exclusive.json", &entry_json(""));
    let other_program = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&json)
        .unwrap();

    assert!(
        attempt(&coord, "t", &[], SERIAL_NONE).is_none(),
        "an entry that cannot be read blocks even a non-serial test"
    );
    assert!(json.exists());

    drop(other_program);
    assert!(
        can_start_now(&coord, &[], SERIAL_NONE),
        "released, the dead entry does not block"
    );
    assert!(!json.exists());
}

/// Liveness is the owner's hold, not the file: while it is registered the entry blocks, and once
/// the registration is dropped without unregistering (as a crash would leave it) the next scan
/// finds it dead and removes it.
#[test]
fn a_dropped_registration_leaves_a_dead_entry_the_next_scan_removes() {
    let (_dir, coord) = temp_coord();
    let reg = running(&coord, "t", &[], SERIAL_NONE);
    let entry = running_dir(&coord).join(format!("{}.json", reg.id()));
    assert!(
        attempt(&coord, "global", &[], SERIAL_ALL).is_none(),
        "the held entry blocks"
    );
    assert!(entry.exists());

    drop(reg);

    assert!(can_start_now(&coord, &[], SERIAL_ALL), "the dead entry must not block");
    assert!(!entry.exists(), "the scan removes it");
}

/// Another program with the entry open (an antivirus scanner, say) can stop its deletion, but
/// that does not make a finished test fail: the entry has no write handle, so it is dead and the
/// next scan removes it.
#[cfg(windows)]
#[test]
fn unregister_leaves_an_entry_another_program_holds_open_to_the_next_scan() {
    use std::os::windows::fs::OpenOptionsExt;

    let (_dir, coord) = temp_coord();
    let mut reg = running(&coord, "t", &[], SERIAL_NONE);
    let entry = running_dir(&coord).join(format!("{}.json", reg.id()));
    let other_program = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(super::lock::WINDOWS_SHARE_MODE)
        .open(&entry)
        .unwrap();

    reg.unregister();

    assert!(
        entry.exists(),
        "it cannot be deleted while the other program has it open"
    );
    assert!(
        can_start_now(&coord, &[], SERIAL_ALL),
        "an entry nobody holds for writing is dead, so it does not block"
    );
    drop(other_program);
    assert!(can_start_now(&coord, &[], SERIAL_ALL));
    assert!(!entry.exists(), "the next scan removes it");
}

// Mutex =====

#[test]
fn entry_is_created_under_the_mutex() {
    let (_dir, coord) = temp_coord();
    let _hook = set_test_seam_hook(Seam::Created, {
        let coord = coord.clone();
        move || {
            let fresh = open_lock_target(&coord);
            assert!(
                matches!(try_lock_exclusive(&fresh), Err(std::fs::TryLockError::WouldBlock)),
                "the mutex must be held while the entry is created"
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

/// This test binary, set to run the ignored child body `test` of this module and nothing else.
/// Child bodies are `#[ignore]`d so that they never run as tests of their own, and return at once
/// without their environment variable, so `--include-ignored` runs stay green.
fn child_command(test: &str) -> std::process::Command {
    let module = module_path!().split_once("::").expect("module path has a crate").1;
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--ignored", "--exact", &format!("{module}::{test}"), "--nocapture"]);
    cmd
}

/// The body of the process [`spawn_registered_child`] starts: registers `SERIAL_ALL` in
/// `CHILD_DIR_ENV`, reports, and holds the registration until killed.
#[test]
#[ignore = "a child body: run by spawn_registered_child"]
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
    use std::process::Stdio;

    let mut cmd = child_command("registered_child");
    cmd.env(CHILD_DIR_ENV, coord)
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
    assert_eq!(child_files.len(), 1, "the child holds one entry: {child_files:?}");
    assert!(
        !can_start_now(&coord, &[], SERIAL_NONE),
        "a live SERIAL_ALL child blocks"
    );

    child.kill().unwrap();
    child.wait().unwrap();

    // Not `attempt`: Windows releases a dead process's locks asynchronously, so the first scans may
    // still see the lock held.
    let mut reg = register(&coord, "parent", &[], SERIAL_ALL);
    assert_eq!(
        running_files(&coord),
        vec![format!("{}.json", reg.id())],
        "the child's entry must be gone"
    );
    reg.unregister();
}

#[cfg(unix)]
const FD_CHILD_DIR_ENV: &str = "SKULD_REGISTRY_TEST_FD_CHILD_DIR";
#[cfg(unix)]
const FD_CHILD_DONE: &str = "child-unregistered";

/// The body of the process [`unregister_needs_no_new_file_descriptor`] starts: registers, spends
/// every free file descriptor, then unregisters.
#[cfg(unix)]
#[test]
#[ignore = "a child body: run by unregister_needs_no_new_file_descriptor"]
fn unregistering_child() {
    let Some(dir) = std::env::var_os(FD_CHILD_DIR_ENV) else {
        return;
    };
    let mut reg = register(Path::new(&dir), "child", &[], SERIAL_NONE);
    assert_eq!(running_files(Path::new(&dir)).len(), 1, "registered: one entry");

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
    let output = child_command("unregistering_child")
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
        "unregistration removes the entry"
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

/// The panic of unregistering `reg`, as its message.
fn unregister_panic(reg: &mut Registration) -> String {
    let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reg.unregister()))
        .expect_err("unregistering a moved registration must panic");
    crate::coordination::panic_payload_message(payload.as_ref()).to_owned()
}

#[cfg(unix)]
#[test]
fn unregister_panics_naming_the_entry_when_running_dir_was_replaced() {
    let (_dir, coord) = temp_coord();
    let mut reg = running(&coord, "t", &[], SERIAL_NONE);
    let aside = coord.join("running.aside");
    std::fs::rename(running_dir(&coord), &aside).unwrap();
    std::fs::create_dir(running_dir(&coord)).unwrap();

    let msg = unregister_panic(&mut reg);
    assert!(
        msg.contains(&format!("{}.json", reg.id())) && msg.contains("deleted, replaced, or its directory moved"),
        "{msg}"
    );
}

/// A running test's entry cannot be deleted, so deleting `.skuld/` fails instead of splitting the
/// run; a registration that conflicts with it still waits.
#[cfg(windows)]
#[test]
fn entry_cannot_be_deleted_while_registered() {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    let (_dir, coord) = temp_coord();
    let mut reg = running(&coord, "t", &[], SERIAL_NONE);
    let err = std::fs::remove_file(running_dir(&coord).join(format!("{}.json", reg.id())))
        .expect_err("Windows must refuse to delete an entry its owner holds open");
    assert_eq!(err.raw_os_error(), Some(ERROR_SHARING_VIOLATION), "{err}");
    assert!(
        attempt(&coord, "global", &[], SERIAL_ALL).is_none(),
        "the entry survived the delete attempt, so it still blocks"
    );
    reg.unregister();
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

/// A scanner whose mutex has split (it holds the lock on the replaced directory) must not list,
/// delete or create anything in the new copy; the final check then reports the move.
#[cfg(unix)]
#[test]
fn a_split_scanner_leaves_the_new_directory_untouched() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let (_dir, coord) = temp_coord();
    let holds = Rc::new(RefCell::new(Vec::new()));
    let _hook = set_test_seam_hook(Seam::Scanning, {
        let (coord, holds) = (coord.clone(), holds.clone());
        move || {
            split(&coord);
            std::fs::create_dir(running_dir(&coord)).unwrap();
            hand_made(&coord, "stale.json.tmp", "partial");
            hand_made(&coord, "dead.json", &entry_json("*"));
            let live = hand_made(&coord, "live.json", &entry_json(""));
            holds.borrow_mut().push(hold(&live));
        }
    });
    let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| attempt(&coord, "t", &[], SERIAL_NONE)))
        .err()
        .expect("a split scanner must panic");
    let msg = crate::coordination::panic_payload_message(payload.as_ref());
    assert!(msg.contains("deleted, replaced, or its directory moved"), "{msg}");

    assert_eq!(running_files(&coord), ["dead.json", "live.json", "stale.json.tmp"]);
    assert_eq!(
        std::fs::read_to_string(running_dir(&coord).join("dead.json")).unwrap(),
        entry_json("*")
    );
}

/// An entry that cannot be inspected (here, `running/` became a file) is reported with the error,
/// not as a move.
#[cfg(unix)]
#[test]
fn unregister_panics_with_the_error_when_the_entry_cannot_be_inspected() {
    let (_dir, coord) = temp_coord();
    let mut reg = running(&coord, "t", &[], SERIAL_NONE);
    std::fs::rename(running_dir(&coord), coord.join("running.aside")).unwrap();
    std::fs::write(running_dir(&coord), "").unwrap();

    let msg = unregister_panic(&mut reg);
    assert!(
        msg.contains("failed to inspect")
            && msg.contains(&format!("{}.json", reg.id()))
            && msg.contains("Not a directory")
            && !msg.contains("deleted, replaced"),
        "{msg}"
    );
}

// Directories changed between creation and locking =====

/// What `attempt` panicked with.
fn attempt_panic(coord: &Path) -> String {
    let payload = std::panic::catch_unwind(|| attempt(coord, "t", &[], SERIAL_NONE))
        .err()
        .expect("the attempt must panic");
    crate::coordination::panic_payload_message(payload.as_ref()).to_owned()
}

/// `running/` deleted after the attempt created it: only something outside skuld does that, so
/// the attempt fails naming the directory instead of registering where other tests may not look.
#[test]
fn a_running_dir_deleted_before_the_lock_panics_naming_it() {
    let (_dir, coord) = temp_coord();
    let _hook = set_test_seam_hook(Seam::RunningCreated, {
        let coord = coord.clone();
        move || std::fs::remove_dir(running_dir(&coord)).unwrap()
    });
    let msg = attempt_panic(&coord);
    assert!(
        msg.contains(&format!("{:?}", running_dir(&coord))) && msg.contains("SKULD_DB_DIR"),
        "{msg}"
    );
}

/// A coordination directory replaced between opening the mutex target and locking it panics
/// naming the move, as a split anywhere else does.
#[test]
fn a_directory_replaced_before_the_lock_is_taken_panics_naming_the_move() {
    let (_dir, coord, retarget) = linked_coord();
    let _hook = set_test_seam_hook(Seam::TargetOpened, retarget);
    let msg = attempt_panic(&coord);
    assert!(
        msg.contains(" moved while registering") && msg.contains("deleted, replaced, or its directory moved"),
        "{msg}"
    );
}

/// A mutex target that cannot be inspected fails the check naming the error, without claiming
/// the directory moved.
#[cfg(unix)]
#[test]
fn an_uninspectable_mutex_target_fails_unregistration_naming_the_error() {
    use std::os::unix::fs::PermissionsExt;

    assert_ne!(
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() },
        0,
        "root ignores the permission this test removes"
    );
    let (dir, coord) = temp_coord();
    let mut reg = running(&coord, "t", &[], SERIAL_NONE);
    let parent = coord.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).unwrap();
    let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reg.unregister())).err();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    drop((reg, dir));
    let payload = payload.expect("an uninspectable mutex target must panic");
    let msg = crate::coordination::panic_payload_message(payload.as_ref());
    assert!(
        msg.contains("could not be checked")
            && msg.contains("Permission denied")
            && msg.contains("SKULD_DB_DIR")
            && !msg.contains(" moved while"),
        "{msg}"
    );
}

/// A `running/` that cannot be listed (here a dangling symlink, which directory creation accepts
/// as present) fails with the error.
#[cfg(unix)]
#[test]
fn a_running_dir_that_is_a_dangling_symlink_panics_with_the_error() {
    let (_dir, coord) = temp_coord();
    std::fs::create_dir_all(&coord).unwrap();
    std::os::unix::fs::symlink(coord.join("nowhere"), running_dir(&coord)).unwrap();

    let msg = attempt_panic(&coord);
    assert!(
        msg.contains(&format!("{:?}", running_dir(&coord))) && msg.contains("No such file"),
        "{msg}"
    );
}

/// The whole coordination directory deleted after the attempt created it fails naming it.
#[test]
fn a_coordination_dir_deleted_before_the_lock_panics_naming_it() {
    let (_dir, coord) = temp_coord();
    let _hook = set_test_seam_hook(Seam::RunningCreated, {
        let coord = coord.clone();
        move || std::fs::remove_dir_all(&coord).unwrap()
    });
    let msg = attempt_panic(&coord);
    let coord_shown = format!("{coord:?}");
    assert!(
        msg.contains(coord_shown.trim_end_matches('"')) && msg.contains("SKULD_DB_DIR"),
        "{msg}"
    );
}

/// A symlink ancestor of `coord` retargeted mid-run leaves the registration's entry behind the
/// old target, so unregistering must fail rather than report success against whatever the path
/// names now. Windows withholds `FILE_SHARE_DELETE` from the entry, but that does not stop an
/// ancestor being retargeted.
#[test]
fn unregister_panics_when_a_parent_symlink_is_retargeted_mid_run() {
    let (_dir, coord, retarget) = linked_coord();
    let mut a = running(&coord, "a", &[], SERIAL_ALL);
    retarget();

    // `b` registers through the retargeted link, in a coordination directory `a` knows nothing
    // of, so it must not wait on `a`'s global-serial entry.
    let _b = running(&coord, "b", &[], SERIAL_ALL);

    let msg = unregister_panic(&mut a);
    assert!(msg.contains("deleted, replaced, or its directory moved"), "{msg}");
}
