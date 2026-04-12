fn main() {
    // OUT_DIR = target/{profile}/build/{pkg}-{hash}/out
    // Walk up 3 levels to reach target/{profile}/, which is shared by all
    // crates in the workspace. This resolves correctly even with a custom
    // CARGO_TARGET_DIR or .cargo/config.toml target-dir setting.
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let profile_dir = std::path::Path::new(&out_dir)
        .ancestors()
        .nth(3)
        .expect("OUT_DIR must have at least 3 ancestors");
    println!("cargo:rustc-env=SKULD_TARGET_PROFILE_DIR={}", profile_dir.display());
}
