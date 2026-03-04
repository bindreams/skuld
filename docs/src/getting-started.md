# Getting Started

## Setup

Add skuld as a dev-dependency and declare a `[[test]]` target with `harness = false` in your `Cargo.toml`:

```toml
[dev-dependencies]
skuld = { path = "skuld" }

[[test]]
name = "my_tests"
path = "tests/my_tests.rs"
harness = false
```

Create the test entry point. Tests live in a support module alongside the entry point:

```rust
// tests/my_tests.rs
#[path = "my_tests_support/mod.rs"]
mod support;

fn main() {
    skuld::run_all();
}
```

## Writing your first test

Annotate test functions with `#[skuld::test]`. Every annotated function is automatically registered with the harness:

```rust
// tests/my_tests_support/mod.rs

#[skuld::test]
fn basic_test() {
    assert_eq!(2 + 2, 4);
}
```

Run with:

```bash
cargo test
```

## Adding preconditions

Declare runtime requirements with `requires`. Each requirement is a function `fn() -> Result<(), String>` — returning `Ok(())` means the requirement is met, `Err(reason)` means it's not:

```rust
fn valgrind() -> Result<(), String> {
    skuld::probe_executable("valgrind")
}

fn my_binary() -> Result<(), String> {
    skuld::probe_path("target/debug/my_binary")
}

#[skuld::test(requires = [valgrind, my_binary])]
fn smoke_test() {
    // This body only runs if both valgrind and my_binary are available.
}
```

When a requirement is missing, the test shows as `ignored` and the reason appears in the unavailability summary:

```
running 2 tests
test smoke_test     ... ignored
test full_pipeline  ... ignored

test result: ok. 0 passed; 0 failed; 2 ignored

--- Unavailable (2) ---
  smoke_test:     valgrind not installed
  full_pipeline:  valgrind not installed
```

## Built-in probe helpers

| Function                 | Checks                      |
| ------------------------ | --------------------------- |
| `probe_executable(name)` | `<name> --version` succeeds |
| `probe_path(path)`       | File or directory exists    |

## Next steps

- [Writing Tests](writing-tests.md) — all `#[skuld::test]` options
- [Fixtures](fixtures.md) — dependency-injected test resources
- [Labels](labels.md) — filter tests from the command line
