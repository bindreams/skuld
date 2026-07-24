use super::*;

#[test]
fn builds_expected_argv() {
    let cmd = build_nextest_run_command(Path::new("target/skuld/nextest.toml"), &["--no-fail-fast".to_string()]);
    assert_eq!(cmd.get_program(), "cargo");
    let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
    assert_eq!(
        args,
        vec![
            "nextest",
            "run",
            "--tool-config-file",
            "skuld:target/skuld/nextest.toml",
            "--no-fail-fast"
        ]
    );
}

#[test]
fn passthrough_args_are_appended_in_order() {
    let cmd = build_nextest_run_command(Path::new("x.toml"), &["-E".to_string(), "test(foo)".to_string()]);
    let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
    assert_eq!(args.last().unwrap(), &"test(foo)");
    assert_eq!(args[args.len() - 2], "-E");
}
