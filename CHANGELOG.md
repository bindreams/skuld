# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Serial filter expressions.** `serial = <expr>` restricts mutual exclusion
  to tests whose labels match the boolean expression (e.g.
  `serial = DATABASE & !FAST`). Supports `&` (AND), `|` (OR), `!` (NOT), and
  parenthesized grouping. A bare `serial` now means "serial with everything" —
  it blocks ALL other tests, not just other serial tests.

- `LabelFilter` type with `From<Label>` conversion and `&`, `|`, `!` operator
  overloads for building serial filters programmatically.

- `TestRunner::add_serial_with` for adding dynamic tests with a `LabelFilter`.

- SQLite-based cross-process serial coordination (replaces the previous
  file-lock mechanism).

- Startup validation panics with source locations (`file:line:column`) if
  duplicate `new_label!` definitions or orphan `get_label!` references exist.

### Changed

- **`serial` semantics changed.** A bare `serial` now blocks ALL tests (serial
  and non-serial), not just other serial tests. The `TestDef.serial` and
  `FixtureDef.serial` fields changed from `bool` to `&'static str` (empty =
  non-serial, `"*"` = serial with everything, expression = filtered serial).
  `collect_fixture_serial` returns `String` instead of `bool`.

- **`fd-lock` replaced by `rusqlite` (bundled).** Serial coordination now uses
  a SQLite database instead of file locks, enabling filter-aware cross-process
  mutual exclusion.

- **Labels are now sentinel values (`Label` type) instead of plain strings.**
  Use `new_label!` to define label constants, `get_label!` to reference labels
  defined elsewhere. `#[skuld::test(labels = [...])]` now accepts `Label`
  constant paths instead of bare identifiers. `default_labels!` likewise accepts
  `Label` paths. `TestRunner::add`/`add_serial` take `&[Label]` instead of
  `&[&str]`.

- **Label names are now restricted to Rust identifier syntax** (ASCII letters,
  digits, underscore; must not start with a digit). Names with hyphens or
  leading digits are no longer accepted. Invalid names in `new_label!` /
  `get_label!` are caught at compile time; invalid names in `SKULD_LABELS` are
  caught at parse time.

- **Label filtering uses `SKULD_LABELS` env var with boolean expression syntax.**
  Supports `&` (AND), `|` (OR), `!` (NOT), and parenthesized grouping.
  Precedence: `!` > `&` > `|`. Unset = no filtering, all tests run.

- **Per-test output capture now happens at the file-descriptor level instead of
  through a tracing subscriber.** Skuld no longer installs any `tracing`
  dispatcher during test execution, so tests are free to install their own
  `tracing_subscriber::registry().try_init()` (or any other subscriber setup)
  without competing with the harness. Capture is keyed on libtest-mimic's
  `--nocapture` flag:

  - default (`cargo test`): FD-level capture via `dup2` / `SetStdHandle`,
    forced `--test-threads=1`, silent on pass, dumped on failure;
  - `--nocapture` or `cargo nextest run`: capture disabled, default parallelism.

  Fixes bindreams/hole#196, where a test installing its own
  `tracing_subscriber::registry().try_init()` saw its events silently
  swallowed by skuld's thread-local `set_default` subscriber.

- `SKULD_DEBUG=1` environment variable emits diagnostic lines around each
  test's execution (scope entry, capture enable/disable, body enter/exit).

### Removed

- Labels no longer appear in test output names (the libtest-mimic `kind` field
  is no longer set).

- The `--label` CLI flag is removed. Use `SKULD_LABELS` env var instead.

- The comma-separated `SKULD_LABELS` syntax is replaced by boolean expressions.
  `SKULD_LABELS=docker,slow` → `SKULD_LABELS="docker | slow"`.

- `tracing` and `tracing-subscriber` are no longer runtime dependencies of the
  skuld crate. They remain as dev-dependencies for regression tests.

- The private `CaptureBuffer` / `CaptureWriter` types in `src/capture.rs` are
  gone. No public API is affected; the module was never re-exported.

- `probe_executable` and `probe_path` helper functions. Inline the equivalent
  logic directly in your requirement functions (see updated docs).
