//! Subject of subprocess invocations in
//! `tests/fixture_setup_should_panic_cli.rs`. Not a real product binary.
//!
//! `broken`'s setup always fails, and `uses_broken_fixture` is a
//! `should_panic` test that depends on it. Before fixture setup was moved
//! outside `should_panic`'s `catch_unwind`, this setup failure satisfied
//! the panic expectation and the test wrongly reported `ok`.

#[skuld::fixture]
fn broken() -> Result<u32, String> {
    Err("setup intentionally fails".to_string())
}

#[skuld::test(should_panic)]
fn uses_broken_fixture(#[fixture(broken)] _v: &u32) {
    panic!("test body must not run when fixture setup fails");
}

fn main() {
    skuld::run_all();
}
