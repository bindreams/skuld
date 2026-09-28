#[path = "../src/test_support.rs"]
mod fixture_lock;
use fixture_lock::lock_fixture_workspace;
use std::path::Path;
use std::process::{Child, Command};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cargo-skuld")
}

#[test]
fn cargo_shaped_argv_strips_the_subcommand_name() {
    // `cargo skuld nextest gen` execs this binary with argv[1] == "skuld";
    // running `cargo-skuld` directly does not. main() strips the former and
    // must leave the latter alone. Nesting the commands is what made that
    // strip necessary, and nothing else here covers it.
    let _guard = lock_fixture_workspace();
    let out_dir = tempfile::tempdir().expect("tempdir");
    let output = out_dir.path().join("skuld-nextest.toml");

    let status = Command::new(bin())
        .current_dir(_guard.root())
        .args(["skuld", "nextest", "gen", "--output"])
        .arg(&output)
        .status()
        .expect("spawn gen");
    assert!(status.success(), "cargo-shaped argv was rejected");
    assert!(output.exists(), "no config written via the cargo-shaped argv");
}

#[test]
fn gen_writes_groups_for_the_shared_resource_and_weird_name_conflicts() {
    let _guard = lock_fixture_workspace();
    let out_dir = tempfile::tempdir().expect("tempdir");
    let output = out_dir.path().join("skuld-nextest.toml");

    let status = Command::new(bin())
        .current_dir(_guard.root())
        .args(["nextest", "gen", "--output"])
        .arg(&output)
        .status()
        .expect("spawn gen");
    assert!(status.success());

    let rendered = std::fs::read_to_string(&output).expect("output file must exist");
    let value: toml::Value = rendered.parse().expect("valid TOML");
    let overrides = value["profile"]["default"]["overrides"]
        .as_array()
        .expect("overrides array");
    // Two independent conflicts: the shared-resource pair and the
    // weird-named escape-proof pair.
    assert_eq!(overrides.len(), 2, "expected exactly two conflict groups: {rendered}");
    let filters: Vec<&str> = overrides.iter().map(|o| o["filter"].as_str().unwrap()).collect();
    assert!(filters
        .iter()
        .any(|f| f.contains("a_uses_shared_resource") && f.contains("b_locks_shared_resource")));
    assert!(filters
        .iter()
        .any(|f| f.contains("weird") && f.contains("[a]") && f.contains("[b]")));
}

#[test]
fn gen_check_matches_after_gen() {
    let _guard = lock_fixture_workspace();
    let out_dir = tempfile::tempdir().expect("tempdir");
    let output = out_dir.path().join("skuld-nextest.toml");
    let gen_status = Command::new(bin())
        .current_dir(_guard.root())
        .args(["nextest", "gen", "--output"])
        .arg(&output)
        .status()
        .expect("spawn gen");
    assert!(gen_status.success());
    let check_status = Command::new(bin())
        .current_dir(_guard.root())
        .args(["nextest", "gen", "--check", "--output"])
        .arg(&output)
        .status()
        .expect("spawn gen --check");
    assert!(
        check_status.success(),
        "gen --check must succeed immediately after gen with no source changes"
    );
}

#[test]
fn gen_check_fails_on_stale_file() {
    let _guard = lock_fixture_workspace();
    let out_dir = tempfile::tempdir().expect("tempdir");
    let output = out_dir.path().join("skuld-nextest.toml");
    std::fs::write(&output, "# stale, does not match current test set\n").unwrap();
    let check_status = Command::new(bin())
        .current_dir(_guard.root())
        .args(["nextest", "gen", "--check", "--output"])
        .arg(&output)
        .status()
        .expect("spawn gen --check");
    assert!(!check_status.success(), "gen --check must fail on a stale file");
    assert!(
        std::fs::read_to_string(&output).unwrap().contains("stale"),
        "must not have overwritten the stale file"
    );
}

