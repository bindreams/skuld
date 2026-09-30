fn shared() -> tokio::runtime::Handle {
    unimplemented!()
}

#[skuld::test(runtime = shared)]
async fn handle_builder() {}

fn main() {}
