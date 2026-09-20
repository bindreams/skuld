# cargo-skuld

A cargo subcommand for [Skuld](https://skuld.readthedocs.io), a Rust test harness with runtime
preconditions, per-test FD capture and fixture injection.

## `cargo skuld nextest`

Generates nextest `test-groups` from Skuld's serial-test metadata, so **nextest** enforces serial
constraints by scheduling rather than tests blocking inside Skuld's own coordination. A test held
behind a group is never spawned, so it never spends its slow-timeout waiting.

```sh
cargo skuld nextest gen          # write .config/skuld-nextest.toml, to commit
cargo skuld nextest gen --check  # CI: fail if the committed file is stale
cargo skuld nextest run          # regenerate and run
```

The generated file is not auto-discovered — pass it explicitly, with an absolute path:

```sh
cargo nextest run --tool-config-file "skuld:$PWD/.config/skuld-nextest.toml"
```

See the [nextest integration guide](https://skuld.readthedocs.io/en/latest/nextest.html) for
details, including keeping the generated file in sync with a pre-commit hook.
