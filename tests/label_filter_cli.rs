//! End-to-end tests for `SKULD_LABELS` filtering.
//!
//! Runtime-only behaviors. Compile-fail scenarios belong in
//! `tests/compile_errors.rs`. Each test spawns the `label_filter_fixture`
//! subprocess (see `tests/support_bins/label_filter_fixture/main.rs`)
//! with a specific `SKULD_LABELS` value and asserts which tests ran (via
//! marker files in a tempdir) and which appeared as `ignored` or
//! `ok`/`failed` (via libtest-mimic's `--format json` events).
//!
//! The plan for this suite lives at
//! `C:\Users\bindreams\.claude\plans\peppy-meandering-snail.md`.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

// Shared infrastructure =================================================================================
//
// No driver-level mutex. All skuld binaries in the workspace share the same
// `.skuld.db` (path is compile-time baked via `SKULD_TARGET_PROFILE_DIR` at
// build.rs:11), but the WAL-init retry loop at src/coordination.rs:74-82
// already handles concurrent opens across processes. `tests/capture_cli.rs`
// uses the same pattern with no mutex; staying consistent with it.

/// Captured outcome of one fixture run.
struct RunOutcome {
    /// Names of tests whose body actually executed (marker file written).
    markers: HashSet<String>,
    /// Names libtest-mimic reported as ignored.
    ignored: Vec<String>,
    /// Names libtest-mimic reported as passed (`ok`).
    passed: Vec<String>,
    /// Names libtest-mimic reported as failed.
    failed: Vec<String>,
    /// `filtered_out` count from the suite-end JSON event.
    num_filtered_out: u64,
    /// Whether the suite-end event was seen at all (false on crash / panic
    /// before any tests run, e.g. malformed SKULD_LABELS).
    has_summary: bool,
    exit_code: Option<i32>,
    stderr: String,
    stdout: String,
    // RAII: dropped after the outcome, keeping the marker dir alive until
    // all asserts have run.
    _marker_dir: TempDir,
}

impl RunOutcome {
    fn ran(&self, name: &str) -> bool {
        self.markers.contains(name)
    }
}

struct ParsedEvents {
    passed: Vec<String>,
    failed: Vec<String>,
    ignored: Vec<String>,
    num_filtered_out: u64,
    has_summary: bool,
}

/// Spawn the fixture with `--format json` plus any extra args.
#[track_caller]
fn run_fixture(labels: Option<&str>, extra_args: &[&str]) -> RunOutcome {
    let mut args: Vec<&str> = vec!["--format", "json"];
    args.extend_from_slice(extra_args);
    run_fixture_raw(labels, &args)
}

/// Spawn the fixture with an arbitrary arg list (no injected `--format`).
/// Used only by the `--list` probe, whose output is not JSON regardless of
/// `--format`.
#[track_caller]
fn run_fixture_raw(labels: Option<&str>, args: &[&str]) -> RunOutcome {
    let marker_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_label_filter_fixture"));
    cmd.args(args);
    for key in [
        "SKULD_LABELS",
        "SKULD_DEBUG",
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
    cmd.env("LABEL_FILTER_FIXTURE_MARKERS", marker_dir.path());
    if let Some(v) = labels {
        cmd.env("SKULD_LABELS", v);
    }
    let out = cmd.output().expect("spawn label_filter_fixture");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let parsed = parse_json_events(&stdout);
    let markers = collect_markers(marker_dir.path());

    RunOutcome {
        markers,
        ignored: parsed.ignored,
        passed: parsed.passed,
        failed: parsed.failed,
        num_filtered_out: parsed.num_filtered_out,
        has_summary: parsed.has_summary,
        exit_code: out.status.code(),
        stderr,
        stdout,
        _marker_dir: marker_dir,
    }
}

/// Parse stdout as one JSON event per line. Ignore non-JSON lines (e.g.
/// `--list` output or diagnostics). Extract per-test outcomes and the
/// suite-end `filtered_out` count.
fn parse_json_events(stdout: &str) -> ParsedEvents {
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    let mut ignored = Vec::new();
    let mut num_filtered_out = 0u64;
    let mut has_summary = false;

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ty = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let event = value.get("event").and_then(|v| v.as_str()).unwrap_or("");
        match (ty, event) {
            ("test", "ok") => {
                if let Some(n) = value.get("name").and_then(|v| v.as_str()) {
                    passed.push(n.to_string());
                }
            }
            ("test", "failed") => {
                if let Some(n) = value.get("name").and_then(|v| v.as_str()) {
                    failed.push(n.to_string());
                }
            }
            ("test", "ignored") => {
                if let Some(n) = value.get("name").and_then(|v| v.as_str()) {
                    ignored.push(n.to_string());
                }
            }
            ("suite", "ok") | ("suite", "failed") => {
                has_summary = true;
                num_filtered_out = value.get("filtered_out").and_then(|v| v.as_u64()).unwrap_or(0);
            }
            _ => {}
        }
    }

    ParsedEvents {
        passed,
        failed,
        ignored,
        num_filtered_out,
        has_summary,
    }
}

