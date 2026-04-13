# skuld

Test harness for Rust with runtime preconditions, fixture injection, and label filtering.

Rust's built-in test framework has no way to mark a test as "ignored with reason" at runtime. Tests that need external tools (valgrind, docker, a built binary) either silently pass when the tool is missing, or hard-fail. `skuld` replaces the built-in harness with one that checks preconditions at runtime, reports unmet ones as `ignored`, and prints a summary showing exactly what's missing.

## Setup

Add a `[[test]]` target with `harness = false` in your `Cargo.toml`:

```toml
[dev-dependencies]
skuld = { path = "skuld" }

[[test]]
name = "my_tests"
path = "tests/my_tests.rs"
harness = false
```

Create the test entry point:

```rust
// tests/my_tests.rs
#[path = "my_tests_support/mod.rs"]
mod support;

fn main() {
    skuld::run_all();
}
```

## Unit tests

To use `skuld` for tests inside `src/`, disable the default harness for the library target and add the entry point:

```toml
[lib]
harness = false
```

```rust
// lib.rs
#[cfg(test)]
fn main() {
    skuld::run_all();
}
```

Now `#[skuld::test]` works in any `#[cfg(test)]` module under `src/`:

```rust
// src/my_module.rs
#[cfg(test)]
mod tests {
    #[skuld::test]
    fn unit_test_example() {
        assert_eq!(2 + 2, 4);
    }
}
```

> **Note:** Without `[lib] harness = false`, the default Rust test harness runs instead of skuld, silently reporting `running 0 tests` with no error.

## Writing tests

Annotate test functions with `#[skuld::test]`. The attribute supports several options:

```rust
fn valgrind() -> Result<(), String> {
    use std::process::{Command, Stdio};
    Command::new("valgrind")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
        .then_some(())
        .ok_or_else(|| "valgrind not installed".into())
}

#[skuld::test(requires = [valgrind], labels = [slow])]
fn smoke_test() {
    // Runs only if valgrind is available.
}

#[skuld::test(name = "custom display name")]
fn internal_name() { /* ... */ }

#[skuld::test(ignore)]
fn wip() { /* ... */ }

#[skuld::test(ignore = "blocked on #123")]
fn blocked_test() { /* ... */ }

#[skuld::test(serial)]
fn modifies_global_state() { /* ... */ }

#[skuld::test(should_panic)]
fn panics_on_bad_input() {
    my_function(invalid_input);
}

#[skuld::test(should_panic = "out of range")]
fn panics_with_message() {
    my_function(too_large);
}
```

Every `#[skuld::test]` function is registered with the harness. Functions without `#[skuld::test]` are invisible to skuld.

## Async tests

Enable the `tokio` feature to use `async fn` test bodies:

```toml
[dev-dependencies]
skuld = { path = "skuld", features = ["tokio"] }
```

```rust
#[skuld::test]
async fn connects_to_server() {
    let stream = tokio::net::TcpStream::connect("127.0.0.1:8080").await.unwrap();
    // ...
}
```

Async tests run on a single-threaded tokio runtime (`current_thread` with `enable_all()`). All existing features — fixtures, `requires`, `should_panic`, `serial`, labels — work with async tests.

Tests may also return `Result<(), E>` where `E: Debug`. An `Err` return fails the test:

```rust
#[skuld::test]
async fn parses_config() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config().await?;
    assert_eq!(config.port, 8080);
    Ok(())
}
```

## Fixtures

Fixtures provide dependency-injected values to test functions. Define a fixture with `#[skuld::fixture]` and inject it with `#[fixture]` on a test parameter:

```rust
use std::path::Path;

#[skuld::fixture(deref)]
fn temp_dir(#[fixture(test_name)] name: &str) -> Result<skuld::TempDir, String> {
    // skuld provides TempDir and TestName as built-in fixtures.
    // This example shows how custom fixtures work.
    todo!()
}

#[skuld::test]
fn my_test(#[fixture(temp_dir)] dir: &Path) {
    assert!(dir.exists());
}
```

