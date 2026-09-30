fn builder() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().build().unwrap()
}

#[skuld::test(runtime = builder)]
fn sync_with_runtime() {}

fn main() {}
