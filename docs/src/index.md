:::{toctree}
:hidden:
getting-started.md
Writing Tests \<writing-tests.md>
Fixtures \<fixtures.md>
Labels \<labels.md>
Serial Tests \<serial.md>
Dynamic Tests \<dynamic-tests.md>
License \<license.md>
GitHub Repository <https://github.com/AZhukova/skuld>
:::

# skuld

Test harness for Rust with runtime preconditions, fixture injection, and label filtering.

Rust's built-in test framework has no way to mark a test as "ignored with reason" at runtime. Tests that need external tools (valgrind, docker, a built binary) either silently pass when the tool is missing, or hard-fail. `skuld` replaces the built-in harness with one that checks preconditions at runtime, reports unmet ones as `ignored`, and prints a summary showing exactly what's missing.

Get started with the [Getting Started](getting-started.md) guide.

## Features

- **Runtime preconditions** — declare what a test needs; unmet preconditions produce `ignored`, not failures.
- **Fixture injection** — dependency-injected test resources with three lifetime scopes.
- **Label filtering** — tag tests with sentinel `Label` values and filter via the `SKULD_LABELS` environment variable.
- **Serial tests** — tests that touch process-global state run under a mutex.
- **Dynamic tests** — generate tests at runtime from data files or other sources.
- **Unavailability reporting** — a summary after the test run shows exactly what's missing.

## How it works

1. `#[skuld::test]` is a proc macro that preserves the original function and appends an `inventory::submit!` call to register it with the harness.
1. `run_all()` (or `TestRunner::run_tests()`) iterates all registered tests, checks preconditions and fixture requirements at runtime, and builds `libtest-mimic::Trial`s — marking unmet tests as ignored.
1. After `libtest-mimic::run()` completes, the unavailability summary is printed to stderr.

## License

```{image} https://www.apache.org/foundation/press/kit/img/the-apache-way-badge/Indigo-THE_APACHE_WAY_BADGE-rgb.svg
:width: 150px
:align: right
```

Copyright 2026, Anna Zhukova

This project is licensed under the Apache 2.0 license ([full text](license.md)).

## About

```{image} _static/norns.jpg
:width: 122px
:align: right
```

**Skuld** is the youngest of the three Norns in Norse mythology — the weavers of fate who sit beneath the world-tree Yggdrasil. While her sisters Urðr and Verðanði govern the past and the present, Skuld presides over _what shall be_: obligations yet unfulfilled, debts yet unpaid. Her name shares its root with the English word _should_.
