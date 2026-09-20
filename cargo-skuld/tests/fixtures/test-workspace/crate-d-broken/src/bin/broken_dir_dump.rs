// a directory at the dump path makes read_to_string fail portably on both
// Windows and Unix.
fn main() {
    if let Ok(path) = std::env::var("SKULD_NEXTEST_METADATA_PATH") {
        std::fs::create_dir(path).unwrap();
    }
}
