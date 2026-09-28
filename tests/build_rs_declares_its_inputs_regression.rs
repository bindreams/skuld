//! Guards one contributor to the mechanism behind CI run 36320721947's
//! `Test (darwin/arm64)` failure: `build.rs` emitted no `rerun-if-changed`,
//! so cargo fell back to scanning every file in the package to decide
//! whether to rerun it — including directories outside cargo's own
//! control, like `cargo-skuld/tests/fixtures/test-workspace`, which a
//! concurrent build elsewhere can be renaming or removing at that exact
//! moment. Reproduced directly: with the fallback active, placing an
//! unreadable, untracked directory anywhere under the repo makes any
//! build depending on `skuld` fail with `Failed to read the directory
//! ...: Permission denied` — the same failure class as the CI run's `...
//! No such file or directory`, deterministically, no concurrency
//! required. Declaring `build.rs` as its own only real input (it reads no
//! files; `OUT_DIR` is cargo-computed per invocation, not an external
//! input to track) removes that fallback — but only for a *warm* build
//! against an already-initialized target directory: the first build
//! against any given one still runs the scan regardless, since cargo has
//! no prior record yet to know it can skip it, and the fixture's own
//! target directory is cold on every CI run. What actually stops CI's
//! specific failure is `cargo-skuld/tests/fixtures/test-workspace/
//! .gitignore`'s `/target*`, which keeps the scan from ever stepping into
//! the directory in question regardless of warm or cold. This test guards
//! `build.rs`'s contribution on its own terms — cutting scan frequency
//! down to one per target directory instead of one per build — not a
//! claim that it alone would have stopped CI's failure.
//!
//! Behavioral, not a source-text check: a commented-out directive still
//! contains the literal string `cargo:rerun-if-changed=build.rs`, so
//! grepping `build.rs`'s own source would pass on a build.rs that no
//! longer actually emits it — while the underlying bug (cargo scanning
//! the whole repo) would be back. Instead this reads cargo's own record
//! of what the build script actually printed on its last run: cargo
//! caches a build script's raw stdout at
//! `<OUT_DIR>/../output` (an implementation detail of cargo's build
//! script machinery, not a stable public API, but the only way to
//! observe from a test what instructions were actually emitted).

#[test]
fn build_script_emitted_rerun_if_changed_for_its_own_source() {
    let output_path = std::path::Path::new(env!("OUT_DIR"))
        .parent()
        .expect("OUT_DIR must have a parent")
        .join("output");
    let output = std::fs::read_to_string(&output_path)
        .unwrap_or_else(|e| panic!("failed to read cargo's build script output cache at {output_path:?}: {e}"));
    assert!(
        output
            .lines()
            .any(|line| { line == "cargo:rerun-if-changed=build.rs" || line == "cargo::rerun-if-changed=build.rs" }),
        "build.rs's last recorded run did not emit `cargo:rerun-if-changed=build.rs`. Without \
         it, cargo falls back to scanning every file in the package on every build to decide \
         whether to rerun this script, which can fail with \"Failed to read the directory\" if \
         anything under the repo is transiently unreadable or missing mid-walk (see CI run \
         36320721947). Recorded output:\n{output}"
    );
}
