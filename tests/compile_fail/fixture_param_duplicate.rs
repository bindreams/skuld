#[skuld::fixture]
fn a() -> Result<u8, String> {
    Ok(0)
}

#[skuld::fixture]
fn b() -> Result<u8, String> {
    Ok(0)
}

#[skuld::test]
fn test_repeats(#[fixture(a)] #[fixture(b)] x: &u8) {}

#[skuld::test]
fn test_repeats_non_adjacent(#[fixture(a)] #[allow(unused_variables)] #[fixture(b)] x: &u8) {}

#[skuld::fixture]
fn fixture_repeats(#[fixture(a)] #[fixture(b)] x: &u8) -> Result<u8, String> {
    Ok(*x)
}

#[skuld::fixture]
fn fixture_repeats_non_adjacent(#[fixture(a)] #[allow(unused_variables)] #[fixture(b)] x: &u8) -> Result<u8, String> {
    Ok(*x)
}

fn main() {}
