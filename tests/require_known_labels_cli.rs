//! `TestRunner::require_known_labels()`: with it, a `SKULD_LABELS` name that no
//! `#[skuld::label]` in the binary declares fails at startup. Without it, an
//! unknown name matches no test: `typo` selects nothing, but `!typo` selects
//! everything.

use std::ffi::OsStr;
use std::process::{Command, Output};

const KNOWN: &str = env!("CARGO_BIN_EXE_known_labels_fixture");

/// `bin --list` with the environment scrubbed, `SKULD_LABELS` set to `labels`
/// if given, and `lenient` dropping the fixture's opt-in.
fn spawn(bin: &str, labels: Option<&OsStr>, lenient: bool) -> Output {
    let mut cmd = Command::new(bin);
    cmd.arg("--list");
    for key in [
        "SKULD_LABELS",
        "SKULD_DEBUG",
        "SKULD_NEXTEST_METADATA_PATH",
        "KNOWN_LABELS_FIXTURE_LENIENT",
        "RUST_TEST_THREADS",
        "RUST_TEST_NOCAPTURE",
        "RUST_LOG",
        "RUST_BACKTRACE",
        "NEXTEST_EXECUTION_MODE",
        "NEXTEST_RUN_ID",
        "NEXTEST_BIN_EXE_NAME",
    ] {
        cmd.env_remove(key);
    }
    if let Some(v) = labels {
        cmd.env("SKULD_LABELS", v);
    }
    if lenient {
        cmd.env("KNOWN_LABELS_FIXTURE_LENIENT", "1");
    }
    cmd.output().expect("spawn fixture")
}

fn run(labels: &str, lenient: bool) -> Output {
    spawn(KNOWN, Some(OsStr::new(labels)), lenient)
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn unknown_label_fails_at_startup() {
    let out = run("nope", false);
    assert!(!out.status.success(), "expected startup failure for an unknown label");
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("skuld: SKULD_LABELS names unknown label(s) \"nope\"; declared labels: [\"alpha\", \"beta\"]"),
        "{stderr}"
    );
}

/// The message names each unknown label once, lowercased and sorted, and
/// lists the declared set once without the unknown names.
#[test]
fn the_message_is_deduplicated_sorted_and_lowercased() {
    let out = run("Zed | nope & ZED & Nope & alpha", false);
    assert!(
        stderr_of(&out).contains(
            "skuld: SKULD_LABELS names unknown label(s) \"nope\", \"zed\"; declared labels: [\"alpha\", \"beta\"]"
        ),
        "{}",
        stderr_of(&out)
    );
}

#[test]
fn unset_with_the_opt_in_lists_every_test() {
    let out = spawn(KNOWN, None, false);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let listed = String::from_utf8_lossy(&out.stdout);
    assert!(
        listed.contains("t_alpha: test") && listed.contains("t_beta: test"),
        "{listed}"
    );
}

#[test]
fn a_binary_declaring_no_labels_rejects_every_name() {
    let out = spawn(env!("CARGO_BIN_EXE_no_labels_fixture"), Some(OsStr::new("nope")), false);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("names unknown label(s) \"nope\"; declared labels: []"),
        "{}",
        stderr_of(&out)
    );
}

#[test]
fn unknown_label_is_found_even_where_the_filter_simplifies_it_away() {
    let out = run("alpha | nope | !nope", false);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("\"nope\""));
}

#[test]
fn every_unknown_label_is_named() {
    let out = run("nope & other", false);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("\"nope\"") && stderr.contains("\"other\""), "{stderr}");
}

#[test]
fn known_label_runs_normally() {
    for filter in ["alpha", "ALPHA", "alpha | beta", "!alpha", "true"] {
        let out = run(filter, false);
        assert!(
            out.status.success(),
            "{filter:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let listed = String::from_utf8_lossy(&run("alpha", false).stdout).into_owned();
    assert!(
        listed.contains("t_alpha: test") && !listed.contains("t_beta"),
        "{listed}"
    );
}

#[test]
fn without_the_opt_in_an_unknown_label_selects_nothing() {
    let out = run("nope", true);
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains(": test"));
}

#[test]
fn without_the_opt_in_a_negated_unknown_label_selects_everything() {
    let out = run("!nope", true);
    assert!(out.status.success());
    let listed = String::from_utf8_lossy(&out.stdout);
    assert!(
        listed.contains("t_alpha: test") && listed.contains("t_beta: test"),
        "{listed}"
    );
}

/// `nope` is the only unknown name in each filter.
#[test]
fn unknown_label_fails_wherever_it_sits_in_the_expression() {
    for filter in [
        "!nope",
        "(nope)",
        "!(alpha & (beta | nope))",
        "alpha & !(beta | nope)",
        "!!NoPe",
        "NOPE",
        "false & nope",
        "true | nope",
    ] {
        let out = run(filter, false);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{filter:?} must fail at startup:\n{stderr}");
        assert!(stderr.contains("\"nope\""), "{filter:?} must name \"nope\":\n{stderr}");
    }
}

/// `true` and `false` are constants, not label names; `true_ish` is a name.
#[test]
fn boolean_constants_are_not_label_names() {
    for filter in ["true", "false", "TRUE", "False", "alpha | false", "!true"] {
        let out = run(filter, false);
        assert!(
            out.status.success(),
            "{filter:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let out = run("true_ish", false);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("\"true_ish\""));
}

/// A malformed filter fails at the parse, not as an unknown name.
#[test]
fn malformed_filter_still_fails_with_the_opt_in() {
    for filter in ["nope &", "alpha nope", "", "   ", "nope)", "(alpha", "al-pha"] {
        let out = run(filter, false);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{filter:?} must fail:\n{stderr}");
        assert!(
            stderr.contains("skuld: SKULD_LABELS: invalid label expression"),
            "{filter:?} must report the parse error:\n{stderr}"
        );
    }
}

/// Not valid UTF-8 fails at the read, with or without the opt-in.
#[cfg(unix)]
#[test]
fn non_utf8_filter_fails_with_and_without_the_opt_in() {
    use std::os::unix::ffi::OsStrExt;

    for lenient in [false, true] {
        let out = spawn(KNOWN, Some(OsStr::from_bytes(b"al\xFFpha")), lenient);
        let stderr = stderr_of(&out);
        assert!(!out.status.success(), "lenient={lenient}: must fail:\n{stderr}");
        assert!(
            stderr.contains("skuld: SKULD_LABELS is not valid UTF-8"),
            "lenient={lenient}: {stderr}"
        );
    }
}