#[track_caller]
fn collect_markers(dir: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read marker dir {dir:?}: {e}"));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("read marker entry: {e}"));
        if let Some(name) = entry.file_name().to_str() {
            out.insert(name.to_string());
        }
    }
    out
}

// Assertion helpers ======================================================================================

#[track_caller]
fn assert_no_unexpected_failures(out: &RunOutcome) {
    if !out.failed.is_empty() {
        panic!(
            "fixture reported test failures: {:?}\nstdout:\n{}\nstderr:\n{}",
            out.failed, out.stdout, out.stderr
        );
    }
}

#[track_caller]
fn assert_ran_set(out: &RunOutcome, expected: &[&str]) {
    assert_no_unexpected_failures(out);
    let expected: HashSet<String> = expected.iter().map(|s| s.to_string()).collect();
    if out.markers != expected {
        let extra: Vec<&String> = out.markers.difference(&expected).collect();
        let missing: Vec<&String> = expected.difference(&out.markers).collect();
        panic!(
            "marker mismatch\n  unexpected runs: {extra:?}\n  missing runs: {missing:?}\nstdout:\n{}\nstderr:\n{}",
            out.stdout, out.stderr
        );
    }
}

#[track_caller]
fn assert_ran(out: &RunOutcome, name: &str) {
    assert_no_unexpected_failures(out);
    if !out.ran(name) {
        panic!(
            "expected {name} to run. markers={:?}\nstdout:\n{}\nstderr:\n{}",
            out.markers, out.stdout, out.stderr
        );
    }
}

#[track_caller]
fn assert_absent(out: &RunOutcome, name: &str) {
    if out.ran(name) {
        panic!("{name} should not have run. markers={:?}", out.markers);
    }
    let sname = name.to_string();
    if out.ignored.contains(&sname) || out.passed.contains(&sname) || out.failed.contains(&sname) {
        panic!(
            "{name} should not have appeared in libtest output.\n  passed={:?}\n  ignored={:?}\n  failed={:?}",
            out.passed, out.ignored, out.failed
        );
    }
}

#[track_caller]
fn assert_ignored_and_absent_marker(out: &RunOutcome, name: &str) {
    if !out.ignored.contains(&name.to_string()) {
        panic!(
            "expected {name} to appear as ignored. ignored={:?}\nstdout:\n{}",
            out.ignored, out.stdout
        );
    }
    if out.ran(name) {
        panic!("ignored test {name} should not have executed its body");
    }
}

#[track_caller]
fn assert_nonzero_exit_with_label_panic(out: &RunOutcome) {
    if out.exit_code == Some(0) {
        panic!(
            "expected non-zero exit for SKULD_LABELS panic.\nstdout:\n{}\nstderr:\n{}",
            out.stdout, out.stderr
        );
    }
    if !out.stderr.contains("skuld: SKULD_LABELS:") {
        panic!(
            "expected stderr to contain `skuld: SKULD_LABELS:`.\nstdout:\n{}\nstderr:\n{}",
            out.stdout, out.stderr
        );
    }
    assert!(
        out.markers.is_empty(),
        "panic before collection: no marker should have been written, got {:?}",
        out.markers
    );
}

// Catalogs ===============================================================================================

