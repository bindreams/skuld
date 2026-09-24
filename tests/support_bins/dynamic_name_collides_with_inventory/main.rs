//! Subject of subprocess invocations in `tests/libtest_names_cli.rs`. Not a
//! real product binary.
//!
//! An inventory test named `clash` and a dynamic test added with the same
//! name — must panic at startup and name both origins.

#[skuld::test]
fn clash() {}

fn main() {
    let mut runner = skuld::TestRunner::new();
    runner.add("clash", &[], false, || {});
    runner.run();
}
