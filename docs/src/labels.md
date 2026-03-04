# Labels

Tests can be labeled for selective execution from the command line.

## Labeling tests

Add labels with the `labels` option:

```rust
#[skuld::test(labels = [docker, slow])]
fn heavy_test() { /* ... */ }

#[skuld::test(labels = [unit])]
fn fast_test() { /* ... */ }
```

## Filtering from the command line

Use `--label` to filter tests. Arguments after `--` are passed to the test binary:

```bash
cargo test -- --label docker          # run only tests labeled "docker"
cargo test -- --label=docker,!slow    # docker tests, excluding slow ones
cargo test -- --label unit            # only unit tests
```

**Filtering rules:**
- **Includes** form a union: the test must match **any** include.
- **Excludes** subtract: the test must not match **any** exclude (prefix with `!`).
- **No includes** → all tests are included by default.
- **No selectors** → all tests pass the filter.

Multiple `--label` flags are supported:

```bash
cargo test -- --label docker --label integration    # union of both
```

## Module-level defaults

Use `default_labels!` to set default labels for all `#[skuld::test]` functions in a module:

```rust
skuld::default_labels!(smoke, unit);

#[skuld::test]                      // inherits [smoke, unit]
fn test_a() { /* ... */ }

#[skuld::test(labels = [slow])]     // gets [slow], NOT [smoke, unit, slow]
fn test_b() { /* ... */ }

#[skuld::test(labels = [])]         // gets nothing (explicit opt-out)
fn test_c() { /* ... */ }
```

Explicit `labels = [...]` (including empty) **fully replaces** the module defaults — there is no merging.

Default labels are matched by module path prefix, so a `default_labels!` in a parent module applies to all children unless overridden.