/// Every marker-writing test in the fixture, keyed by marker name.
fn all_tests() -> &'static [&'static str] {
    &[
        "t_none",
        "t_fast",
        "t_slow",
        "t_fast_docker",
        "t_slow_docker",
        "t_fast_slow_compound",
        "t_dup_fast",
        "t_outer_ignored_fast",
        "t_native_ignored_fast",
        "t_outer_reason_ignored_fast",
        "t_native_reason_ignored_fast",
        "t_req_fast",
        "t_req_unmet_fast",
        "t_serial_fast",
        "t_serial_filter_fast",
        "t_should_panic_fast",
        "t_should_panic_slow",
        "t_default",
        "t_explicit_db",
        "t_optout",
        "t_nested_default",
        "t_override",
        "dyn_fast",
        "dyn_slow_ignored",
        "dyn_serial_fast",
        "dyn_serial_filter_fast",
    ]
}

/// Tests that are ignored (either by `#[ignore]`, native `ignore`, unmet
/// `requires`, or dynamic `ignored=true`) — marker is never written.
fn ignored_when_unfiltered() -> &'static [&'static str] {
    &[
        "t_outer_ignored_fast",
        "t_native_ignored_fast",
        "t_outer_reason_ignored_fast",
        "t_native_reason_ignored_fast",
        "t_req_unmet_fast",
        "dyn_slow_ignored",
    ]
}

/// Names of statically-ignored inventory tests (all four `Ignore::{Yes, WithReason}`
/// variants × outer-attribute / macro-arg spellings).
const STATICALLY_IGNORED_INVENTORY: &[&str] = &[
    "t_outer_ignored_fast",
    "t_native_ignored_fast",
    "t_outer_reason_ignored_fast",
    "t_native_reason_ignored_fast",
];

// Group A — Baseline =====================================================================================

#[test]
fn a1_unset_runs_all_nonignored() {
    let out = run_fixture(None, &[]);
    assert_eq!(out.exit_code, Some(0), "stderr:\n{}", out.stderr);

    let expected_ran: HashSet<String> = all_tests()
        .iter()
        .filter(|n| !ignored_when_unfiltered().contains(n))
        .map(|s| s.to_string())
        .collect();
    let expected_ignored: HashSet<String> = ignored_when_unfiltered().iter().map(|s| s.to_string()).collect();

    assert_eq!(out.markers, expected_ran, "markers diff\nstdout:\n{}", out.stdout);
    let got_ignored: HashSet<String> = out.ignored.iter().cloned().collect();
    assert_eq!(got_ignored, expected_ignored, "ignored diff");
}

#[test]
fn a2_empty_string_panics() {
    let out = run_fixture(Some(""), &[]);
    assert_nonzero_exit_with_label_panic(&out);
}

#[test]
fn a3_malformed_trailing_op_panics() {
    let out = run_fixture(Some("a &"), &[]);
    assert_nonzero_exit_with_label_panic(&out);
}

#[test]
fn a4_malformed_bare_bang_panics() {
    let out = run_fixture(Some("!"), &[]);
    assert_nonzero_exit_with_label_panic(&out);
}

#[test]
fn a5_malformed_unmatched_paren_panics() {
    let out = run_fixture(Some("(a"), &[]);
    assert_nonzero_exit_with_label_panic(&out);
}

#[test]
fn a6_malformed_numeric_label_panics() {
    let out = run_fixture(Some("1foo"), &[]);
    assert_nonzero_exit_with_label_panic(&out);
}

#[test]
fn a7_malformed_hyphenated_label_panics() {
    let out = run_fixture(Some("a-b"), &[]);
    assert_nonzero_exit_with_label_panic(&out);
}

#[test]
fn a8_whitespace_only_panics() {
    let out = run_fixture(Some("   "), &[]);
    assert_nonzero_exit_with_label_panic(&out);
}

#[test]
fn a9_case_insensitive_match() {
    // Post-#29: `SKULD_LABELS` is matched case-insensitively. `FAST`,
    // `Fast`, and `fast` must select the same set.
    let out_upper = run_fixture(Some("FAST"), &[]);
    let out_mixed = run_fixture(Some("Fast"), &[]);
    let out_lower = run_fixture(Some("fast"), &[]);
    assert_eq!(out_upper.markers, out_lower.markers);
    assert_eq!(out_mixed.markers, out_lower.markers);
    assert!(!out_lower.markers.is_empty(), "sanity: fast matches something");
}

// Group B — Operators ====================================================================================