### Scopes

Each fixture has a lifetime scope:

| Scope                | Behaviour                                                           |
| -------------------- | ------------------------------------------------------------------- |
| `variable` (default) | Fresh instance per request. Dropped when the `FixtureHandle` drops. |
| `test`               | Cached per test. Dropped when the test ends.                        |
| `process`            | Cached globally. Dropped after all tests finish (LIFO).             |

```rust
#[skuld::fixture(scope = process, requires = [docker_available])]
fn corpus_image() -> Result<CorpusImage, String> { /* ... */ }
```

A fixture may only depend on fixtures of the **same or wider** scope. Dependency cycles are detected at startup.

### Built-in fixtures

| Fixture     | Scope    | Type                         | Serial | Description                                    |
| ----------- | -------- | ---------------------------- | ------ | ---------------------------------------------- |
| `test_name` | test     | `TestName` (deref to `&str`) | no     | Current test function name                     |
| `temp_dir`  | variable | `TempDir` (deref to `&Path`) | no     | Temporary directory named after the test       |
| `env`       | test     | `EnvGuard`                   | yes    | Set/remove env vars with automatic revert      |
| `cwd`       | test     | `CwdGuard`                   | yes    | Change working directory with automatic revert |

### Deref coercion

Fixtures annotated with `deref` can be injected as their `Deref::Target` type:

```rust
// TempDir implements Deref<Target = Path>, so both work:
fn example1(#[fixture(temp_dir)] dir: &skuld::TempDir) { /* ... */ }
fn example2(#[fixture(temp_dir)] dir: &Path) { /* ... */ }
```

## Labels

Tests can be labeled for selective execution:

```rust
#[skuld::test(labels = [docker, slow])]
fn heavy_test() { /* ... */ }
```

Filter from the command line:

```bash
cargo test -- --label docker          # run only tests labeled "docker"
cargo test -- --label=docker,!slow    # docker tests, excluding slow ones
```

### Module-level defaults

```rust
skuld::default_labels!(smoke, unit);

#[skuld::test]                      // inherits [smoke, unit]
fn test_a() { /* ... */ }

#[skuld::test(labels = [slow])]     // gets [slow], NOT [smoke, unit, slow]
fn test_b() { /* ... */ }

#[skuld::test(labels = [])]         // gets nothing (explicit opt-out)
fn test_c() { /* ... */ }
```

## Serial tests

Tests that modify process-global state (environment variables, current directory) must not run in parallel with other such tests. Mark them with `serial`:

```rust
#[skuld::test(serial)]
fn test_with_global_state() { /* ... */ }
```

Fixtures can also declare `serial`. Any test using a serial fixture automatically inherits the flag:

```rust
#[skuld::fixture(scope = test, serial)]
fn env() -> Result<EnvGuard, String> { /* ... */ }

#[skuld::test]
fn my_test(#[fixture] env: &EnvGuard) {
    // Automatically serial — env fixture declares it.
    env.set("MY_VAR", "value");
}
```

All serial tests run under a cross-process file lock (`target/{profile}/.skuld-serial.lock`). Under `cargo test` the lock is trivially uncontended; under `cargo nextest run` (process-per-test) it serializes across processes automatically. Non-serial tests are unaffected and may still run in parallel.

## Dynamic tests

Use `TestRunner` to mix inventory-registered and runtime-generated tests:

```rust
fn main() {
    let mut runner = skuld::TestRunner::new();
    for file in std::fs::read_dir("test_data").unwrap() {
        let path = file.unwrap().path();
        runner.add(
            path.display().to_string(),
            &["data"],
            false,
            move || { /* test body */ },
        );
    }
    runner.run();
}
```

## Running tests

### Capture model

