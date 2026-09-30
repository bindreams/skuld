//! Not a trybuild case: this package's dev-dependencies enable the `tokio`
//! feature, and cargo unifies features per package. So this builds a scratch
//! crate that depends on skuld with default features, from the same lockfile.

use std::process::Command;

#[test]
fn runtime_arg_requires_the_tokio_feature() {
    let skuld_dir = env!("CARGO_MANIFEST_DIR");
    let dir = tempfile::tempdir().expect("scratch crate dir");
    std::fs::create_dir(dir.path().join("src")).unwrap();
    let table = |entries: &[(&str, toml::Value)]| -> toml::Table {
        entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    };
    let manifest = table(&[
        (
            "package",
            table(&[
                ("name", "no_tokio_probe".into()),
                ("version", "0.0.0".into()),
                ("edition", "2021".into()),
            ])
            .into(),
        ),
        ("workspace", table(&[]).into()),
        (
            "dependencies",
            table(&[("skuld", table(&[("path", skuld_dir.into())]).into())]).into(),
        ),
    ]);
    std::fs::write(dir.path().join("Cargo.toml"), toml::to_string(&manifest).unwrap()).unwrap();
    std::fs::copy(
        std::path::Path::new(skuld_dir).join("Cargo.lock"),
        dir.path().join("Cargo.lock"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/main.rs"),
        "fn builder() {}\n\n#[skuld::test(runtime = builder)]\nasync fn t() {}\n\nfn main() {}\n",
    )
    .unwrap();

    let out = Command::new(env!("CARGO"))
        .args(["check", "--offline", "--quiet", "--manifest-path"])
        .arg(dir.path().join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", env!("CARGO_TARGET_TMPDIR"))
        .env_remove("RUSTFLAGS")
        .output()
        .expect("spawn cargo check");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "expected a compile error; stderr:\n{stderr}");
    assert!(
        stderr.contains("requires skuld's `tokio` feature"),
        "the error must say the tokio feature is required; stderr:\n{stderr}"
    );
}
