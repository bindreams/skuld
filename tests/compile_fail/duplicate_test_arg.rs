fn ok() -> Result<(), String> {
    Ok(())
}

#[skuld::label]
const A: skuld::Label;

#[skuld::test(requires = [ok], requires = [ok])]
fn dup_requires() {}

#[skuld::test(name = "a", name = "b")]
fn dup_name() {}

#[skuld::test(labels = [A], labels = [A])]
fn dup_labels() {}

#[skuld::test(ignore, ignore = "why")]
fn dup_ignore() {}

#[skuld::test(serial, serial = A)]
fn dup_serial() {}

#[skuld::test(should_panic, should_panic = "x")]
fn dup_should_panic() {}

#[skuld::test(labels = [A], name = "x", labels = [A])]
fn dup_labels_non_adjacent() {}

#[skuld::test(requires = [ok], should_panic, requires = [ok])]
fn dup_requires_non_adjacent() {}

fn main() {}
