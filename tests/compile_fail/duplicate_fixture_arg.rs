fn ok() -> Result<(), String> {
    Ok(())
}

#[skuld::label]
const A: skuld::Label;

#[skuld::fixture(requires = [ok], requires = [ok])]
fn dup_requires() -> Result<u8, String> {
    Ok(0)
}

#[skuld::fixture(scope = test, scope = process)]
fn dup_scope() -> Result<u8, String> {
    Ok(0)
}

#[skuld::fixture(name = "a", name = "b")]
fn dup_name() -> Result<u8, String> {
    Ok(0)
}

#[skuld::fixture(deref, deref)]
fn dup_deref() -> Result<Box<u8>, String> {
    Ok(Box::new(0))
}

#[skuld::fixture(serial, serial = A)]
fn dup_serial() -> Result<u8, String> {
    Ok(0)
}

#[skuld::fixture(name = "a", deref, name = "b")]
fn dup_name_non_adjacent() -> Result<Box<u8>, String> {
    Ok(Box::new(0))
}

#[skuld::fixture(scope = test, serial, scope = process)]
fn dup_scope_non_adjacent() -> Result<u8, String> {
    Ok(0)
}

fn main() {}
