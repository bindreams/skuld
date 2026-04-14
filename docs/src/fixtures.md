# Fixtures

Fixtures provide dependency-injected values to test functions. Define a fixture with `#[skuld::fixture]` and inject it into a test with `#[fixture]` on a parameter.

## Basic usage

```rust
use std::path::Path;

#[skuld::test]
fn my_test(#[fixture(temp_dir)] dir: &Path) {
    assert!(dir.exists());
}
```

The `#[fixture(temp_dir)]` annotation tells skuld to look up the fixture named `temp_dir` and inject it as the parameter's value. If the parameter name matches the fixture name, a bare `#[fixture]` suffices:

```rust
#[skuld::test]
fn my_test(#[fixture] temp_dir: &Path) {
    // Same as #[fixture(temp_dir)] — parameter name matches fixture name.
}
```

## Defining fixtures

A fixture is a function annotated with `#[skuld::fixture]` that returns `Result<T, String>`:

```rust
pub struct MyResource { /* ... */ }

#[skuld::fixture]
fn my_resource() -> Result<MyResource, String> {
    MyResource::create().map_err(|e| format!("setup failed: {e}"))
}
```

The `#[skuld::fixture]` attribute supports these options:

| Option                            | Description                                                                            |
| --------------------------------- | -------------------------------------------------------------------------------------- |
| `scope = variable\|test\|process` | Lifetime scope (default: `variable`)                                                   |
| `requires = [...]`                | Runtime preconditions (propagated to tests)                                            |
| `name = "..."`                    | Override the fixture name (default: function name)                                     |
| `deref`                           | Also support injection as `Deref::Target` type                                         |
| `serial` or `serial = <expr>`     | Tests using this fixture inherit the serial constraint (see [Serial Tests](serial.md)) |

## Scopes

Each fixture has a lifetime scope that controls when it's created and destroyed:

| Scope                | Behaviour                                                             |
| -------------------- | --------------------------------------------------------------------- |
| `variable` (default) | Fresh instance per injection. Dropped when the `FixtureHandle` drops. |
| `test`               | Cached per test. Dropped when the test ends.                          |
| `process`            | Cached globally. Dropped after all tests finish (LIFO).               |

```rust
#[skuld::fixture(scope = test)]
fn db_connection() -> Result<DbConn, String> { /* ... */ }

#[skuld::fixture(scope = process, requires = [docker_available])]
fn corpus_image() -> Result<CorpusImage, String> { /* ... */ }
```

**Scope dependency rule:** a fixture may only depend on fixtures of the **same or wider** scope (`variable` < `test` < `process`). Dependency cycles are detected at startup.

## Fixture dependencies

Fixtures can depend on other fixtures using the same `#[fixture]` parameter syntax:

```rust
#[skuld::fixture(scope = test, deref)]
fn test_name() -> Result<TestName, String> { /* ... */ }

#[skuld::fixture(deref)]
fn temp_dir(#[fixture(test_name)] name: &str) -> Result<TempDir, String> {
    // `name` is injected from the test_name fixture.
    tempfile::Builder::new()
        .prefix(&format!("{name}-"))
        .tempdir()
        .map(|inner| TempDir { inner })
        .map_err(|e| format!("failed to create temp dir: {e}"))
}
```

## Deref coercion

Fixtures annotated with `deref` can be injected as either their own type or their `Deref::Target`:

```rust
// TempDir implements Deref<Target = Path>, so both work:
fn example1(#[fixture(temp_dir)] dir: &skuld::TempDir) { /* ... */ }
fn example2(#[fixture(temp_dir)] dir: &Path) { /* ... */ }
```

## Requirement propagation

If a fixture declares `requires = [...]`, any test using that fixture automatically inherits those requirements — even without listing them in the test's own `requires`. This is transitive: if fixture A depends on fixture B which has `requires = [docker]`, a test using fixture A will also require docker.

## Built-in fixtures

| Fixture     | Scope    | Type                         | Serial | Description                                    |
| ----------- | -------- | ---------------------------- | ------ | ---------------------------------------------- |
| `test_name` | test     | `TestName` (deref to `&str`) | no     | Current test function name                     |
| `temp_dir`  | variable | `TempDir` (deref to `&Path`) | no     | Temporary directory named after the test       |
| `env`       | test     | `EnvGuard`                   | yes    | Set/remove env vars with automatic revert      |
| `cwd`       | test     | `CwdGuard`                   | yes    | Change working directory with automatic revert |

(env-environment-variables)=

### `env` — environment variables

The `env` fixture provides an `EnvGuard` for safely modifying environment variables. All changes are reverted when the test ends:

```rust
#[skuld::test]
fn my_test(#[fixture] env: &skuld::EnvGuard) {
    env.set("DATABASE_URL", "sqlite::memory:");
    env.remove("PROD_SECRET");
    // Both changes are automatically reverted after the test.
}
```

Because environment variables are process-global, the `env` fixture is marked `serial` — tests using it never run in parallel.

(cwd-working-directory)=

### `cwd` — working directory

The `cwd` fixture provides a `CwdGuard` for safely changing the working directory. It maintains a stack of directories, so `back()` returns to the previous one (like `cd -`):

```rust
#[skuld::test]
fn my_test(#[fixture] cwd: &skuld::CwdGuard, #[fixture(temp_dir)] dir: &Path) {
    cwd.set(dir);
    assert_eq!(std::env::current_dir().unwrap(), dir);
    cwd.back();  // Return to the previous directory.
    // The original directory is restored when the test ends regardless.
}
```

Like `env`, the `cwd` fixture is `serial`.

## Serial fixtures

Fixtures support the same `serial` syntax as tests. A bare `serial` means serial with everything; `serial = <expr>` applies a filter:

```rust
#[skuld::label] const DATABASE: skuld::Label;

#[skuld::fixture(scope = test, serial = DATABASE)]
fn db_conn() -> Result<DbConn, String> {
    Ok(DbConn::new())
}
```

Any test using `db_conn` inherits `serial = DATABASE` without declaring it. When multiple fixtures contribute different serial filters, they are combined with OR. See [Serial Tests](serial.md) for details.

## Tool fixtures

Fixtures like `env` and `cwd` are "tool fixtures" — instead of returning a value to read, they return an object with methods that the test calls. This pattern is useful for any fixture that needs test-specific arguments:

```rust
#[skuld::fixture(scope = test, serial)]
fn env() -> Result<EnvGuard, String> {
    Ok(EnvGuard::new())  // Returns a tool, not a value.
}

#[skuld::test]
fn my_test(#[fixture] env: &EnvGuard) {
    env.set("KEY", "value");  // Test provides "parameters" as method calls.
}
```

## Eager initialization

Process-scoped fixtures can be pre-initialized before tests run using `warm_up`:

```rust
fn main() {
    skuld::warm_up("corpus_image");  // Initialize now, not on first use.
    skuld::run_all();
}
```