const FAST_RUNS_UNDER_POSITIVE_FILTER: &[&str] = &[
    "t_fast",
    "t_fast_docker",
    "t_dup_fast",
    "t_req_fast",
    "t_serial_fast",
    "t_serial_filter_fast",
    "t_should_panic_fast",
    "dyn_fast",
    "dyn_serial_fast",
    "dyn_serial_filter_fast",
];

#[test]
fn b1_bare_label_fast() {
    let out = run_fixture(Some("fast"), &[]);
    assert_ran_set(&out, FAST_RUNS_UNDER_POSITIVE_FILTER);
    // Prefix-collision guard: "fast" must not match the "fast_slow" label.
    // `assert_ran_set` already catches extras, but state the invariant
    // explicitly so a future refactor can't silently erode it.
    assert_absent(&out, "t_fast_slow_compound");
}

#[test]
fn b2_bare_label_docker() {
    let out = run_fixture(Some("docker"), &[]);
    assert_ran_set(&out, &["t_fast_docker", "t_slow_docker"]);
}

#[test]
fn b3_or_fast_or_slow() {
    let out = run_fixture(Some("fast|slow"), &[]);
    let mut expected: Vec<&str> = FAST_RUNS_UNDER_POSITIVE_FILTER.to_vec();
    expected.extend_from_slice(&["t_slow", "t_slow_docker", "t_should_panic_slow"]);
    assert_ran_set(&out, &expected);
}

#[test]
fn b4_and_fast_and_docker() {
    let out = run_fixture(Some("fast & docker"), &[]);
    assert_ran_set(&out, &["t_fast_docker"]);
}

#[test]
fn b5_not_fast() {
    let out = run_fixture(Some("!fast"), &[]);
    // Everything without a `fast` label.
    assert_ran_set(
        &out,
        &[
            "t_none",
            "t_slow",
            "t_slow_docker",
            "t_fast_slow_compound",
            "t_should_panic_slow",
            "t_default",
            "t_explicit_db",
            "t_optout",
            "t_nested_default",
            "t_override",
        ],
    );
}

#[test]
fn b6_double_negation() {
    let out = run_fixture(Some("!!fast"), &[]);
    assert_ran_set(&out, FAST_RUNS_UNDER_POSITIVE_FILTER);
}

#[test]
fn b7_parenthesized_or_and() {
    let out = run_fixture(Some("(fast|slow) & docker"), &[]);
    assert_ran_set(&out, &["t_fast_docker", "t_slow_docker"]);
}

#[test]
fn b8_whitespace_permissive() {
    let out = run_fixture(Some("  fast  &  docker  "), &[]);
    assert_ran_set(&out, &["t_fast_docker"]);
}

#[test]
fn b9_tabs_and_spaces() {
    let out = run_fixture(Some("fast\t&\tdocker"), &[]);
    assert_ran_set(&out, &["t_fast_docker"]);
}

// Group C — Precedence ===================================================================================

#[test]
fn c1_and_binds_tighter_than_or() {
    // fast | (slow & docker)
    let out = run_fixture(Some("fast | slow & docker"), &[]);
    // fast-labeled tests ALL run; t_slow_docker runs (slow & docker);
    // t_slow alone does NOT run.
    let mut expected: Vec<&str> = FAST_RUNS_UNDER_POSITIVE_FILTER.to_vec();
    expected.push("t_slow_docker");
    assert_ran_set(&out, &expected);
}

#[test]
fn c2_not_binds_tighter_than_and() {
    // (!fast) & docker
    let out = run_fixture(Some("!fast & docker"), &[]);
    assert_ran_set(&out, &["t_slow_docker"]);
}

#[test]
fn c3_not_binds_tighter_than_or() {
    // (!fast) | slow
    let out = run_fixture(Some("!fast | slow"), &[]);
    assert_ran_set(
        &out,
        &[
            "t_none",
            "t_slow",
            "t_slow_docker",
            "t_fast_slow_compound",
            "t_should_panic_slow",
            "t_default",
            "t_explicit_db",
            "t_optout",
            "t_nested_default",
            "t_override",
        ],
    );
}

