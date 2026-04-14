# Serial Tests

Some test resources are process-global — environment variables, the current working directory, and similar shared state. Tests that modify them cannot safely run in parallel.

## Marking tests as serial

### Serial with everything

A bare `serial` flag blocks the test against **all** other tests — both serial and non-serial:

```rust
#[skuld::test(serial)]
fn test_with_global_state() {
    std::env::set_var("UNSAFE_BUT_SERIAL", "ok");
}
```

While a `serial` test is running, no other test executes. This is the safest option when your test touches truly process-global state.

### Serial with a filter expression

When only a subset of tests conflict, use `serial = <expr>` to restrict mutual exclusion to tests whose labels match the expression:

```rust
#[skuld::label] const DATABASE: skuld::Label;
#[skuld::label] const FAST: skuld::Label;

#[skuld::test(labels = [DATABASE], serial = DATABASE)]
fn migrate_schema() {
    // Blocks other tests that are serial with DATABASE,
    // but non-DATABASE tests can run in parallel.
}
```

The expression supports boolean operators with bare operator syntax:

| Syntax                       | Meaning                                      |
| ---------------------------- | -------------------------------------------- |
| `serial = DATABASE`          | Serial with tests labeled DATABASE           |
| `serial = DATABASE & !FAST`  | Serial with DATABASE tests that are not FAST |
| `serial = DATABASE \| CACHE` | Serial with DATABASE or CACHE tests          |
| `serial = (A \| B) & !C`     | Grouped expression                           |

Operator precedence: `!` > `&` > `|`. Parentheses override precedence.

Labels used in serial expressions must be `Label` constants in scope (defined with `#[skuld::label]`). The expression matches label names case-insensitively, so `serial = DATABASE` and `serial = database` are equivalent.

## How coordination works

Serial tests are coordinated through a SQLite database, automatically managed by skuld. This works across multiple test processes — if two test binaries run concurrently, their serial constraints are respected across process boundaries.

## Serial fixtures

Fixtures can declare `serial` too. Any test that uses a serial fixture automatically inherits the serial constraint:

```rust
#[skuld::label] const DATABASE: skuld::Label;

#[skuld::fixture(scope = test, serial = DATABASE)]
fn db_conn() -> Result<DbConn, String> {
    Ok(DbConn::new())
}

#[skuld::test]
fn my_test(#[fixture] db_conn: &DbConn) {
    // No `serial` on the test, but db_conn is serial = DATABASE → test is serial with DATABASE.
}
```

A bare `serial` on a fixture works the same as on a test — serial with everything:

```rust
#[skuld::fixture(scope = test, serial)]
fn env() -> Result<EnvGuard, String> {
    Ok(EnvGuard::new())
}
```

This propagation is transitive: if fixture A depends on fixture B, and B has `serial = X`, then any test using A is also serial with X. When multiple fixtures contribute different serial filters, they are combined with OR — the test is serial with the union of all constraints.

## Built-in serial fixtures

The {ref}`env <env-environment-variables>` and {ref}`cwd <cwd-working-directory>` fixtures are both serial because they modify process-global state. You don't need to add `serial` to your tests when using them — it's inherited automatically.

## `LabelFilter` type

For programmatic use (e.g. in dynamic tests), the `LabelFilter` type supports the same operators via Rust's operator overloads:

```rust
use skuld::LabelFilter;

#[skuld::label] const DATABASE: skuld::Label;
#[skuld::label] const FAST: skuld::Label;

let filter: LabelFilter = DATABASE.into();
let filter = DATABASE & !FAST;
let filter = DATABASE | FAST;
```

See [Dynamic Tests](dynamic-tests.md) for using `LabelFilter` with `TestRunner::add_serial_with`.