#[test]
fn gen_on_a_workspace_with_no_skuld_binaries_is_a_harmless_noop() {
    let empty_ws = tempfile::tempdir().expect("tempdir");
    std::fs::write(empty_ws.path().join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    let out_dir = tempfile::tempdir().expect("tempdir");
    let output = out_dir.path().join("skuld-nextest.toml");
    let status = Command::new(bin())
        .current_dir(empty_ws.path())
        .args(["nextest", "gen", "--output"])
        .arg(&output)
        .status()
        .expect("spawn gen");
    assert!(status.success());
    let value: toml::Value = std::fs::read_to_string(&output)
        .unwrap()
        .parse()
        .expect("valid TOML even with zero binaries");
    assert!(value.get("test-groups").is_none() || value["test-groups"].as_table().unwrap().is_empty());
}

fn read_window(dir: &Path, name: &str) -> (u128, u128) {
    let start: u128 = std::fs::read_to_string(dir.join(format!("{name}-proc-start")))
        .unwrap_or_else(|e| panic!("start marker for {name} missing: {e}"))
        .parse()
        .unwrap();
    let end: u128 = std::fs::read_to_string(dir.join(format!("{name}-proc-end")))
        .unwrap_or_else(|e| panic!("end marker for {name} missing: {e}"))
        .parse()
        .unwrap();
    (start, end)
}

fn windows_overlap(a: (u128, u128), b: (u128, u128)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// `std::process::Child` does not kill its process on drop, so a panic
/// between `spawn` and `wait` (e.g. a failed `assert!`) would leave the
/// fixture binary running after this test's stack unwinds. Declared after
/// the fixture lock guard at each call site so it drops — and reaps the
/// child — first, keeping it out of the fixture's target dir before the
/// next test can touch it.
struct KillOnDrop(Child);

impl KillOnDrop {
    fn spawn(cmd: &mut Command) -> std::io::Result<Self> {
        cmd.spawn().map(KillOnDrop)
    }

    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.0.wait()
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Repeatedly calls `check` until it returns `true` or `bound` elapses.
/// This is the permitted exception to "don't synchronize via time": a
/// child process's exit is exactly the kind of external event whose
/// precise timing isn't ours to control even when the code that triggers
/// it (`kill`) is correct — signal delivery and process teardown aren't
/// instantaneous. `bound` is a *failure* bound ("didn't happen within this
/// long"), not a guessed "long enough" duration a single check is bet on;
/// each iteration re-checks the real, current condition.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn poll_until(bound: std::time::Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + bound;
    loop {
        if check() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(target_os = "macos")]
fn macos_kill0(pid: u32) -> std::io::Result<()> {
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// The process's start time via `proc_pidinfo(PROC_PIDTBSDINFO)`, or the
/// OS error if it can't be read. NOT a zombie/reaped distinguisher on its
/// own — measured directly: `proc_pidinfo` returns `ESRCH` for an
/// unreaped zombie exactly the same as for a fully reaped (gone) pid,
/// which `ps -o stat=` (`Z`, `<defunct>`) confirms is still a real,
/// unreaped zombie at that point. `macos_confirm_reaped` combines this
/// with `macos_kill0` (which, unlike `proc_pidinfo`, *does* see a zombie)
/// to actually distinguish the two.
#[cfg(target_os = "macos")]
fn macos_process_start_time(pid: u32) -> std::io::Result<(u64, u64)> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as _,
            size,
        )
    };
    if ret <= 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok((info.pbi_start_tvsec, info.pbi_start_tvusec))
    }
}

/// True once `pid` is reaped: either gone entirely (`kill(pid, 0)` fails),
/// or now a *different* process (`proc_pidinfo`'s start time no longer
/// matches `original_start`). False for a zombie: `kill(pid, 0)` still
/// succeeds (a zombie occupies a pid slot until waited on) while
/// `proc_pidinfo` already can't see it — that combination is exactly the
/// "killed but not reaped" state a kill-only, no-`wait` mutant leaves
/// behind, and is the case this function must reject.
#[cfg(target_os = "macos")]
fn macos_confirm_reaped(pid: u32, original_start: (u64, u64)) -> bool {
    if macos_kill0(pid).is_err() {
        return true;
    }
    matches!(macos_process_start_time(pid), Ok(start) if start != original_start)
}

#[cfg(windows)]
fn windows_duplicate_handle(handle: windows_sys::Win32::Foundation::HANDLE) -> windows_sys::Win32::Foundation::HANDLE {
    use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let current = unsafe { GetCurrentProcess() };
    let mut dup = std::ptr::null_mut();
    let ok = unsafe { DuplicateHandle(current, handle, current, &mut dup, 0, 0, DUPLICATE_SAME_ACCESS) };
    assert!(ok != 0, "DuplicateHandle failed: {:?}", std::io::Error::last_os_error());
    dup
}

