fn main() {
    // Without any rerun-if-* directive, cargo falls back to scanning every
    // file in the package — the whole repo — on every build that needs
    // this build script, to decide whether to rerun it. That scan can step
    // into a directory a concurrent build is renaming or removing anywhere
    // under the repo (this is what caused CI run 36320721947's failure).
    // `cargo-skuld/tests/fixtures/test-workspace/.gitignore`'s `/target*`,
    // not this line, is what actually stops that specific failure: this
    // only cuts the scan out of *warm* builds against an
    // already-initialized target directory — the very first build against
    // any given one still runs it regardless, since cargo has no prior
    // record yet to compare against. The only real input here is this
    // file itself (OUT_DIR is cargo-computed per invocation, not a value
    // that changes independently of cargo's own build graph).
    println!("cargo:rerun-if-changed=build.rs");

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