Under `cargo test`, skuld captures each test's `stdout` and `stderr` via a file-descriptor redirect (`dup2` on Unix; `SetStdHandle` + `_dup2` on Windows). On pass the captured bytes are discarded; on failure they are dumped to the real `stderr` between `---- captured ----` markers, followed by the panic. The capture intercepts at the FD level, so every write — `println!`, `eprintln!`, raw `io::stdout().write_all`, FFI output, tracing subscribers installed by the test body, and even output from spawned child processes — is captured. Tests are free to install their own `tracing_subscriber::registry().try_init()` and skuld stays out of the dispatch path entirely.

Because FD redirect is a process-wide operation, capture mode forces `--test-threads=1`. For parallel execution, either run with `--nocapture` or use `cargo nextest run` (recommended for large suites — nextest runs each test in its own subprocess and captures via OS pipes externally, so skuld's in-process redirect is unnecessary and disabled automatically). Serial tests are safe under nextest: the `serial` lock uses a cross-process file lock, so `#[skuld::test(serial)]` correctly serializes even when nextest spawns separate processes.

```bash
cargo test                      # default: FD capture, serial, silent on pass
cargo test -- --nocapture       # no capture, default parallelism, all output visible
cargo nextest run               # process-per-test parallelism via nextest
```

### `SKULD_DEBUG=1`

Set `SKULD_DEBUG=1` to get diagnostic lines around each test's execution, useful for debugging capture setup or runner behavior:

```bash
SKULD_DEBUG=1 cargo test
# ...
# [skuld] my_test: starting
# [skuld-debug] my_test: entering test scope
# [skuld-debug] my_test: capture enabled (fd redirect)
# [skuld-debug] my_test: capture disabled
# [skuld] my_test: pass (3 ms)
```

### A note on `tracing-subscriber`'s `tracing-log` feature

If your test code or code under test pulls in `tracing-subscriber` directly, **do not enable its `tracing-log` feature**. The feature auto-installs a `log::Log` shim on the first subscriber `init`, which mutates `log::max_level` globally. Downstream projects have hit Windows CI timeout regressions from this — see bindreams/hole#147. If you need the `log`→`tracing` bridge, call `tracing_log::LogTracer::init()` yourself in the test that needs it, and accept that doing so is a process-wide, one-time operation.

## Output

When all requirements are met:

```
running 2 tests
test smoke_test     ... ok
test full_pipeline  ... ok

test result: ok. 2 passed; 0 failed; 0 ignored
```

When a requirement is missing:

```
running 2 tests
test smoke_test     ... ignored
test full_pipeline  ... ignored

test result: ok. 0 passed; 0 failed; 2 ignored

--- Unavailable (2) ---
  smoke_test:     valgrind not installed
  full_pipeline:  valgrind not installed
```

## How it works

1. `#[skuld::test]` is a proc macro that preserves the original function and appends an `inventory::submit!` call to register it with the harness.
1. `run_all()` (or `TestRunner::run_tests()`) iterates all registered tests, checks preconditions and fixture requirements at runtime, and builds `libtest-mimic::Trial`s — marking unmet tests as ignored.
1. After `libtest-mimic::run()` completes, the unavailability summary is printed to stderr.

## License

<img align="right" width="150px" height="150px" src="https://www.apache.org/foundation/press/kit/img/the-apache-way-badge/Indigo-THE_APACHE_WAY_BADGE-rgb.svg">

Copyright 2026, Anna Zhukova

This project is licensed under the Apache 2.0 license. The license text can be found at [LICENSE.md](/LICENSE.md).

## About

<img align="right" width="122px" height="180px" src="docs/src/_static/norns.jpg">

**Skuld** is the youngest of the three Norns in Norse mythology — the weavers of fate who sit beneath the world-tree Yggdrasil. While her sisters Urðr and Verðanði govern the past and the present, Skuld presides over *what shall be*: obligations yet unfulfilled, debts yet unpaid. Her name shares its root with the English word *should*.