/// Guards `KillOnDrop` itself: that a panic between spawning a child and
/// explicitly `wait`ing on it still gets the child killed *and reaped*,
/// not just killed. Re-invokes this same test binary, filtered to just
/// this test under a magic env var, as the long-lived child — a small,
/// self-contained sleep instead of an external `sleep`/`timeout` binary,
/// which would need one implementation on Unix and a different one on
/// Windows.
///
/// Identifies the child by more than its bare pid (or, on Windows, a bare
/// handle *value*): once a pid is reaped, the kernel is free to hand it to
/// an unrelated process, and a check keyed on the bare pid alone can't
/// tell the two apart — it would see "something answers to this pid" and
/// wrongly call that "not reaped". Linux uses a `pidfd` (a stable
/// reference to the exact process instance, immune to pid reuse by
/// construction); macOS combines a liveness check that *does* see a
/// zombie with a start-time check that doesn't, since neither alone
/// distinguishes "reaped" from "zombie" (see `macos_confirm_reaped`);
/// Windows duplicates a handle before the kill, which keeps referring to
/// the exact same process object even after the original handle (owned by
/// the `Child` `KillOnDrop` wraps) is closed.
#[test]
fn kill_on_drop_reaps_the_child_even_if_the_scope_panics_before_wait() {
    if std::env::var_os("GEN_AND_RUN_KILL_ON_DROP_CHILD").is_some() {
        // Child mode: stay alive for up to 60s (never reached in a
        // passing run) unless killed first.
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    }

    let _guard = lock_fixture_workspace();
    let child = KillOnDrop::spawn(
        Command::new(std::env::current_exe().expect("current_exe"))
            .args([
                "kill_on_drop_reaps_the_child_even_if_the_scope_panics_before_wait",
                "--exact",
            ])
            .env("GEN_AND_RUN_KILL_ON_DROP_CHILD", "1"),
    )
    .expect("spawn long-lived child");
    let pid = child.0.id();

    // Captured while the child is known-alive (Command::spawn only
    // returns once the OS process exists), before the simulated panic.
    #[cfg(target_os = "linux")]
    let pidfd = rustix::process::pidfd_open(
        rustix::process::Pid::from_raw(pid as i32).expect("pid must be nonzero"),
        rustix::process::PidfdFlags::empty(),
    )
    .expect("pidfd_open while child is alive");
    #[cfg(target_os = "macos")]
    let original_start = macos_process_start_time(pid).expect("proc_pidinfo while child is alive");
    #[cfg(windows)]
    let dup_handle = {
        use std::os::windows::io::AsRawHandle;
        windows_duplicate_handle(child.0.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE)
    };

    // Simulate a panic between spawn() and an explicit wait() (e.g. a
    // failed assert!) — the exact scenario KillOnDrop exists for. `child`
    // must still be in scope (and therefore still get dropped, and
    // therefore killed) when the stack unwinds past this point.
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _keep_alive = &child;
        panic!("simulated failure between spawn() and wait()");
    }));
    assert!(unwound.is_err(), "the simulated panic must have actually panicked");
    drop(child); // KillOnDrop's Drop must kill and reap here.

    #[cfg(target_os = "linux")]
    {
        use rustix::io::Errno;
        use rustix::process::{waitid, WaitId, WaitIdOptions};
        use std::os::fd::AsFd;
        // ECHILD is Linux's answer once a pidfd's process has already been
        // waited on by someone else (KillOnDrop's own `wait`, here) — a
        // zombie is still waitable, and without NOWAIT this call would
        // reap it itself and report ECHILD on the very next call
        // regardless of whether KillOnDrop's own Drop ever ran — measured
        // directly: dropping NOWAIT here let the very first mutant-vs-
        // correct-code check pass on the kill-only mutant, because
        // checking became indistinguishable from correctly reaping.
        // NOWAIT makes this a non-destructive peek: a zombie keeps
        // reporting `Ok(Some(_))` on every call instead of being consumed
        // by the first one.
        let reaped = poll_until(std::time::Duration::from_secs(5), || {
            matches!(
                waitid(
                    WaitId::PidFd(pidfd.as_fd()),
                    WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT
                ),
                Err(Errno::CHILD)
            )
        });
        assert!(
            reaped,
            "pid {pid} must be reaped (waitid(P_PIDFD, WNOHANG) -> ECHILD) within 5s of \
             KillOnDrop's guard being dropped"
        );
    }
    #[cfg(target_os = "macos")]
    {
        let reaped = poll_until(std::time::Duration::from_secs(5), || {
            macos_confirm_reaped(pid, original_start)
        });
        assert!(
            reaped,
            "pid {pid} must be reaped within 5s of KillOnDrop's guard being dropped — still \
             signalable via kill(pid, 0) with no differing process identity to explain it \
             (looks like an unreaped zombie)"
        );
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
        // A real, bounded OS wait on the exact process object the
        // duplicated handle refers to — not a sleep-then-check guess.
        let wait = unsafe { WaitForSingleObject(dup_handle, 5_000) };
        assert_eq!(
            wait, WAIT_OBJECT_0,
            "process must exit within 5s of KillOnDrop's guard being dropped"
        );
        let mut exit_code = 0u32;
        let ok = unsafe { GetExitCodeProcess(dup_handle, &mut exit_code) };
        assert_ne!(
            ok,
            0,
            "GetExitCodeProcess failed: {:?}",
            std::io::Error::last_os_error()
        );
        assert_ne!(
            exit_code, STILL_ACTIVE as u32,
            "process must not report STILL_ACTIVE once WaitForSingleObject signaled it exited"
        );
        unsafe { CloseHandle(dup_handle) };
    }
}

