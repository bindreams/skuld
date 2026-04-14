# Labels

Labels are sentinel values for tagging and filtering tests.

## Defining labels

Use `#[skuld::label]` on a constant declaration. The label's string name is the identifier lowercased — `DOCKER` becomes the name `"docker"`.

```rust
#[skuld::label] pub const DOCKER: skuld::Label;
#[skuld::label] pub const SLOW: skuld::Label;
#[skuld::label] pub const UNIT: skuld::Label;
```

The const name must be a valid Rust identifier (ASCII letters, digits, and underscores; must not start with a digit) — it's a plain Rust `const`, so the compiler enforces the usual identifier rules on the symbol. The runtime label string is the identifier lowercased.

### Cross-crate sharing

To use a label defined in another crate, just `use` it:

```rust
use other_crate::DOCKER;

#[skuld::test(labels = [DOCKER])]
fn my_test() { /* ... */ }
```

At startup, skuld panics (with both source locations) if two `#[skuld::label]` declarations in the binary produce the same lowercased name.

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

Label names in `SKULD_LABELS` are matched case-insensitively. `SKULD_LABELS=DOCKER`, `SKULD_LABELS=Docker`, and `SKULD_LABELS=docker` are equivalent.

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
#[skuld::label] pub const SMOKE: skuld::Label;
#[skuld::label] pub const UNIT: skuld::Label;
#[skuld::label] pub const SLOW: skuld::Label;
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
