#[skuld::label]
const A: skuld::Label;

const NOT_A_LABEL: u8 = 0;

#[skuld::fixture(labels = A)]
fn no_brackets() -> Result<u8, String> {
    Ok(0)
}

#[skuld::fixture(labels)]
fn no_value() -> Result<u8, String> {
    Ok(0)
}

#[skuld::fixture(labels = ["a"])]
fn string_entry() -> Result<u8, String> {
    Ok(0)
}

#[skuld::fixture(labels = [NOT_A_LABEL])]
fn wrong_type() -> Result<u8, String> {
    Ok(0)
}

#[skuld::fixture(labels = [A,, A])]
fn empty_entry() -> Result<u8, String> {
    Ok(0)
}

#[skuld::fixture(bogus)]
fn unknown_key() -> Result<u8, String> {
    Ok(0)
}

fn main() {}
