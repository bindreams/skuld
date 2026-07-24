// a well-formed dump where ONE test's serial_filter is unparsable ("(" —
// unmatched paren), alongside a valid sibling, to prove per-TEST (not
// per-binary) exclusion.
fn main() {
    if let Ok(path) = std::env::var("SKULD_NEXTEST_METADATA_PATH") {
        let json = r#"{"tests":[
            {"name":"good_test","labels":[],"serial_filter":""},
            {"name":"bad_test","labels":[],"serial_filter":"("}
        ]}"#;
        std::fs::write(path, json).unwrap();
    }
}
