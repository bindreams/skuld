//! Subject of `tests/require_known_labels_cli.rs`. Calls
//! `require_known_labels()` unless `KNOWN_LABELS_FIXTURE_LENIENT` is set.

#[skuld::label]
pub const ALPHA: skuld::Label;
#[skuld::label]
pub const BETA: skuld::Label;

#[skuld::test(labels = [ALPHA])]
fn t_alpha() {}

#[skuld::test(labels = [BETA])]
fn t_beta() {}

fn main() {
    let mut runner = skuld::TestRunner::new();
    if std::env::var_os("KNOWN_LABELS_FIXTURE_LENIENT").is_none() {
        runner.require_known_labels();
    }
    runner.run()
}