#[test]
fn c4_parens_override_and_precedence() {
    let out = run_fixture(Some("(fast | slow) & !docker"), &[]);
    // fast or slow, and not docker.
    assert_ran_set(
        &out,
        &[
            "t_fast",
            "t_slow",
            "t_dup_fast",
            "t_req_fast",
            "t_serial_fast",
            "t_serial_filter_fast",
            "t_should_panic_fast",
            "t_should_panic_slow",
            "dyn_fast",
            "dyn_serial_fast",
            "dyn_serial_filter_fast",
        ],
    );
}

#[test]
fn c5_left_assoc_or() {
    let out = run_fixture(Some("fast | slow | db"), &[]);
    let mut expected: Vec<&str> = FAST_RUNS_UNDER_POSITIVE_FILTER.to_vec();
    expected.extend_from_slice(&[
        "t_slow",
        "t_slow_docker",
        "t_should_panic_slow",
        "t_explicit_db",
        "t_override",
    ]);
    assert_ran_set(&out, &expected);
}

#[test]
fn c6_unsatisfiable_and() {
    let out = run_fixture(Some("fast & slow & docker"), &[]);
    assert_ran_set(&out, &[]);
}

#[test]
fn c7_negation_with_nested_default() {
    // !fast & integration — default-inherited `integration` tests that don't have fast.
    let out = run_fixture(Some("!fast & integration"), &[]);
    assert_ran_set(&out, &["t_default", "t_nested_default"]);
}

// Group D — default_labels! inheritance ==================================================================

#[test]
fn d1_inherited_matches_default_label() {
    let out = run_fixture(Some("integration"), &[]);
    assert_ran_set(&out, &["t_default", "t_nested_default"]);
}

#[test]
fn d2_explicit_replaces_defaults_db() {
    let out = run_fixture(Some("db"), &[]);
    assert_ran_set(&out, &["t_explicit_db", "t_override"]);
}

#[test]
fn d3_explicit_opt_out_absent_under_positive_filter() {
    let out = run_fixture(Some("fast"), &[]);
    assert_absent(&out, "t_optout");
}

#[test]
fn d4_optout_survives_negation() {
    let out = run_fixture(Some("!integration"), &[]);
    assert_ran(&out, "t_optout");
    assert_absent(&out, "t_default");
    assert_absent(&out, "t_nested_default");
}

#[test]
fn d5_longest_prefix_wins() {
    // Under `db`, `override_defaults::t_override` runs (nested default).
    let out_db = run_fixture(Some("db"), &[]);
    assert_ran(&out_db, "t_override");

    // Under `integration`, it does NOT (nested default overrode outer).
    let out_int = run_fixture(Some("integration"), &[]);
    assert_absent(&out_int, "t_override");
}

// (D6 "alias in inherited module" and D7 "get_label equivalence" removed:
//  #29 replaced `new_label!`/`get_label!` with `#[skuld::label]`, and a label
//  declaration of the same lowercased name panics at startup — so
//  "same-string alias in the same binary" is no longer a supported concept.
//  Cross-crate re-use is a plain `use`, which has no runtime surface here.)

// Group E — Dynamic tests ================================================================================

#[test]
fn e1_dyn_fast_matches() {
    let out = run_fixture(Some("fast"), &[]);
    assert_ran(&out, "dyn_fast");
    assert_ran(&out, "dyn_serial_fast");
    assert_ran(&out, "dyn_serial_filter_fast");
    assert_absent(&out, "dyn_slow_ignored");
}

#[test]
fn e2_dyn_slow_matches_but_ignored() {
    let out = run_fixture(Some("slow"), &[]);
    assert_ignored_and_absent_marker(&out, "dyn_slow_ignored");
    assert_absent(&out, "dyn_fast");
    assert_absent(&out, "dyn_serial_fast");
    assert_absent(&out, "dyn_serial_filter_fast");
}

#[test]
fn e3_dyn_filtered_out_not_ignored() {
    let out = run_fixture(Some("docker"), &[]);
    for n in &[
        "dyn_fast",
        "dyn_slow_ignored",
        "dyn_serial_fast",
        "dyn_serial_filter_fast",
    ] {
        assert_absent(&out, n);
    }
}

