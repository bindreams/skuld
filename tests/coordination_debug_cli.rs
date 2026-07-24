//! End-to-end tests for coordinate()'s first-genuine-wait debug
//! diagnostic, via `coordination_debug_fixture`. Contention between the
//! two globally-serial tests is engineered deterministically: `hold_a`
//! blocks on stdin once registered, so the driver can guarantee `hold_b`'s
//! subprocess only starts once `hold_a` is confirmably still registered —
//! no sleep, no poll-with-timeout, just blocking pipe reads on real events.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn spawn_holder(skuld_debug: bool) -> (std::process::Child, BufReader<std::process::ChildStdout>) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_coordination_debug_fixture"));
    cmd.args(["hold_a", "--exact", "--nocapture"]);
    cmd.env_remove("SKULD_LABELS");
    if skuld_debug {
        cmd.env("SKULD_DEBUG", "1");
    } else {
        cmd.env_remove("SKULD_DEBUG");
    }
    let mut holder = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn holder");
    let mut stdout = BufReader::new(holder.stdout.take().unwrap());
    // libtest-mimic writes its own preamble ("\n", "running 1 test\n") to
    // stdout before the test body runs, so REGISTERED isn't necessarily the
    // first line. Block-read line by line (no timeout, no poll) until it
    // appears.
    let mut line = String::new();
    loop {
        line.clear();
        let n = stdout.read_line(&mut line).expect("read from holder stdout");
        assert!(n > 0, "holder stdout closed before confirming registration");
        if line.trim() == "REGISTERED" {
            break;
        }
    }
    (holder, stdout)
}

fn spawn_waiter(skuld_debug: bool) -> std::process::Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_coordination_debug_fixture"));
    cmd.args(["hold_b", "--exact", "--nocapture"]);
    cmd.env_remove("SKULD_LABELS");
    if skuld_debug {
        cmd.env("SKULD_DEBUG", "1");
    } else {
        cmd.env_remove("SKULD_DEBUG");
    }
    cmd.stderr(Stdio::piped()).spawn().expect("spawn waiter")
}

#[test]
fn first_wait_logs_a_debug_diagnostic() {
    // The holder is confirmably registered before we spawn the waiter, so
    // the waiter's first coordinate() attempt is guaranteed — not merely
    // likely — to hit contention.
    let (mut holder, _holder_stdout) = spawn_holder(true);
    let mut waiter = spawn_waiter(true);

    // Block until we see the diagnostic in the waiter's stderr — a real
    // blocking read on the pipe, not a timeout.
    let mut waiter_stderr = BufReader::new(waiter.stderr.take().unwrap());
    let mut found = false;
    let mut collected = String::new();
    let mut line = String::new();
    while waiter_stderr.read_line(&mut line).expect("read waiter stderr") > 0 {
        collected.push_str(&line);
        if line.contains("blocked on a serial constraint") {
            found = true;
            break;
        }
        line.clear();
    }
    assert!(
        found,
        "expected the first-wait debug diagnostic in waiter stderr; got:\n{collected}"
    );

    holder
        .stdin
        .take()
        .unwrap()
        .write_all(b"RELEASE\n")
        .expect("release holder");
    assert!(holder.wait().expect("wait holder").success());
    assert!(waiter.wait().expect("wait waiter").success());
}

#[test]
fn no_diagnostic_without_skuld_debug() {
    let (mut holder, _holder_stdout) = spawn_holder(false);
    let waiter = spawn_waiter(false);

    // Release immediately — this assertion is checked over the waiter's
    // FULL captured stderr after it exits, so it holds regardless of
    // whether the waiter's attempt happens to land before or after
    // release; it is not a timing-dependent check.
    holder
        .stdin
        .take()
        .unwrap()
        .write_all(b"RELEASE\n")
        .expect("release holder");

    let holder_out = holder.wait_with_output().expect("wait holder");
    let waiter_out = waiter.wait_with_output().expect("wait waiter");
    assert!(holder_out.status.success());
    assert!(waiter_out.status.success());
    let waiter_stderr = String::from_utf8_lossy(&waiter_out.stderr);
    assert!(
        !waiter_stderr.contains("blocked on a serial constraint"),
        "diagnostic must be gated behind SKULD_DEBUG; got:\n{waiter_stderr}"
    );
}
