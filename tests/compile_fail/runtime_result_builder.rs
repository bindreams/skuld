fn fallible() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread().build()
}

#[skuld::test(runtime = fallible)]
async fn result_builder() {}

fn main() {}