#[test]
fn e4_dyn_slow_ignored_executes_under_include_ignored() {
    // Regression guard: the dynamic-test path already handles --include-ignored
    // correctly. This test locks that in so the inventory-path fix for #31
    // doesn't accidentally change dynamic semantics.
    let out = run_fixture(Some("slow"), &["--include-ignored"]);
    assert!(
        out.ran("dyn_slow_ignored"),
        "dynamic ignored body must execute under --include-ignored. markers={:?}",
        out.markers
    );
    assert!(
        out.passed.contains(&"dyn_slow_ignored".to_string()),
        "dyn_slow_ignored expected in passed; got {:?}",
        out.passed
    );
}

// Group F — #[ignore] interaction ========================================================================

#[test]
fn f1_outer_ignore_matching_appears_ignored() {
    let out = run_fixture(Some("fast"), &[]);
    assert_ignored_and_absent_marker(&out, "t_outer_ignored_fast");
    assert_ignored_and_absent_marker(&out, "t_outer_reason_ignored_fast");
}

#[test]
fn f2_native_ignore_matching_appears_ignored() {
    let out = run_fixture(Some("fast"), &[]);
    assert_ignored_and_absent_marker(&out, "t_native_ignored_fast");
    assert_ignored_and_absent_marker(&out, "t_native_reason_ignored_fast");
}

#[test]
fn f3_ignore_nonmatching_absent_not_ignored() {
    let out = run_fixture(Some("slow"), &[]);
    for name in STATICALLY_IGNORED_INVENTORY {
        assert_absent(&out, name);
    }
}

#[test]
fn f4_include_ignored_executes_body() {
    // Fix for issue #31: --include-ignored must execute the real body of
    // statically-ignored tests, not a stub. Covers all four variants
    // (outer attribute / macro arg × plain / with reason).
    let out = run_fixture(Some("fast"), &["--include-ignored"]);
    for name in STATICALLY_IGNORED_INVENTORY {
        assert!(
            out.ran(name),
            "{name} body must execute under --include-ignored. markers={:?}",
            out.markers
        );
        assert!(
            out.passed.contains(&(*name).to_string()),
            "{name} expected in passed; got {:?}",
            out.passed
        );
    }
}

#[test]
fn f5_ignored_flag_executes_bodies() {
    // Fix for issue #31: --ignored must execute the real body of
    // statically-ignored tests, and filter out non-ignored ones. Covers
    // all four variants (outer attribute / macro arg × plain / with reason).
    let out = run_fixture(Some("fast"), &["--ignored"]);
    for name in STATICALLY_IGNORED_INVENTORY {
        assert!(
            out.ran(name),
            "{name} body must execute under --ignored. markers={:?}",
            out.markers
        );
        assert!(
            out.passed.contains(&(*name).to_string()),
            "{name} expected in passed; got {:?}",
            out.passed
        );
    }
    // Non-ignored fast tests don't appear at all under --ignored.
    assert!(
        !out.passed.contains(&"t_fast".to_string()),
        "t_fast should not be in passed under --ignored"
    );
    assert!(!out.ran("t_fast"), "t_fast should not have run under --ignored");
}

// Group G — requires interaction =========================================================================

#[test]
fn g1_requires_met_filtered_in_runs() {
    let out = run_fixture(Some("fast"), &[]);
    assert_ran(&out, "t_req_fast");
}

#[test]
fn g2_requires_unmet_filtered_in_reports_unavailable() {
    let out = run_fixture(Some("fast"), &[]);
    assert_ignored_and_absent_marker(&out, "t_req_unmet_fast");
    assert!(
        out.stderr.contains("Unavailable") && out.stderr.contains("t_req_unmet_fast"),
        "expected 'Unavailable' block mentioning t_req_unmet_fast.\nstderr:\n{}",
        out.stderr
    );
}

#[test]
fn g3_requires_unmet_filtered_out_absent() {
    let out = run_fixture(Some("slow"), &[]);
    assert_absent(&out, "t_req_unmet_fast");
    assert!(
        !out.stderr.contains("t_req_unmet_fast"),
        "expected t_req_unmet_fast to be absent from stderr.\nstderr:\n{}",
        out.stderr
    );
}

