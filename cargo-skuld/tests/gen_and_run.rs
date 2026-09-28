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

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
compile_error!(
    "kill_on_drop_reaps_the_child_even_if_the_scope_panics_before_wait's reaping check is only \
     implemented for Linux, macOS, and Windows — add a case for this target before enabling the \
     test here, rather than letting it compile and silently assert nothing"
);

/// Runs `drop(child)` on a background thread and blocks up to `bound` for
/// it to finish, panicking instead of hanging the whole test binary if it
/// doesn't. `KillOnDrop::drop` calls `Child::wait()`, which is itself
/// unbounded: a Drop that only waits and never kills would block here
/// forever, since the child (see child mode below) never exits on its
/// own. This is what turns that hang into an observable test failure —
/// not a condition worth polling, just one call whose own completion
/// isn't guaranteed.
fn drop_bounded(child: KillOnDrop, bound: std::time::Duration) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(child);
        let _ = tx.send(());
    });
    rx.recv_timeout(bound).unwrap_or_else(|_| {
        panic!(
            "KillOnDrop's Drop did not return within {bound:?} — a Drop that only waits and \
             never kills would hang here forever, since the child never exits on its own"
        )
    });
}

/// Guards `KillOnDrop` itself: that a panic between spawning a child and
/// explicitly `wait`ing on it still gets the child killed *and reaped*,
/// not just killed. Re-invokes this same test binary, filtered to just
/// this test under a magic env var, as the long-lived child — a small,
/// self-contained park loop instead of an external `sleep`/`timeout`
/// binary, which would need one implementation on Unix and a different
/// one on Windows. Parks forever rather than sleeping for a fixed
/// duration: a Drop that only waits and never kills (mutant M2) would
/// otherwise pass once the child eventually exited on its own, since
/// `wait()` would then return successfully — just slowly. Bounded by
/// `drop_bounded`, so the resulting hang is an observable test failure.
///
/// Checked once, immediately after `drop_bounded` returns, not polled:
/// `KillOnDrop::drop` calls `Child::wait()` synchronously, so a correct
/// Drop has already fully reaped the child by the time it returns — a
/// polling window only gives a *broken* Drop (e.g. one that kills and
/// reaps on a detached thread instead of inline) more time to
/// accidentally still succeed within it (measured: a zero-duration bound
/// passed 15/15 against correct code, while a detached-reap-after-50ms
/// mutant passed inside a 5s polling window).
///
/// Identifies the child by more than its bare pid (or, on Windows, a bare
/// handle *value*): once a pid is reaped, the kernel is free to hand it to
/// an unrelated process, and a check keyed on the bare pid alone can't
/// tell the two apart. Linux uses a `pidfd` (a stable reference to the
/// exact process instance, immune to pid reuse by construction). macOS
/// uses `waitid(P_PID, pid, WEXITED | WNOHANG | WNOWAIT)`: `ECHILD` means
/// "pid is not an unreaped child of this process", i.e. reaped — checked
/// against *our own* child list, not "does some process have this pid"
/// (what `kill(pid, 0)` or `proc_pidinfo` would answer), which is what
/// closes the reuse gap even if the OS recycles the pid to an unrelated
/// process elsewhere on the system. The one remaining gap — another child
/// of *this same process* taking the freed pid before the check runs — is
/// closed by convention, not by this function: every `gen_and_run.rs` test
/// that spawns a child holds `_guard` (the fixture's cross-process
/// `flock`, which also excludes concurrent threads within this same
/// process, since each acquisition opens its own file description) before
/// spawning, so no other child of this process can be spawned while this
/// test holds it. Windows clones a handle before the kill via
/// `try_clone_to_owned` (not `DuplicateHandle` directly: the returned
/// `OwnedHandle` closes itself on drop, including if an assert below
/// panics first, instead of needing a manual, easy-to-skip `CloseHandle`)
/// — the clone keeps referring to the exact same process object even
/// after the original handle (owned by the `Child` `KillOnDrop` wraps) is
/// closed.
#[test]
fn kill_on_drop_reaps_the_child_even_if_the_scope_panics_before_wait() {
    if std::env::var_os("GEN_AND_RUN_KILL_ON_DROP_CHILD").is_some() {
        // Child mode: park forever unless killed first — see this
        // function's own doc for why not a fixed-duration sleep.
        loop {
            std::thread::park();
        }
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
    #[cfg(windows)]
    let cloned_handle: std::os::windows::io::OwnedHandle = {
        use std::os::windows::io::AsHandle;
        child
            .0
            .as_handle()
            .try_clone_to_owned()
            .expect("clone process handle while child is alive")
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
    drop_bounded(child, std::time::Duration::from_secs(10)); // KillOnDrop's Drop must kill and reap here.

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
        // directly: dropping NOWAIT here let the very first version of
        // this check pass on the kill-only mutant, because checking
        // became indistinguishable from correctly reaping. NOWAIT makes
        // this a non-destructive peek: a zombie keeps reporting
        // `Ok(Some(_))` instead of being consumed by the first call.
        let reaped = matches!(
            waitid(
                WaitId::PidFd(pidfd.as_fd()),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT
            ),
            Err(Errno::CHILD)
        );
        assert!(
            reaped,
            "pid {pid} must already be reaped (waitid(P_PIDFD, WNOHANG | WNOWAIT) -> ECHILD) \
             immediately after KillOnDrop's guard was dropped"
        );
    }
    #[cfg(target_os = "macos")]
    {
        use rustix::io::Errno;
        use rustix::process::{waitid, Pid, WaitId, WaitIdOptions};
        let rpid = Pid::from_raw(pid as i32).expect("pid must be nonzero");
        let reaped = matches!(
            waitid(
                WaitId::Pid(rpid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT
            ),
            Err(Errno::CHILD)
        );
        assert!(
            reaped,
            "pid {pid} must no longer be an unreaped child of this process (waitid(P_PID, \
             WNOHANG | WNOWAIT) -> ECHILD) immediately after KillOnDrop's guard was dropped"
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::{STILL_ACTIVE, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
        let raw = cloned_handle.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
        // Zero timeout: an asynchronous TerminateProcess that hasn't
        // actually finished yet must show up as not-yet-signaled right
        // now, not eventually — a positive timeout here would let a
        // kill-only mutant's async termination complete somewhere inside
        // the wait window and pass anyway.
        let wait = unsafe { WaitForSingleObject(raw, 0) };
        assert_eq!(
            wait, WAIT_OBJECT_0,
            "process must already be signaled (exited) immediately after KillOnDrop's guard was \
             dropped — Child::wait() inside Drop is synchronous, so a correct Drop leaves \
             nothing left to wait for here"
        );
        let mut exit_code = 0u32;
        let ok = unsafe { GetExitCodeProcess(raw, &mut exit_code) };
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
        // cloned_handle drops here (RAII), closing the handle even if an
        // assert above panicked first.
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
