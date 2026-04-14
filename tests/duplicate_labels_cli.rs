//! Out-of-harness test: spawn a skuld binary that declares the same label
//! twice (in two sibling modules) and assert the startup validation panics
//! with both source locations. Cannot be asserted from inside a single
//! skuld run because the panic happens during `validate_labels()` in
//! `TestRunner::run_tests` and aborts the process.

use std::process::Command;

#[test]
fn duplicate_labels_panic_with_both_locations() {
    let bin = env!("CARGO_BIN_EXE_duplicate_labels");
    let out = Command::new(bin).output().expect("spawn duplicate_labels binary");

    assert!(
        !out.status.success(),
        "expected duplicate_labels binary to panic at startup, got success"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("thread 'main' panicked") || stderr.contains("panicked at"),
        "expected a main-thread panic in stderr; got:\n{stderr}"
    );
    assert!(
        stderr.contains("label validation failed"),
        "expected 'label validation failed' in stderr; got:\n{stderr}"
    );
    assert!(
        stderr.contains("\"dup\""),
        "expected the lowercased name 'dup' in stderr; got:\n{stderr}"
    );

    // Both sibling module files must appear and as *distinct* file:line:col
    // entries. Regex-free: collect every `...a.rs:<line>:<col>` /
    // `...b.rs:<line>:<col>` substring and assert at least one of each.
    let locations: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("a.rs:") || l.contains("b.rs:"))
        .collect();
    assert!(
        locations.iter().any(|l| l.contains("a.rs:")),
        "expected an a.rs:<line>:<col> location in stderr; got:\n{stderr}"
    );
    assert!(
        locations.iter().any(|l| l.contains("b.rs:")),
        "expected a b.rs:<line>:<col> location in stderr; got:\n{stderr}"
    );
    // Each location line must include a line:column pair with non-zero
    // digits; `file!:0:0` would mean the proc-macro synthesised the span.
    for loc in &locations {
        let pair = loc
            .rsplit_once(':')
            .and_then(|(rest, col)| rest.rsplit_once(':').map(|(_, line)| (line, col)));
        let (line, col) = pair.expect("location string has file:line:col");
        assert!(
            line.trim()
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit() && c != '0'),
            "expected non-zero line number in {loc:?}"
        );
        assert!(
            col.trim()
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit() && c != '0'),
            "expected non-zero column number in {loc:?}"
        );
    }
}
