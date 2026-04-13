# Labels

Labels are sentinel values for tagging and filtering tests.

## Defining labels

Use `new_label!` to define a label constant:

```rust
skuld::new_label!(pub DOCKER, "docker");
skuld::new_label!(pub SLOW, "slow");
skuld::new_label!(pub UNIT, "unit");
```

To reference a label defined elsewhere (e.g. in another crate), use `get_label!`:

```rust
skuld::get_label!(pub DOCKER, "docker"); // must have a new_label!("docker") somewhere
```

Label names must be valid Rust identifiers (ASCII letters, digits, and underscores; must not start with a digit). Invalid names are rejected at compile time.

At startup, skuld validates that:

- No two `new_label!` calls share the same name (panics with both source locations).
- Every `get_label!` has a matching `new_label!` (panics with the orphan's location).

## Labeling tests

Pass label constants to the `labels` option:

```rust
#[skuld::test(labels = [DOCKER, SLOW])]
fn heavy_test() { /* ... */ }

#[skuld::test(labels = [UNIT])]
fn fast_test() { /* ... */ }
```

## Filtering with `SKULD_LABELS`

Set the `SKULD_LABELS` environment variable to a boolean expression to filter tests at collection time. Tests not matching the filter do not appear at all (not ignored — absent):

```bash
SKULD_LABELS=docker cargo test                         # only tests labeled "docker"
SKULD_LABELS="docker | slow" cargo test                # tests labeled "docker" OR "slow"
SKULD_LABELS="docker & slow" cargo test                # tests labeled "docker" AND "slow"
SKULD_LABELS="!slow" cargo test                        # all tests except "slow"
SKULD_LABELS="(docker | integration) & !slow" cargo test  # combined
```

### Expression syntax

| Operator | Meaning    | Example                           |
| -------- | ---------- | --------------------------------- |
| (none)   | bare label | `docker`                          |
| `!`      | NOT        | `!slow`                           |
| `&`      | AND        | `docker & slow`                   |
| `\|`     | OR         | `docker \| slow`                  |
| `()`     | grouping   | `(docker \| integration) & !slow` |

**Precedence** (highest to lowest): `!` > `&` > `|`

Whitespace between tokens is optional. Quote the value in shell when using `|`.

**Unset** `SKULD_LABELS` → no filtering, all tests run.

## Module-level defaults

Use `default_labels!` to set default labels for all `#[skuld::test]` functions in a module:

```rust
skuld::new_label!(pub SMOKE, "smoke");
skuld::new_label!(pub UNIT, "unit");
skuld::new_label!(pub SLOW, "slow");
skuld::default_labels!(SMOKE, UNIT);

#[skuld::test]                      // inherits [SMOKE, UNIT]
fn test_a() { /* ... */ }

#[skuld::test(labels = [SLOW])]     // gets [SLOW], NOT [SMOKE, UNIT, SLOW]
fn test_b() { /* ... */ }

#[skuld::test(labels = [])]         // gets nothing (explicit opt-out)
fn test_c() { /* ... */ }
```

Explicit `labels = [...]` (including empty) **fully replaces** the module defaults — there is no merging.

Default labels are matched by module path prefix, so a `default_labels!` in a parent module applies to all children unless overridden.