#[test]
fn negative_control_two_directly_spawned_processes_overlap() {
    // Bypasses nextest's scheduler entirely — spawns the two conflicting
    // tests' binaries directly, back-to-back, so overlap is guaranteed by
    // construction (microseconds between spawns vs. each test's 200ms
    // body), not by scheduler luck. Validates the MEASUREMENT technique
    // (process-lifetime windows correctly detect two simultaneously-alive
    // processes); the positive case below validates nextest's own
    // scheduling behavior separately.
    let _guard = lock_fixture_workspace();
    let timing_dir = tempfile::tempdir().expect("tempdir");
    let binaries = cargo_skuld::discovery::discover_binaries(_guard.root()).expect("discovery");
    let bin_a = &binaries
        .iter()
        .find(|b| b.binary_id.contains("fixture-crate-a"))
        .expect("crate-a binary")
        .binary_path;
    let bin_b = &binaries
        .iter()
        .find(|b| b.binary_id.contains("fixture-crate-b"))
        .expect("crate-b binary")
        .binary_path;

    let mut child_a = KillOnDrop::spawn(
        Command::new(bin_a)
            .args(["a_uses_shared_resource", "--exact"])
            .env("SKULD_NEXTEST_FIXTURE_TIMING_DIR", timing_dir.path()),
    )
    .expect("spawn crate-a binary directly");
    let mut child_b = KillOnDrop::spawn(
        Command::new(bin_b)
            .args(["b_locks_shared_resource", "--exact"])
            .env("SKULD_NEXTEST_FIXTURE_TIMING_DIR", timing_dir.path()),
    )
    .expect("spawn crate-b binary directly");
    assert!(child_a.wait().expect("wait a").success());
    assert!(child_b.wait().expect("wait b").success());

    let a = read_window(timing_dir.path(), "a_uses_shared_resource");
    let b = read_window(timing_dir.path(), "b_locks_shared_resource");
    assert!(
        windows_overlap(a, b),
        "methodology check: two processes spawned back-to-back by this test were expected to \
         overlap but did not — a={a:?} b={b:?}"
    );
}

#[test]
fn run_serializes_the_cross_binary_conflict_via_generated_tool_config() {
    // Positive case: with the generated tool-config, nextest must not
    // launch the second process until the first has fully exited.
    let _guard = lock_fixture_workspace();
    let real_dir = tempfile::tempdir().expect("tempdir");
    let output_dir = tempfile::tempdir().expect("tempdir");
    let status = Command::new(bin())
        .current_dir(_guard.root())
        .env("SKULD_NEXTEST_FIXTURE_TIMING_DIR", real_dir.path())
        .args(["nextest", "run", "--output"])
        .arg(output_dir.path().join("skuld-nextest.toml"))
        .status()
        .expect("spawn cargo-skuld run");
    assert!(
        status.success(),
        "cargo-skuld run must succeed against the fixture workspace"
    );
    let real_a = read_window(real_dir.path(), "a_uses_shared_resource");
    let real_b = read_window(real_dir.path(), "b_locks_shared_resource");
    assert!(
        !windows_overlap(real_a, real_b),
        "conflicting tests' PROCESSES overlapped even with the generated tool-config: \
         a={real_a:?} b={real_b:?} — nextest did not serialize them via the group"
    );
}

/// Proves `escape_nextest_name`'s output is accepted by REAL nextest
/// parsing, not just by our own string assertions — both weird-named
/// tests must actually execute.
#[test]
fn run_correctly_selects_tests_with_special_characters_in_their_names() {
    let _guard = lock_fixture_workspace();
    let dir = tempfile::tempdir().expect("tempdir");
    let output_dir = tempfile::tempdir().expect("tempdir");
    let status = Command::new(bin())
        .current_dir(_guard.root())
        .env("SKULD_NEXTEST_FIXTURE_TIMING_DIR", dir.path())
        .args(["nextest", "run", "--output"])
        .arg(output_dir.path().join("skuld-nextest.toml"))
        .status()
        .expect("spawn run");
    assert!(status.success());
    assert!(
        dir.path().join("weird-a-ran").exists(),
        "the weird-named test in crate-a must have executed"
    );
    assert!(
        dir.path().join("weird-b-ran").exists(),
        "the weird-named test in crate-b must have executed"
    );
}
