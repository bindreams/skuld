//! Subject of `tests/require_known_labels_cli.rs`: declares no labels.

#[skuld::test]
fn t_unlabeled() {}

fn main() {
    let mut runner = skuld::TestRunner::new();
    runner.require_known_labels();
    runner.run()
}
