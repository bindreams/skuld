//! Fixture-carried labels: a test's labels are its own plus those of every
//! fixture it uses, transitively.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

fn command(labels: Option<&str>) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fixture_labels_fixture"));
    for key in [
        "SKULD_LABELS",
        "SKULD_DEBUG",
        "SKULD_NEXTEST_METADATA_PATH",
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
    cmd
}

/// The nextest metadata dump for a `--list` run under `labels`.
fn dump(labels: &str) -> Vec<serde_json::Value> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("meta.json");
    let out = command(Some(labels))
        .arg("--list")
        .env("SKULD_NEXTEST_METADATA_PATH", &path)
        .output()
        .expect("spawn fixture_labels_fixture");
    assert!(
        out.status.success(),
        "fixture_labels_fixture failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("metadata dump")).expect("valid JSON");
    json["tests"].as_array().expect("tests array").clone()
}

fn listed(labels: &str) -> BTreeSet<String> {
    dump(labels)
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

/// The exact `labels` array of each test in the dump, in order, so a
/// duplicate shows.
fn labels_of(tests: &[serde_json::Value], name: &str) -> Vec<String> {
    let t = tests
        .iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("{name} missing from the dump"));
    t["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn fixture_labels_select_the_test() {
    assert_eq!(
        listed("leaf"),
        set(&[
            "t_direct",
            "t_transitive",
            "t_two_hops",
            "t_diamond",
            "t_own_equals_fixture_label",
            "t_explicit_empty",
            "t_own_plus_fixture",
            "t_default_plus_fixture",
            "t_default_dropped",
            "hold_labeled",
        ])
    );
}

#[test]
fn fixture_labels_are_transitive() {
    assert_eq!(listed("mid"), set(&["t_transitive", "t_two_hops"]));
}

#[test]
fn own_labels_and_fixture_labels_combine() {
    assert_eq!(listed("own"), set(&["t_own_plus_fixture", "t_own_repeated"]));
    assert_eq!(listed("own & leaf"), set(&["t_own_plus_fixture"]));
}

#[test]
fn a_module_default_and_fixture_labels_combine_under_filtering() {
    assert_eq!(listed("modl"), set(&["t_default_plus_fixture"]));
    assert_eq!(listed("modl & leaf"), set(&["t_default_plus_fixture"]));
}

#[test]
fn a_module_default_does_not_reach_a_sibling_module_sharing_its_prefix() {
    let tests = dump("true");
    assert_eq!(labels_of(&tests, "t_sibling_not_defaulted"), Vec::<String>::new());
}

#[test]
fn tests_without_labeled_fixtures_are_unselected() {
    assert_eq!(
        listed("!leaf & !mid & !own & !rep & !modl"),
        set(&[
            "t_none",
            "t_plain_fixture",
            "t_sibling_not_defaulted",
            "wait_serial_leaf"
        ])
    );
}

/// Exact, ordered arrays: own labels first, then fixture labels in visit
/// order, each label once.
#[test]
fn fixture_labels_reach_the_nextest_metadata() {
    let tests = dump("true");
    let l = |name: &str| labels_of(&tests, name);
    assert_eq!(l("t_direct"), ["leaf"]);
    assert_eq!(l("t_transitive"), ["mid", "leaf"]);
    assert_eq!(l("t_two_hops"), ["mid", "leaf"]);
    assert_eq!(l("t_diamond"), ["leaf"]);
    assert_eq!(l("t_repeated_fixture_label"), ["rep"]);
    assert_eq!(l("t_own_equals_fixture_label"), ["leaf"]);
    assert_eq!(l("t_own_repeated"), ["own"]);
    assert_eq!(l("t_own_plus_fixture"), ["own", "leaf"]);
    assert_eq!(l("t_none"), Vec::<String>::new());
    assert_eq!(l("t_plain_fixture"), Vec::<String>::new());
    let serial = tests.iter().find(|t| t["name"] == "wait_serial_leaf").unwrap();
    assert_eq!(serial["serial_filter"], "leaf");
}

/// `labels = []` drops the module default and keeps the fixture's labels.
#[test]
fn explicit_empty_labels_drop_only_the_module_default() {
    let tests = dump("true");
    assert_eq!(labels_of(&tests, "t_default_plus_fixture"), ["modl", "leaf"]);
    assert_eq!(labels_of(&tests, "t_explicit_empty"), ["leaf"]);
    assert_eq!(labels_of(&tests, "t_default_dropped"), ["leaf"]);
}

/// Kills and reaps the holder if an assertion unwinds before it is released.
struct Holder(Child);

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A test that is `leaf` only through a fixture is subject to another test's
/// `serial = leaf`: the serial test blocks while the holder is registered.
#[test]
fn fixture_labels_drive_serial_filters() {
    let mut holder = Holder(
        command(None)
            .args(["hold_labeled", "--exact", "--nocapture"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn holder"),
    );
    let mut holder_stdout = BufReader::new(holder.0.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            holder_stdout.read_line(&mut line).expect("read holder stdout") > 0,
            "holder stdout closed before confirming registration"
        );
        if line.trim() == "REGISTERED" {
            break;
        }
    }

    let mut waiter = Holder(
        command(None)
            .args(["wait_serial_leaf", "--exact", "--nocapture"])
            .env("SKULD_DEBUG", "1")
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn waiter"),
    );
    let mut waiter_stderr = BufReader::new(waiter.0.stderr.take().unwrap());
    let mut collected = String::new();
    let mut found = false;
    line.clear();
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
        "the serial=leaf test must block on the fixture-labeled holder; stderr:\n{collected}"
    );

    holder
        .0
        .stdin
        .take()
        .unwrap()
        .write_all(b"RELEASE\n")
        .expect("release holder");
    // Read both pipes to EOF before `wait()`, so a full pipe cannot block the child.
    std::io::copy(&mut waiter_stderr, &mut std::io::sink()).expect("drain waiter stderr");
    std::io::copy(&mut holder_stdout, &mut std::io::sink()).expect("drain holder stdout");
    assert!(holder.0.wait().expect("wait holder").success());
    assert!(waiter.0.wait().expect("wait waiter").success());
}
