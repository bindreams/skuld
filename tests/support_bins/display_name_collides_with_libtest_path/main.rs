//! Subject of subprocess invocations in `tests/libtest_names_cli.rs`. Not a
//! real product binary.
//!
//! With `libtest_names()` on, `mod a { fn same }` resolves to the computed
//! trial name `"a::same"`. A crate-root test explicitly declares
//! `name = "a::same"`, colliding with that computed name — must panic at
//! startup and name both origins.

mod a {
    #[skuld::test]
    fn same() {}
}

#[skuld::test(name = "a::same")]
fn collider() {}

fn main() {
    let mut runner = skuld::TestRunner::new();
    runner.libtest_names();
    runner.run();
}
