#[skuld::test]
#[should_panic]
#[should_panic]
fn duplicate_should_panic() {
    panic!("boom");
}

fn main() {}
