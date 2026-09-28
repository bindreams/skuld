pub mod command;
pub mod discovery;
pub mod emit;
pub mod graph;
pub mod metadata;
// Shared by path with `tests/gen_and_run.rs`, which includes this same
// file under its own `mod` — see `test_support.rs` for why.
#[cfg(test)]
mod test_support;
// Declared here rather than nested inside `test_support.rs` itself, so it
// compiles exactly once: `test_support.rs` is also compiled, via
// `#[path]`, into `gen_and_run.rs`, which has no use for this test module.
#[cfg(test)]
#[path = "test_support/test_support_tests.rs"]
mod test_support_tests;
