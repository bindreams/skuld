# Nextest Integration

Projects using `skuld` with [nextest](https://nexte.st) can generate nextest
`test-groups` from Skuld's serial-test metadata, so nextest itself enforces
serial constraints instead of tests blocking inside Skuld's coordination.

## Usage

```bash
cargo install cargo-skuld-nextest
cargo skuld-nextest run          # always-fresh: regenerates + runs
# or:
cargo skuld-nextest gen          # write .config/skuld-nextest.toml to commit
cargo skuld-nextest gen --check  # CI: fail if the committed file is stale
cargo nextest run --tool-config-file "skuld:$PWD/.config/skuld-nextest.toml"  # consume the committed file
```

`--tool-config-file` requires an absolute path — nextest rejects relative
ones — so `$PWD` (or an equivalent absolute path) is required here. Unlike
`.config/nextest.toml`, `.config/skuld-nextest.toml` is not auto-discovered;
nextest only reads it when passed explicitly via `--tool-config-file`.

## Keeping `gen` output in sync with a pre-commit hook

To keep a committed `gen` output in sync automatically without installing
anything by hand, reference this repo as a pre-commit hook source in your own
`prek.toml` (vanilla pre-commit works too, but auto-building from a cargo
workspace is a prek-specific improvement — see
[pre-commit/pre-commit#2931](https://github.com/pre-commit/pre-commit/issues/2931)
— so non-prek users may need to `cargo install cargo-skuld-nextest`
themselves first):

```toml
[[repos]]
repo = "https://github.com/bindreams/skuld"
rev = "vX.Y.Z"
hooks = [{ id = "skuld-nextest-gen" }]
```
