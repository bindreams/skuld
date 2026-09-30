#[skuld::fixture]
fn a() -> Result<u8, String> {
    Ok(0)
}

#[skuld::fixture]
fn b() -> Result<u8, String> {
    Ok(0)
}

#[skuld::test]
fn test_two_names(#[fixture(a, b)] a: &u8) {}

#[skuld::test]
fn test_name_value(#[fixture = "a"] a: &u8) {}

#[skuld::test]
fn test_string(#[fixture("a")] a: &u8) {}

#[skuld::test]
fn test_assignment(#[fixture(a = b)] a: &u8) {}

#[skuld::fixture]
fn fixture_two_names(#[fixture(a, b)] a: &u8) -> Result<u8, String> {
    Ok(*a)
}

#[skuld::fixture]
fn fixture_name_value(#[fixture = "a"] a: &u8) -> Result<u8, String> {
    Ok(*a)
}

#[skuld::fixture]
fn fixture_string(#[fixture("a")] a: &u8) -> Result<u8, String> {
    Ok(*a)
}

fn main() {}
