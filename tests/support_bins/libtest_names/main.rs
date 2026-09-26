//! Subject of subprocess invocations in `tests/libtest_names_cli.rs`. Not a
//! real product binary.
//!
//! Two sibling modules each declare a `same` test, plus a crate-root `top`
//! test, to exercise `libtest_names()`'s module-path-without-first-segment
//! naming.

mod a {
    #[skuld::test]
    fn same() {}
}

mod b {
    #[skuld::test]
    fn same() {}
}

#[skuld::test]
fn top() {}

fn main() {
    let mut runner = skuld::TestRunner::new();
    runner.libtest_names();
    runner.run();
}
