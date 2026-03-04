# Writing Tests

## The `#[skuld::test]` attribute

Every test function must be annotated with `#[skuld::test]`. The attribute registers the function with the harness and supports the following options:

```rust
#[skuld::test]                                     // no options
#[skuld::test(requires = [valgrind, my_binary])]   // runtime preconditions
#[skuld::test(name = "custom display name")]        // custom name in output
#[skuld::test(labels = [docker, slow])]             // labels for filtering
#[skuld::test(ignore)]                              // statically ignored
#[skuld::test(ignore = "blocked on #123")]          // ignored with reason
#[skuld::test(serial)]                              // run under global mutex
```

Options can be combined:

```rust
#[skuld::test(requires = [docker], labels = [integration], serial)]
fn heavy_test() { /* ... */ }
```

:::{note}
Do not add `#[test]` alongside `#[skuld::test]` — the skuld macro already registers the function. Adding both will produce a compile error.
:::

## Preconditions

Each entry in `requires = [...]` must be a function with signature `fn() -> Result<(), String>`:

```rust
fn docker() -> Result<(), String> {
    skuld::probe_executable("docker")
}

fn corpus_exists() -> Result<(), String> {
    skuld::probe_path("test_data/corpus")
}

#[skuld::test(requires = [docker, corpus_exists])]
fn integration_test() {
    // Runs only if both checks pass.
}
```

If any requirement returns `Err`, the test is marked `ignored` (not failed) and the reason is collected for the unavailability summary.

Fixture requirements also propagate: if a test uses a fixture that has `requires = [...]`, those requirements are checked too. See [Fixtures](fixtures.md) for details.

## Custom display names

By default, the test name in output is the function name. Override it with `name`:

```rust
#[skuld::test(name = "arithmetic: 2 + 2 = 4")]
fn test_add() {
    assert_eq!(2 + 2, 4);
}
```

## Static ignore

Mark a test as statically ignored (always skipped, no precondition check):

```rust
#[skuld::test(ignore)]
fn work_in_progress() { /* ... */ }

#[skuld::test(ignore = "blocked on #123")]
fn blocked_test() { /* ... */ }
```

Statically ignored tests do not appear in the unavailability summary.

## Output

When all requirements are met:

```
running 3 tests
test smoke_test     ... ok
test full_pipeline  ... ok
test unit_test      ... ok

test result: ok. 3 passed; 0 failed; 0 ignored
```

When requirements are missing:

```
running 3 tests
test smoke_test     ... ignored
test full_pipeline  ... ignored
test unit_test      ... ok

test result: ok. 1 passed; 0 failed; 2 ignored

--- Unavailable (2) ---
  smoke_test:     valgrind not installed
  full_pipeline:  valgrind not installed
```
