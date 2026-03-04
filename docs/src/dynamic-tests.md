# Dynamic Tests

For tests generated at runtime (e.g. from data files), use `TestRunner` instead of `run_all()`.

## Basic usage

```rust
fn main() {
    let mut runner = skuld::TestRunner::new();

    for file in std::fs::read_dir("test_data").unwrap() {
        let path = file.unwrap().path();
        runner.add(
            path.display().to_string(),
            &["data"],        // labels
            false,             // ignored
            move || {
                let content = std::fs::read_to_string(&path).unwrap();
                assert!(!content.is_empty());
            },
        );
    }

    runner.run();  // Runs both dynamic and #[skuld::test] tests, then exits.
}
```

Dynamic tests are collected alongside `#[skuld::test]`-registered tests. They support labels and label filtering.

## Serial dynamic tests

Use `add_serial` for dynamic tests that need the serial mutex:

```rust
runner.add_serial(
    "env-sensitive test",
    &["integration"],
    false,
    || { /* body */ },
);
```

## Custom CLI arguments

If your test binary accepts custom flags that would otherwise be rejected by the argument parser, strip them:

```rust
let mut runner = skuld::TestRunner::new();
runner.strip_args(&["--no-sandbox", "--headless"]);
// ... add tests ...
runner.run();
```

## `run()` vs `run_tests()`

| Method | Returns | Use case |
| --- | --- | --- |
| `run()` | `!` (exits) | Normal usage — runs tests and exits with the appropriate code. |
| `run_tests()` | `Conclusion` | When you need post-run assertions before exiting. Call `conclusion.exit()` when done. |

```rust
fn main() {
    let conclusion = skuld::TestRunner::new().run_tests();

    // Post-run checks...
    assert!(some_condition());

    conclusion.exit();
}
```