#[test]
fn g4_requires_unmet_executes_under_include_ignored() {
    // Fix for issue #31: preconditions failing at collection time must
    // not gate --include-ignored from running the real body.
    let out = run_fixture(Some("fast"), &["--include-ignored"]);
    assert!(
        out.ran("t_req_unmet_fast"),
        "body must execute under --include-ignored even though requires failed. markers={:?}",
        out.markers
    );
    assert!(
        out.passed.contains(&"t_req_unmet_fast".to_string()),
        "t_req_unmet_fast expected in passed; got {:?}",
        out.passed
    );
    // The collection-time Unavailable summary is still accurate and still reported.
    assert!(
        out.stderr.contains("Unavailable") && out.stderr.contains("t_req_unmet_fast"),
        "Unavailable stderr block must still report the collection-time failure.\nstderr:\n{}",
        out.stderr
    );
}

#[test]
fn g5_requires_unmet_executes_under_ignored() {
    // Fix for issue #31: same as g4, but under --ignored (which also filters
    // out non-ignored tests).
    let out = run_fixture(Some("fast"), &["--ignored"]);
    assert!(
        out.ran("t_req_unmet_fast"),
        "body must execute under --ignored even though requires failed. markers={:?}",
        out.markers
    );
    assert!(
        out.passed.contains(&"t_req_unmet_fast".to_string()),
        "t_req_unmet_fast expected in passed; got {:?}",
        out.passed
    );
    assert!(
        out.stderr.contains("Unavailable") && out.stderr.contains("t_req_unmet_fast"),
        "Unavailable stderr block must still report the collection-time failure.\nstderr:\n{}",
        out.stderr
    );
    // --ignored filters non-ignored tests out.
    assert_absent(&out, "t_fast");
}

// Group H — serial interaction ===========================================================================

#[test]
fn h1_serial_labeled_filtered_in() {
    let out = run_fixture(Some("fast"), &[]);
    assert_ran(&out, "t_serial_fast");
    assert_ran(&out, "t_serial_filter_fast");
}

#[test]
fn h2_serial_labeled_filtered_out_absent() {
    let out = run_fixture(Some("slow"), &[]);
    assert_absent(&out, "t_serial_fast");
    assert_absent(&out, "t_serial_filter_fast");
}

// Group J — should_panic interaction =====================================================================

#[test]
fn j1_should_panic_filtered_in_passes() {
    let out = run_fixture(Some("fast"), &[]);
    assert_ran(&out, "t_should_panic_fast");
    assert!(
        out.passed.contains(&"t_should_panic_fast".to_string()),
        "expected t_should_panic_fast in passed; got {:?}",
        out.passed
    );
    assert_absent(&out, "t_should_panic_slow");
}

#[test]
fn j2_should_panic_filtered_out_absent() {
    let out = run_fixture(Some("slow"), &[]);
    assert_ran(&out, "t_should_panic_slow");
    assert!(out.passed.contains(&"t_should_panic_slow".to_string()));
    assert_absent(&out, "t_should_panic_fast");
}

// Group K — libtest-mimic CLI flag interaction ===========================================================

#[test]
fn k1_skip_flag_composes_with_label() {
    let out = run_fixture(Some("fast"), &["--skip", "t_fast_docker"]);
    assert_absent(&out, "t_fast_docker");
    assert_ran(&out, "t_fast");
    assert_ran(&out, "t_dup_fast");
}

#[test]
fn k2_exact_name_with_label_filter() {
    let out = run_fixture(Some("fast"), &["--exact", "t_fast"]);
    assert_ran(&out, "t_fast");
    // Nothing else that matched `fast` runs because of --exact.
    assert_absent(&out, "t_fast_docker");
    assert_absent(&out, "t_dup_fast");
}

#[test]
fn k3_exact_name_filtered_out_by_label() {
    // With SKULD_LABELS=slow, t_fast is label-filtered out of collection
    // before --exact sees it → nothing runs.
    let out = run_fixture(Some("slow"), &["--exact", "t_fast"]);
    assert_absent(&out, "t_fast");
    assert!(out.markers.is_empty(), "expected no markers; got {:?}", out.markers);
}

