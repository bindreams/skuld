fn a() -> tokio::runtime::Runtime {
    unimplemented!()
}

fn b() -> tokio::runtime::Runtime {
    unimplemented!()
}

#[skuld::test(runtime = a, runtime = b)]
async fn duplicate_adjacent() {}

#[skuld::test(runtime = a, should_panic, runtime = b)]
async fn duplicate_non_adjacent() {}

fn main() {}
