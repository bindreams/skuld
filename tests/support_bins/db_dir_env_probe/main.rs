//! Subject of `tests/relocated_binary_cli.rs`.
//!
//! Two sequential trials: the first sets `SKULD_DB_DIR` (from `PROBE_OTHER_DB_DIR`) in its own
//! body, the second starts afterwards. The driver asserts the second trial did not register in
//! the DB that variable names.

fn main() {
    let other = std::env::var("PROBE_OTHER_DB_DIR").expect("driver must set PROBE_OTHER_DB_DIR");
    let mut runner = skuld::TestRunner::new();
    runner.add("a_sets_env", &[], false, move || {
        std::env::set_var("SKULD_DB_DIR", &other)
    });
    runner.add("b_starts_after", &[], false, || {});
    runner.run();
}