#[test]
fn k4_list_flag_respects_label_filter() {
    let out = run_fixture_raw(Some("fast"), &["--list"]);
    assert_eq!(out.exit_code, Some(0), "stderr:\n{}", out.stderr);
    // No bodies should have run — --list just prints.
    assert!(
        out.markers.is_empty(),
        "expected no markers for --list; got {:?}",
        out.markers
    );
    // Parse `NAME: test` per-line (strict equality on the name column so
    // that `t_fast` cannot match `t_fast_docker` via substring).
    let listed: HashSet<String> = out
        .stdout
        .lines()
        .filter_map(|l| l.trim().strip_suffix(": test").map(|n| n.to_string()))
        .collect();
    // --list enumerates every trial that passed label filtering, including
    // ignored ones (they appear in the trial list with `with_ignored_flag(true)`).
    let mut expected: HashSet<String> = FAST_RUNS_UNDER_POSITIVE_FILTER.iter().map(|s| s.to_string()).collect();
    for name in STATICALLY_IGNORED_INVENTORY {
        expected.insert((*name).to_string());
    }
    expected.insert("t_req_unmet_fast".to_string());
    assert_eq!(listed, expected, "--list output mismatch\nstdout:\n{}", out.stdout);
}

// Group I — guard-rails ==================================================================================

#[test]
fn i1_num_filtered_out_excludes_label_drops() {
    let out = run_fixture(Some("fast"), &[]);
    assert!(out.has_summary, "no suite-end event; stdout:\n{}", out.stdout);
    assert_eq!(
        out.num_filtered_out, 0,
        "label filtering should not populate num_filtered_out; stdout:\n{}",
        out.stdout
    );
}

#[test]
fn i2_num_filtered_out_counts_skip_only() {
    let out = run_fixture(Some("fast"), &["--skip", "t_fast_docker"]);
    assert!(out.has_summary, "no suite-end event; stdout:\n{}", out.stdout);
    assert_eq!(
        out.num_filtered_out, 1,
        "name-filter --skip should populate num_filtered_out; stdout:\n{}",
        out.stdout
    );
}

// (i3 "parent SKULD_LABELS does not leak" removed: the only way to set up
//  the parent-env scenario is `std::env::set_var`, which is unsound under
//  parallel test execution. The scrub is a visible `cmd.env_remove("SKULD_LABELS")`
//  call in `run_fixture_raw`.)

// Group L — extra coverage ===============================================================================

/// Outer whitespace around a valid expression must parse. Mirrors the
/// pest-level whitespace tolerance but sets it end-to-end from the env var.
#[test]
fn l1_outer_whitespace_tolerated() {
    let out = run_fixture(Some("  fast  "), &[]);
    assert_ran_set(&out, FAST_RUNS_UNDER_POSITIVE_FILTER);
}

/// Syntactically valid label name that matches no test in the fixture.
/// The customer-reported "filtering does not work" could literally be a
/// typo in the label name producing an empty collection — lock in that
/// this is a 0-tests-run run, not an error.
#[test]
fn l2_unknown_label_matches_nothing() {
    let out = run_fixture(Some("nosuchlabel"), &[]);
    assert_eq!(out.exit_code, Some(0), "stderr:\n{}", out.stderr);
    assert!(
        out.markers.is_empty(),
        "unknown label should match nothing; got {:?}",
        out.markers
    );
    assert!(out.failed.is_empty(), "no failures expected");
}

/// Pest error content must be preserved through the panic message. A3-A7
/// assert on the prefix; this spot-checks the inner diagnostic.
#[test]
fn l4_malformed_stderr_contains_pest_detail() {
    let out = run_fixture(Some("(a"), &[]);
    assert_nonzero_exit_with_label_panic(&out);
    // Pest's error mentions "expected" something — be lax about exact wording
    // but require the prefix plus *any* descriptive content after it.
    let lower = out.stderr.to_lowercase();
    assert!(
        lower.contains("expected") || lower.contains("paren") || lower.contains("unexpected"),
        "expected pest diagnostic content in stderr; got:\n{}",
        out.stderr
    );
}

/// Literal example from docs/src/labels.md.
#[test]
fn l5_docs_example_literal() {
    let out = run_fixture(Some("(docker | integration) & !slow"), &[]);
    // docker-or-integration tests in the fixture, minus slow.
    // Only docker-labeled tests without slow qualify from the matrix:
    // t_fast_docker (no slow) runs; t_slow_docker (has slow) absent;
    // `integration` is inherited — t_default and t_nested_default qualify
    // (they have integration, no slow).
    assert_ran_set(&out, &["t_fast_docker", "t_default", "t_nested_default"]);
}
