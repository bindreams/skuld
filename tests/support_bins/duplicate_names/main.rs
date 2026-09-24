//! Subject of subprocess invocations in `tests/libtest_names_cli.rs`. Not a
//! real product binary.
//!
//! Two sibling modules each declare a bare `same` test. Without
//! `libtest_names()`, both resolve to the trial name `"same"` — a duplicate
//! that must panic at startup and name both modules.

mod a {
    #[skuld::test]
    fn same() {}
}

mod b {
    #[skuld::test]
    fn same() {}
}

fn main() {
    let runner = skuld::TestRunner::new();
    runner.run();
}
