fn main() {
    if let Ok(path) = std::env::var("SKULD_NEXTEST_METADATA_PATH") {
        std::fs::write(path, b"not valid json").unwrap();
    }
}
