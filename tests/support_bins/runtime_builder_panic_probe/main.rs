//! Subject of `tests/runtime_builder_panic_cli.rs`: async tests whose
//! `runtime = ...` builder panics.

fn exploding_builder() -> tokio::runtime::Runtime {
    panic!("builder exploded")
}

#[skuld::test(runtime = exploding_builder)]
async fn builder_panics() {}

#[skuld::test(runtime = exploding_builder, should_panic)]
async fn builder_panics_under_should_panic() {}

#[skuld::test(runtime = exploding_builder, should_panic = "builder exploded")]
async fn builder_panics_under_should_panic_message() {}

fn main() {
    skuld::run_all();
}
