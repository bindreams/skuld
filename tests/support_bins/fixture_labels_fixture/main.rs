//! Subject of `tests/fixture_labels_cli.rs`: fixtures that carry labels, and
//! tests that pick those labels up by using the fixtures.

#[skuld::label]
pub const LEAF: skuld::Label;
#[skuld::label]
pub const MID: skuld::Label;
#[skuld::label]
pub const OWN: skuld::Label;
#[skuld::label]
pub const REP: skuld::Label;
#[skuld::label]
pub const MODL: skuld::Label;

#[skuld::fixture(labels = [LEAF])]
fn leaf() -> Result<u32, String> {
    Ok(1)
}

/// Carries its own label and reaches `LEAF` only through `leaf`.
#[skuld::fixture(labels = [MID])]
fn mid(#[fixture(leaf)] leaf: &u32) -> Result<u32, String> {
    Ok(*leaf + 1)
}

#[skuld::fixture]
fn plain() -> Result<u32, String> {
    Ok(0)
}

/// Unlabeled fixture that depends on `mid`: labels arrive two hops away.
#[skuld::fixture]
fn wrapper(#[fixture(mid)] mid: &u32) -> Result<u32, String> {
    Ok(*mid)
}

/// Two fixtures reaching the same labeled dependency (a diamond).
#[skuld::fixture]
fn left(#[fixture(leaf)] leaf: &u32) -> Result<u32, String> {
    Ok(*leaf)
}

#[skuld::fixture]
fn right(#[fixture(leaf)] leaf: &u32) -> Result<u32, String> {
    Ok(*leaf)
}

#[skuld::fixture(labels = [REP, REP])]
fn repeating() -> Result<u32, String> {
    Ok(0)
}

#[skuld::test]
fn t_none() {}

#[skuld::test]
fn t_plain_fixture(#[fixture(plain)] _p: &u32) {}

#[skuld::test]
fn t_direct(#[fixture(leaf)] _l: &u32) {}

#[skuld::test]
fn t_transitive(#[fixture(mid)] _m: &u32) {}

#[skuld::test]
fn t_two_hops(#[fixture(wrapper)] _w: &u32) {}

#[skuld::test]
fn t_diamond(#[fixture(left)] _a: &u32, #[fixture(right)] _b: &u32) {}

#[skuld::test]
fn t_repeated_fixture_label(#[fixture(repeating)] _r: &u32) {}

#[skuld::test(labels = [LEAF])]
fn t_own_equals_fixture_label(#[fixture(leaf)] _l: &u32) {}

#[skuld::test(labels = [OWN, OWN])]
fn t_own_repeated() {}

#[skuld::test(labels = [])]
fn t_explicit_empty(#[fixture(leaf)] _l: &u32) {}

#[skuld::test(labels = [OWN])]
fn t_own_plus_fixture(#[fixture(leaf)] _l: &u32) {}

mod defaulted {
    use super::*;

    skuld::default_labels!(super::MODL);

    #[skuld::test]
    fn t_default_plus_fixture(#[fixture(leaf)] _l: &u32) {}

    /// `labels = []` drops the module default; the fixture's label stays.
    #[skuld::test(labels = [])]
    fn t_default_dropped(#[fixture(leaf)] _l: &u32) {}
}

/// Its path starts with `defaulted`'s as a string, but it is not a child.
mod defaulted_sibling {
    #[skuld::test]
    fn t_sibling_not_defaulted() {}
}

/// Holds a `leaf`-labeled test open (blocks on stdin) for the driver.
#[skuld::test]
fn hold_labeled(#[fixture(leaf)] _l: &u32) {
    use std::io::{BufRead, Write};
    println!("REGISTERED");
    std::io::stdout().flush().expect("flush stdout");
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).expect("read RELEASE");
}

/// Serial against every test labeled `leaf`, including ones labeled only
/// through a fixture.
#[skuld::test(serial = LEAF)]
fn wait_serial_leaf() {}

fn main() {
    skuld::run_all();
}
