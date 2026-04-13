#[skuld::test(should_panic)]
#[should_panic]
fn conflicting_should_panic() {
    panic!("boom");
}

fn main() {}
