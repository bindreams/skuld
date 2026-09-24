//! Subject of subprocess invocations in `tests/libtest_names_cli.rs`. Not a
//! real product binary.
//!
//! Two unrelated tests both explicitly declare `name = "dup"`. `display_name`
//! always wins, so both resolve to the trial name `"dup"` — a duplicate that
//! must panic at startup and name both origins.

mod a {
    #[skuld::test(name = "dup")]
    fn one() {}
}

mod b {
    #[skuld::test(name = "dup")]
    fn two() {}
}

fn main() {
    let runner = skuld::TestRunner::new();
    runner.run();
}
