# Serial Tests

Some test resources are process-global — environment variables, the current working directory, and similar shared state. Tests that modify them cannot safely run in parallel.

## Marking tests as serial

```rust
#[skuld::test(serial)]
fn test_with_global_state() {
    std::env::set_var("UNSAFE_BUT_SERIAL", "ok");
}
```

All serial tests run under a single global mutex. Non-serial tests are unaffected and may still run in parallel with each other.

## Serial fixtures

Fixtures can declare `serial` too. Any test that uses a serial fixture automatically becomes serial:

```rust
#[skuld::fixture(scope = test, serial)]
fn env() -> Result<EnvGuard, String> {
    Ok(EnvGuard::new())
}

#[skuld::test]
fn my_test(#[fixture] env: &EnvGuard) {
    // No `serial` on the test, but env is serial → test is serial.
    env.set("MY_VAR", "value");
}
```

This propagation is transitive: if fixture A depends on fixture B, and B is serial, then any test using A is also serial.

## Built-in serial fixtures

The {ref}`env <env-environment-variables>` and {ref}`cwd <cwd-working-directory>` fixtures are both serial because they modify process-global state. You don't need to add `serial` to your tests when using them — it's inherited automatically.
