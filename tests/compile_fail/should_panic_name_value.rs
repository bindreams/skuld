#[skuld::test]
#[should_panic = "msg"]
fn should_panic_name_value() {
    panic!("msg");
}

fn main() {}
