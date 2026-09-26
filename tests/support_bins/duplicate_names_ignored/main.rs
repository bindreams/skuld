//! Subject of subprocess invocations in `tests/libtest_names_cli.rs`. Not a
//! real product binary.
//!
//! An `#[ignore]`d inventory test and an `#[ignore]`d dynamic test share the
//! trial name `clash`. The duplicate-name check validates the
//! *set* of final trial names unconditionally — it must still panic at
//! startup even though neither trial would ever run by default.

#[skuld::test]
#[ignore = "duplicate-name fixture; never meant to run"]
fn clash() {}

fn main() {
    let mut runner = skuld::TestRunner::new();
    runner.add("clash", &[], true, || {});
    runner.run();
}
